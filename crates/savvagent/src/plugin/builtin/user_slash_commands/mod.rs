//! `internal:user-slash-commands` — discovers and dispatches user-defined
//! slash commands from `.savvagent/commands/` and `.claude/commands/`.
//!
//! See `docs/superpowers/specs/2026-05-21-user-slash-commands-design.md`.

mod discovery;
mod frontmatter;
mod name;
mod template;
pub(crate) mod trust;
mod trust_modal;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec,
};
use tokio::sync::RwLock;

use crate::plugin::builtin::user_slash_commands::discovery::{Index, walk_all};
use crate::plugin::builtin::user_slash_commands::trust::TrustLevel;

/// Shared trust-level map type — cloned from `App::trust_levels` at startup
/// so `handle_slash` can read the current trust state without going through App.
pub type TrustMap = Arc<RwLock<BTreeMap<PathBuf, TrustLevel>>>;

/// Built-in plugin that exposes user-authored slash commands.
pub struct UserSlashCommandsPlugin {
    project_root: PathBuf,
    home: PathBuf,
    pub(super) cache: Mutex<Option<Index>>,
    /// Shared with `App::trust_levels`; read under a read-lock in `handle_slash`.
    trust_levels: TrustMap,
}

impl UserSlashCommandsPlugin {
    /// Default constructor used by `register_builtins`: resolves
    /// `project_root` from cwd and `home` from `dirs::home_dir()`.
    /// Accepts the shared trust map cloned from `App::trust_levels`.
    pub fn new(trust_levels: TrustMap) -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            project_root,
            home,
            cache: Mutex::new(None),
            trust_levels,
        }
    }

    /// Override the search roots; used by tests and `/reload-commands` (Task 20).
    /// Accepts an explicit trust map (pass `empty_trust()` in unit tests).
    #[allow(dead_code)]
    pub fn with_roots(project_root: PathBuf, home: PathBuf, trust_levels: TrustMap) -> Self {
        Self {
            project_root,
            home,
            cache: Mutex::new(None),
            trust_levels,
        }
    }

    /// Acquire the cache lock, tolerating a poisoned mutex by accepting the
    /// inner value. The cache is a transient snapshot of disk state; any
    /// inconsistency from a prior panic is repaired by the next
    /// populate-or-reload write.
    fn lock_cache(&self) -> std::sync::MutexGuard<'_, Option<Index>> {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Snapshot the cached Index, populating the cache on first access.
    fn index_snapshot(&self) -> Index {
        let mut g = self.lock_cache();
        if g.is_none() {
            *g = Some(walk_all(&self.project_root, &self.home));
        }
        g.as_ref().unwrap().clone()
    }
}

impl Default for UserSlashCommandsPlugin {
    fn default() -> Self {
        Self::new(Arc::new(RwLock::new(BTreeMap::new())))
    }
}

