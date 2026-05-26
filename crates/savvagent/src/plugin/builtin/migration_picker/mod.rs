//! `internal:migration-picker` — first-launch picker for startup_providers.
//!
//! On `HostStarting`:
//!   - Loads `~/.savvagent/config.toml`.
//!   - Calls `crate::migration::decide_migration`.
//!   - `AlreadyDone` → no-op.
//!   - `Direct { startup_providers }` → write config.toml silently and mark
//!     migration done.
//!   - `Picker { detected }` → emit `Effect::OpenScreen` for `migration.picker`.
//!
//! The screen emits a synthetic slash on Enter/Esc; this plugin handles
//! those slashes to write config.toml and mark v1_done.
//!
//! TODO(deferred): integration test with TempDir + HOME_LOCK that runs
//! a full TUI event-loop path for the picker. The screen-level unit tests
//! in `screen.rs` verify the key-event → effect mapping; a round-trip test
//! would verify the slash handlers write the correct config.toml on disk.

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
    PluginKind, ProviderId, Screen, ScreenArgs, ScreenLayout, ScreenSpec, SlashSpec, StyledLine,
};

use crate::config_file::ConfigFile;
use crate::migration::{MigrationOutcome, decide_migration, dismissed_fallback};

pub mod screen;

const PLUGIN_ID: &str = "internal:migration-picker";

/// Core plugin that runs the first-launch migration check on `HostStarting`
/// and handles the `_internal:migration-confirm` / `_internal:migration-dismiss`
/// synthetic slashes emitted by `MigrationPickerScreen`.
pub struct MigrationPickerPlugin {
    /// Detected provider list captured at `HostStarting` time so the slash
    /// handlers (which run later) can fall back to it.
    detected_cache: Vec<ProviderId>,
}

impl MigrationPickerPlugin {
    /// Construct a new plugin instance.
    pub fn new() -> Self {
        Self {
            detected_cache: Vec::new(),
        }
    }

    /// Load config, decide migration outcome, and return the effects to emit
    /// at `HostStarting`.
    fn process_host_starting(&mut self) -> Vec<Effect> {
        let path = ConfigFile::default_path();
        let cfg = ConfigFile::load_or_default(&path);
        match decide_migration(&cfg) {
            MigrationOutcome::AlreadyDone => vec![],
            MigrationOutcome::Direct { startup_providers } => {
                let mut new_cfg = cfg;
                new_cfg.startup.startup_providers = startup_providers
                    .iter()
                    .map(|id| id.as_str().to_string())
                    .collect();
                new_cfg.migration.v1_done = true;
                if let Err(e) = new_cfg.save(&path) {
                    tracing::warn!(error = %e, "migration: failed to write config.toml");
                }
                vec![]
            }
            MigrationOutcome::Picker { detected } => {
                self.detected_cache = detected.clone();
                vec![Effect::OpenScreen {
                    id: "migration.picker".into(),
                    args: ScreenArgs::MigrationPicker { detected },
                }]
            }
        }
    }

    /// Write `startup_providers` + `v1_done = true` to config.toml and return
    /// a `PushNote` effect. On save failure, the note carries the error text
    /// instead of falsely claiming success.
    fn write_and_mark(&self, startup_providers: Vec<ProviderId>) -> Effect {
        self.write_and_mark_at(&ConfigFile::default_path(), startup_providers)
    }

    /// Path-injectable variant used in tests to avoid mutating the real home dir.
    pub(crate) fn write_and_mark_at(
        &self,
        path: &std::path::Path,
        startup_providers: Vec<ProviderId>,
    ) -> Effect {
        let mut cfg = ConfigFile::load_or_default(path);
        let ids_str = startup_providers
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        cfg.startup.startup_providers = startup_providers
            .iter()
            .map(|id| id.as_str().to_string())
            .collect();
        cfg.migration.v1_done = true;
        let line = match cfg.save(path) {
            Ok(()) => StyledLine::plain(
                rust_i18n::t!("migration.saved", ids = ids_str.as_str()).to_string(),
            ),
            Err(e) => {
                tracing::warn!(error = %e, "migration: failed to write config.toml after confirm");
                StyledLine::plain(
                    rust_i18n::t!("migration.save-failed", err = e.to_string().as_str())
                        .to_string(),
                )
            }
        };
        Effect::PushNote { line }
    }
}