#[async_trait]
impl Plugin for UserSlashCommandsPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands.push(SlashSpec {
            name: "reload-commands".into(),
            summary: "Rescan user-defined slash command directories".into(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        });
        let idx = self.index_snapshot();
        for d in idx.commands.values() {
            let summary = d
                .frontmatter
                .description
                .clone()
                .unwrap_or_else(|| d.path.display().to_string());
            contributions.slash_commands.push(SlashSpec {
                name: d.name.clone(),
                summary,
                args_hint: d.frontmatter.argument_hint.clone(),
                requires_arg: false,
                suppress_prompt_segments: vec![],
            });
        }
        contributions.screens = vec![ScreenSpec {
            id: "trust.modal".into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 60,
                height_pct: 30,
                title: Some("Trust project commands?".into()),
            },
        }];
        Manifest {
            id: PluginId::new("internal:user-slash-commands").expect("valid built-in id"),
            name: "User slash commands".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "User-defined commands from .savvagent/commands/ and .claude/commands/"
                .into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    fn create_screen(
        &self,
        id: &str,
        args: ScreenArgs,
    ) -> Result<Box<dyn savvagent_plugin::Screen>, PluginError> {
        match id {
            "trust.modal" => Ok(Box::new(trust_modal::TrustModal::from_args(args)?)),
            _ => Err(PluginError::ScreenNotFound(id.into())),
        }
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        if name == "reload-commands" {
            *self.lock_cache() = None;
            // Touching index_snapshot repopulates the cache from disk.
            let _ = self.index_snapshot();
            return Ok(vec![
                Effect::ReindexPlugin {
                    id: PluginId::new("internal:user-slash-commands").expect("valid built-in id"),
                },
                Effect::PushNote {
                    line: savvagent_plugin::StyledLine::plain(
                        "user-slash-commands: reloaded".to_string(),
                    ),
                },
            ]);
        }
        let idx = self.index_snapshot();
        let Some(d) = idx.commands.get(name) else {
            return Ok(vec![]);
        };
        let body = d.body.clone();
        let frontmatter_model = d.frontmatter.model.clone();
        let needs_shell =
            crate::plugin::builtin::user_slash_commands::template::contains_shell_token(&body);
        let project_local = d.origin.is_project();
        // Only project-local commands with shell tokens need a trust check.
        // User-scoped commands (home dir) always proceed — the user owns them.
        //
        // `has_explicit_trust` is true when the user has already made a
        // trust decision for this project root (the map contains an entry).
        // Without an explicit decision the project is implicitly untrusted
        // and the modal must be shown so the user can decide.
        let (trust, has_explicit_trust): (Option<TrustLevel>, bool) =
            if needs_shell && project_local {
                let map = self.trust_levels.read().await;
                match map.get(&self.project_root).copied() {
                    Some(t) => (Some(t), true),
                    None => (Some(TrustLevel::SessionTextOnly), false),
                }
            } else {
                (Some(TrustLevel::Always), true)
            };
        // Open the trust modal only when the project has NO explicit trust
        // decision yet.  If the user already chose "session-text-only",
        // let the call fall through to `expand_all` which will return an
        // error for shell tokens — surfaced as a PushNote.
        if needs_shell && project_local && !has_explicit_trust {
            return Ok(vec![
                Effect::StashPendingSlash {
                    name: name.into(),
                    args,
                },
                Effect::OpenScreen {
                    id: "trust.modal".into(),
                    args: ScreenArgs::TrustModal {
                        project_root: self.project_root.clone(),
                    },
                },
            ]);
        }
        let expanded = match crate::plugin::builtin::user_slash_commands::template::expand_all(
            &body, &args, trust,
        )
        .await
        {
            Ok(e) => e,
            Err(msg) => {
                return Ok(vec![Effect::PushNote {
                    line: savvagent_plugin::StyledLine::plain(format!("[error] {msg}")),
                }]);
            }
        };
        let mut effs: Vec<Effect> = Vec::new();
        for w in expanded.warnings {
            effs.push(Effect::PushNote {
                line: savvagent_plugin::StyledLine::plain(format!("[warn] {w}")),
            });
        }
        if let Some(id) = frontmatter_model {
            effs.push(Effect::SetNextTurnModelOverride { id });
        }
        effs.push(Effect::PromptSend {
            text: expanded.text,
        });
        Ok(effs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Convenience: an empty trust map used by tests that don't pre-populate
    /// trust (i.e. project is untrusted from the start).
    fn empty_trust() -> TrustMap {
        Arc::new(RwLock::new(BTreeMap::new()))
    }

    #[test]
    fn manifest_has_reload_commands() {
        let p = UserSlashCommandsPlugin::default();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:user-slash-commands");
        let names: Vec<_> = m
            .contributions
            .slash_commands
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"reload-commands"));
    }

    #[test]
    fn manifest_registers_trust_modal_screen() {
        let p = UserSlashCommandsPlugin::default();
        let m = p.manifest();
        assert_eq!(
            m.contributions.screens.len(),
            1,
            "expected exactly one screen contribution"
        );
        assert_eq!(m.contributions.screens[0].id, "trust.modal");
    }

    #[test]
    fn manifest_includes_discovered_commands() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("review.md"),
            "---\ndescription: Review the diff\n---\nbody",
        )
        .unwrap();

        let p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            empty_trust(),
        );
        let m = p.manifest();
        let names: Vec<_> = m
            .contributions
            .slash_commands
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"reload-commands"));
        assert!(names.contains(&"review"));
        let review = m
            .contributions
            .slash_commands
            .iter()
            .find(|s| s.name == "review")
            .unwrap();
        assert_eq!(review.summary, "Review the diff");
    }

    #[tokio::test]
    async fn handle_slash_emits_prompt_send() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("hello.md"), "---\ndescription: hi\n---\nHello $1").unwrap();

        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            empty_trust(),
        );
        let effs = p.handle_slash("hello", vec!["world".into()]).await.unwrap();
        assert!(effs.iter().any(|e| matches!(
            e,
            savvagent_plugin::Effect::PromptSend { text } if text.contains("Hello world")
        )));
    }

    #[tokio::test]
    async fn handle_slash_unknown_command_returns_empty() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            empty_trust(),
        );
        let effs = p.handle_slash("does-not-exist", vec![]).await.unwrap();
        assert!(effs.is_empty());
    }

    #[tokio::test]
    async fn handle_slash_with_model_emits_override() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("h.md"), "---\nmodel: claude-sonnet-4-6\n---\nbody").unwrap();
        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            empty_trust(),
        );
        let effs = p.handle_slash("h", vec![]).await.unwrap();
        assert!(effs.iter().any(|e| matches!(
            e,
            savvagent_plugin::Effect::SetNextTurnModelOverride { id } if id == "claude-sonnet-4-6"
        )));
    }

    #[tokio::test]
    async fn reload_emits_reindex_and_picks_up_new_files() {
        use std::fs;
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            empty_trust(),
        );

        // Initially empty: only the static `/reload-commands` entry should appear.
        let m = p.manifest();
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .all(|s| s.name != "added")
        );

        // Add a command on disk AFTER the cache was populated.
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("added.md"), "body").unwrap();

        // Reload.
        let effs = p.handle_slash("reload-commands", vec![]).await.unwrap();
        assert!(
            effs.iter()
                .any(|e| matches!(e, savvagent_plugin::Effect::ReindexPlugin { .. }))
        );

        // Manifest now contains the new command.
        let m = p.manifest();
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == "added")
        );
    }

    #[tokio::test]
    async fn handle_slash_template_warning_surfaces_as_push_note() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("f.md"), "Read @/no/such/file please").unwrap();
        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            empty_trust(),
        );
        let effs = p.handle_slash("f", vec![]).await.unwrap();
        // Expect one PushNote with the warning and one PromptSend with the
        // literal @/no/such/file preserved (per template Task 8 contract).
        let warn_count = effs
            .iter()
            .filter(|e| matches!(e, savvagent_plugin::Effect::PushNote { .. }))
            .count();
        let prompt = effs
            .iter()
            .find_map(|e| match e {
                savvagent_plugin::Effect::PromptSend { text } => Some(text),
                _ => None,
            })
            .unwrap();
        assert_eq!(warn_count, 1);
        assert!(prompt.contains("@/no/such/file"));
    }

    /// Task 21: project-local command with shell token, untrusted project →
    /// must emit StashPendingSlash + OpenScreen("trust.modal"), NOT PromptSend.
    #[tokio::test]
    async fn untrusted_project_with_shell_opens_modal() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("danger.md"), "!echo evil").unwrap();

        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            empty_trust(),
        );
        let effs = p.handle_slash("danger", vec![]).await.unwrap();
        assert!(
            effs.iter()
                .any(|e| matches!(e, Effect::OpenScreen { id, .. } if id == "trust.modal")),
            "expected OpenScreen(trust.modal), got: {effs:?}"
        );
        assert!(
            effs.iter()
                .any(|e| matches!(e, Effect::StashPendingSlash { .. })),
            "expected StashPendingSlash, got: {effs:?}"
        );
        // No PromptSend before trust is granted.
        assert!(
            !effs.iter().any(|e| matches!(e, Effect::PromptSend { .. })),
            "PromptSend must NOT fire when project is untrusted"
        );
    }

    /// Task 21: project-local command with shell token, project is trusted →
    /// must run shell and emit PromptSend, NOT open the modal.
    #[tokio::test]
    async fn trusted_project_with_shell_runs_directly() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("ok.md"), "!echo hello").unwrap();

        // Pre-populate trust as Always for the project root.
        let trust = empty_trust();
        trust
            .write()
            .await
            .insert(proj.path().to_path_buf(), TrustLevel::Always);

        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            trust,
        );
        let effs = p.handle_slash("ok", vec![]).await.unwrap();
        // Shell ran; expect PromptSend containing "hello".
        assert!(
            effs.iter()
                .any(|e| matches!(e, Effect::PromptSend { text } if text.contains("hello"))),
            "expected PromptSend with shell output, got: {effs:?}"
        );
        // No modal.
        assert!(
            !effs.iter().any(|e| matches!(e, Effect::OpenScreen { .. })),
            "OpenScreen must NOT fire when project is trusted"
        );
    }

    /// Task 21: non-shell body → trust check is skipped entirely, command runs.
    #[tokio::test]
    async fn no_shell_body_skips_trust_check() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("safe.md"), "Hello $1").unwrap();

        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            empty_trust(),
        );
        let effs = p.handle_slash("safe", vec!["world".into()]).await.unwrap();
        assert!(
            effs.iter()
                .any(|e| matches!(e, Effect::PromptSend { text } if text.contains("Hello world"))),
            "expected PromptSend, got: {effs:?}"
        );
    }

    /// Task 22: end-to-end — discovery via manifest() then dispatch via handle_slash.
    #[tokio::test]
    async fn end_to_end_review_command() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("review.md"),
            "---\ndescription: Review the current diff\nargument-hint: <range>\n---\nReview $ARGUMENTS\n",
        )
        .unwrap();

        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            empty_trust(),
        );

        // 1. Discovery via manifest()
        let m = p.manifest();
        let entry = m
            .contributions
            .slash_commands
            .iter()
            .find(|s| s.name == "review")
            .expect("review command discovered in manifest");
        assert_eq!(entry.summary, "Review the current diff");
        assert_eq!(entry.args_hint.as_deref(), Some("<range>"));

        // 2. Dispatch via handle_slash
        let effs = p
            .handle_slash("review", vec!["HEAD~3..".into()])
            .await
            .unwrap();
        let prompt = effs
            .iter()
            .find_map(|e| match e {
                savvagent_plugin::Effect::PromptSend { text } => Some(text.as_str()),
                _ => None,
            })
            .expect("dispatch emits PromptSend");
        assert!(prompt.contains("Review HEAD~3.."));
    }

    /// C-1: a poisoned cache mutex must not panic the TUI. Both `manifest()` and
    /// `reload-commands` must survive a poisoned lock.
    #[tokio::test]
    async fn poisoned_cache_does_not_panic() {
        use std::sync::Arc;
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let plugin = Arc::new(UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            empty_trust(),
        ));

        // Poison the cache by panicking inside a thread that holds the lock.
        {
            let poison_plugin = plugin.clone();
            let _ = std::thread::spawn(move || {
                let _guard = poison_plugin.cache.lock().unwrap();
                panic!("intentional poison");
            })
            .join();
        } // poison_plugin clone dropped here

        // The cache mutex is now poisoned. Verify normal operations still work.
        // manifest() must not panic.
        let _m = plugin.manifest();

        // reload-commands also must not panic.
        assert_eq!(Arc::strong_count(&plugin), 1);
        let mut plugin = Arc::try_unwrap(plugin).map_err(|_| "still shared").unwrap();
        let effs = plugin
            .handle_slash("reload-commands", vec![])
            .await
            .unwrap();
        assert!(
            effs.iter()
                .any(|e| matches!(e, savvagent_plugin::Effect::ReindexPlugin { .. }))
        );
    }

    /// C-3: user-scoped commands (home dir) with shell tokens must bypass the
    /// trust gate entirely and emit PromptSend. The trust map is irrelevant
    /// for home-dir commands; the user owns those files unconditionally.
    #[tokio::test]
    async fn user_scoped_command_with_shell_runs_without_trust_modal() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        // Write the command under the home dir, NOT the project dir.
        let dir = home.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("danger.md"), "!echo X").unwrap();

        // Use an empty trust map (project is untrusted).
        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            empty_trust(),
        );
        let effs = p.handle_slash("danger", vec![]).await.unwrap();
        // User-scoped origin bypasses the trust gate; shell should run and
        // PromptSend must be emitted.
        assert!(
            effs.iter().any(|e| matches!(e, Effect::PromptSend { .. })),
            "expected PromptSend for user-scoped command, got: {effs:?}"
        );
        // No trust modal opened.
        assert!(
            !effs.iter().any(|e| matches!(e, Effect::OpenScreen { .. })),
            "OpenScreen must NOT fire for user-scoped command, got: {effs:?}"
        );
        // No stash.
        assert!(
            !effs
                .iter()
                .any(|e| matches!(e, Effect::StashPendingSlash { .. })),
            "StashPendingSlash must NOT fire for user-scoped command, got: {effs:?}"
        );
    }

    /// I-6: `/reload-commands` must reflect removed, changed, and newly-added
    /// files in a single call — covering all three delta shapes.
    #[tokio::test]
    async fn reload_picks_up_removed_and_changed_files() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();

        // Seed: x.md (will be changed), z.md (will be removed).
        fs::write(dir.join("x.md"), "original").unwrap();
        fs::write(dir.join("z.md"), "to-be-deleted").unwrap();

        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            empty_trust(),
        );

        // Populate the cache via manifest().
        let m = p.manifest();
        let names: Vec<_> = m
            .contributions
            .slash_commands
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(
            names.contains(&"x"),
            "precondition: x present before reload"
        );
        assert!(
            names.contains(&"z"),
            "precondition: z present before reload"
        );

        // Mutate disk state: change x, add y, remove z.
        fs::write(dir.join("x.md"), "updated").unwrap();
        fs::write(dir.join("y.md"), "new-command").unwrap();
        fs::remove_file(dir.join("z.md")).unwrap();

        // Reload.
        let effs = p.handle_slash("reload-commands", vec![]).await.unwrap();
        assert!(
            effs.iter()
                .any(|e| matches!(e, savvagent_plugin::Effect::ReindexPlugin { .. })),
            "expected ReindexPlugin in reload effects"
        );

        // Re-snapshot after reload.
        let m2 = p.manifest();
        let cmds: std::collections::HashMap<_, _> = m2
            .contributions
            .slash_commands
            .iter()
            .map(|s| (s.name.as_str(), s))
            .collect();

        // x must still be present (it was changed, not removed).
        assert!(cmds.contains_key("x"), "x must remain after reload");
        // y must now be present (newly added).
        assert!(cmds.contains_key("y"), "y must appear after reload");
        // z must be gone (deleted from disk).
        assert!(!cmds.contains_key("z"), "z must not appear after deletion");

        // Verify x's body is the updated version by dispatching it.
        let effs2 = p.handle_slash("x", vec![]).await.unwrap();
        let prompt = effs2
            .iter()
            .find_map(|e| match e {
                savvagent_plugin::Effect::PromptSend { text } => Some(text.as_str()),
                _ => None,
            })
            .expect("x must dispatch to PromptSend");
        assert!(
            prompt.contains("updated"),
            "x body must be 'updated' after reload, got: {prompt:?}"
        );
    }

    /// I-8: a session-text-only trusted project that tries to run a command
    /// with a shell token must emit a PushNote (error) and NOT emit PromptSend.
    #[tokio::test]
    async fn handle_slash_session_text_only_with_shell_emits_error_note() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("run.md"), "!echo X").unwrap();

        // Pre-populate trust as SessionTextOnly for the project root.
        let trust = empty_trust();
        trust
            .write()
            .await
            .insert(proj.path().to_path_buf(), TrustLevel::SessionTextOnly);

        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            trust,
        );
        let effs = p.handle_slash("run", vec![]).await.unwrap();

        // Must contain a PushNote whose text contains "[error]" or mentions
        // "shell substitution disabled".
        let has_error_note = effs.iter().any(|e| match e {
            savvagent_plugin::Effect::PushNote { line } => {
                let text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
                text.contains("[error]") || text.contains("shell substitution disabled")
            }
            _ => false,
        });
        assert!(
            has_error_note,
            "expected an [error] PushNote for session-text-only + shell, got: {effs:?}"
        );
        // Must NOT emit PromptSend.
        assert!(
            !effs.iter().any(|e| matches!(e, Effect::PromptSend { .. })),
            "PromptSend must NOT fire when trust=SessionTextOnly and body has shell token, got: {effs:?}"
        );
    }

    /// Task 22: trust gate — project-local shell command with empty trust map
    /// must stash + open modal, never emit PromptSend.
    #[tokio::test]
    async fn end_to_end_untrusted_shell_does_not_send_prompt() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        // A command with a line-leading shell substitution token, in a project-local dir.
        fs::write(dir.join("danger.md"), "!echo X").unwrap();

        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            empty_trust(),
        );
        let effs = p.handle_slash("danger", vec![]).await.unwrap();
        // Expect stash + modal, NOT PromptSend (untrusted).
        assert!(
            effs.iter()
                .any(|e| matches!(e, savvagent_plugin::Effect::StashPendingSlash { .. }))
        );
        assert!(effs.iter().any(
            |e| matches!(e, savvagent_plugin::Effect::OpenScreen { id, .. } if id == "trust.modal")
        ));
        assert!(
            !effs
                .iter()
                .any(|e| matches!(e, savvagent_plugin::Effect::PromptSend { .. }))
        );
    }
}