impl Default for MigrationPickerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for MigrationPickerPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.hooks = vec![HookKind::HostStarting];
        contributions.screens = vec![ScreenSpec {
            id: "migration.picker".into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 60,
                height_pct: 50,
                title: Some(rust_i18n::t!("migration.picker.title").to_string()),
            },
        }];
        contributions.slash_commands = vec![
            SlashSpec {
                name: "_internal:migration-confirm".into(),
                summary: "(internal) Confirm migration picker selection".into(),
                args_hint: None,
                requires_arg: false,
                suppress_prompt_segments: vec![],
            },
            SlashSpec {
                name: "_internal:migration-dismiss".into(),
                summary: "(internal) Dismiss migration picker".into(),
                args_hint: None,
                requires_arg: false,
                suppress_prompt_segments: vec![],
            },
        ];
        Manifest {
            id: PluginId::new(PLUGIN_ID).expect("valid built-in id"),
            name: "Migration Picker".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.migration-picker-description").to_string(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        if matches!(event, HostEvent::HostStarting) {
            return Ok(self.process_host_starting());
        }
        Ok(vec![])
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        match name {
            "_internal:migration-confirm" => {
                // Slash args are always Vec<String>; convert to ProviderId here
                // at the boundary. Strings that fail ProviderId::new are dropped
                // with a warning — the screen built the list from valid ids so
                // failures here are programming errors.
                let sel_ids: Vec<ProviderId> = args
                    .iter()
                    .filter_map(|s| match ProviderId::new(s.as_str()) {
                        Ok(id) => Some(id),
                        Err(_) => {
                            tracing::warn!(arg = %s, "migration-confirm: dropping invalid provider id");
                            None
                        }
                    })
                    .collect();
                let sel = if sel_ids.is_empty() {
                    dismissed_fallback(&self.detected_cache)
                } else {
                    sel_ids
                };
                Ok(vec![self.write_and_mark(sel)])
            }
            "_internal:migration-dismiss" => {
                let fallback = dismissed_fallback(&self.detected_cache);
                let ids_str = fallback
                    .iter()
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let path = ConfigFile::default_path();
                let mut cfg = ConfigFile::load_or_default(&path);
                cfg.startup.startup_providers =
                    fallback.iter().map(|id| id.as_str().to_string()).collect();
                cfg.migration.v1_done = true;
                let line = match cfg.save(&path) {
                    Ok(()) => StyledLine::plain(
                        rust_i18n::t!("migration.fallback", ids = ids_str.as_str()).to_string(),
                    ),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "migration: failed to write config.toml after dismiss"
                        );
                        StyledLine::plain(
                            rust_i18n::t!("migration.save-failed", err = e.to_string().as_str())
                                .to_string(),
                        )
                    }
                };
                Ok(vec![Effect::PushNote { line }])
            }
            _ => Ok(vec![]),
        }
    }

    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        match (id, args) {
            ("migration.picker", ScreenArgs::MigrationPicker { detected }) => {
                Ok(Box::new(screen::MigrationPickerScreen::new(detected)))
            }
            (other, _) => Err(PluginError::ScreenNotFound(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::HostEvent;

    #[tokio::test]
    async fn already_done_emits_no_effects() {
        // When config already has v1_done=true, process_host_starting returns empty.
        // We can't easily test the full path here without a TempDir + HOME env
        // manipulation, but we can verify handle_slash routing is correct.
        let mut p = MigrationPickerPlugin::new();
        // Confirm with no args falls back to empty (empty detected cache).
        let effs = p
            .handle_slash("_internal:migration-confirm", vec![])
            .await
            .unwrap();
        assert_eq!(effs.len(), 1);
        assert!(matches!(effs[0], Effect::PushNote { .. }));
    }

    // HOME_LOCK is std::sync::Mutex (shared across the savvagent crate)
    // and must span the await. Holding it here serialises with every
    // other HOME-mutating test in this crate; without it the
    // routing/models-pref tests (which redirect $HOME to a tempdir)
    // raced these tests' writes to `~/.savvagent/config.toml`.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn confirm_with_args_uses_provided_ids() {
        use crate::test_helpers::HOME_LOCK;
        let _lock = HOME_LOCK.lock().unwrap();
        let mut p = MigrationPickerPlugin::new();
        // Pre-populate cache so dismissed_fallback has material to work with.
        p.detected_cache = vec![
            ProviderId::new("anthropic").unwrap(),
            ProviderId::new("gemini").unwrap(),
        ];
        let effs = p
            .handle_slash(
                "_internal:migration-confirm",
                vec!["anthropic".into(), "gemini".into()],
            )
            .await
            .unwrap();
        assert_eq!(effs.len(), 1);
        // Should be a PushNote with "migration.saved" text.
        match &effs[0] {
            Effect::PushNote { line } => {
                let text: String = line.spans.iter().map(|s| s.text.clone()).collect();
                assert!(text.contains("anthropic"), "expected ids in note: {text}");
            }
            _ => panic!("expected PushNote, got {:?}", effs[0]),
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn dismiss_uses_fallback() {
        use crate::test_helpers::HOME_LOCK;
        let _lock = HOME_LOCK.lock().unwrap();
        let mut p = MigrationPickerPlugin::new();
        p.detected_cache = vec![
            ProviderId::new("gemini").unwrap(),
            ProviderId::new("anthropic").unwrap(),
        ];
        let effs = p
            .handle_slash("_internal:migration-dismiss", vec![])
            .await
            .unwrap();
        assert_eq!(effs.len(), 1);
        match &effs[0] {
            Effect::PushNote { line } => {
                let text: String = line.spans.iter().map(|s| s.text.clone()).collect();
                // dismissed_fallback prefers anthropic.
                assert!(
                    text.contains("anthropic"),
                    "expected fallback in note: {text}"
                );
            }
            _ => panic!("expected PushNote, got {:?}", effs[0]),
        }
    }

    #[tokio::test]
    async fn on_event_ignores_non_host_starting() {
        let mut p = MigrationPickerPlugin::new();
        let effs = p
            .on_event(HostEvent::ProviderRegistered {
                id: savvagent_plugin::ProviderId::new("anthropic").unwrap(),
                display_name: "Anthropic".into(),
            })
            .await
            .unwrap();
        assert!(effs.is_empty());
    }

    #[test]
    fn manifest_is_core_and_subscribes_to_host_starting() {
        let p = MigrationPickerPlugin::new();
        let m = p.manifest();
        assert_eq!(m.kind, PluginKind::Core);
        assert!(m.contributions.hooks.contains(&HookKind::HostStarting));
        assert_eq!(m.id.as_str(), PLUGIN_ID);
    }

    #[test]
    fn create_screen_returns_migration_picker() {
        let p = MigrationPickerPlugin::new();
        let screen = p
            .create_screen(
                "migration.picker",
                ScreenArgs::MigrationPicker {
                    detected: vec![ProviderId::new("anthropic").unwrap()],
                },
            )
            .unwrap();
        assert_eq!(screen.id(), "migration.picker");
    }

    #[test]
    fn create_screen_unknown_id_returns_not_found() {
        let p = MigrationPickerPlugin::new();
        let result = p.create_screen("unknown", ScreenArgs::None);
        assert!(
            matches!(result, Err(PluginError::ScreenNotFound(ref id)) if id == "unknown"),
            "expected ScreenNotFound(\"unknown\"), got an Ok"
        );
    }

    /// Verify that `write_and_mark_at` surfaces an error note when the target
    /// path cannot be written, instead of falsely claiming the save succeeded.
    ///
    /// The test uses a read-only directory as the parent so `cfg.save` will
    /// fail with a permission error (UNIX only).
    #[test]
    #[cfg(unix)]
    fn write_and_mark_save_failure_returns_error_note() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        // Make the directory unwriteable so creating config.toml inside fails.
        let ro_dir = tmp.path().join("ro");
        fs::create_dir(&ro_dir).unwrap();
        fs::set_permissions(&ro_dir, fs::Permissions::from_mode(0o555)).unwrap();

        let cfg_path = ro_dir.join("config.toml");
        let p = MigrationPickerPlugin::new();
        let eff = p.write_and_mark_at(
            &cfg_path,
            vec![
                ProviderId::new("anthropic").unwrap(),
                ProviderId::new("gemini").unwrap(),
            ],
        );

        // Restore permissions so TempDir can clean up.
        fs::set_permissions(&ro_dir, fs::Permissions::from_mode(0o755)).unwrap();

        match eff {
            Effect::PushNote { line } => {
                let text: String = line.spans.iter().map(|s| s.text.clone()).collect();
                assert!(
                    text.contains("Failed to save"),
                    "expected save-failed message, got: {text}"
                );
                // Must NOT contain the success phrase.
                assert!(
                    !text.contains("Saved startup_providers"),
                    "note falsely claims success: {text}"
                );
            }
            other => panic!("expected PushNote, got {other:?}"),
        }
    }
}
