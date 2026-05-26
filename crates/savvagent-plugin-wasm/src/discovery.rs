//! Walk the four well-known directories and produce a list of valid,
//! manifest-parsed plugin candidates. First-wins by plugin id.
//!
//! Path precedence (matches sub-projects A/B/C):
//! 1. `<project>/.savvagent/plugins/<id>/plugin.toml`
//! 2. `<project>/.claude/plugins/<id>/plugin.toml`
//! 3. `~/.savvagent/plugins/<id>/plugin.toml`
//! 4. `~/.claude/plugins/<id>/plugin.toml`
//!
//! Project paths beat user paths; within the same scope, `.savvagent/`
//! beats `.claude/`. Discovery is **best-effort**: malformed manifests
//! produce a warning (returned in [`Discovery::warnings`]) and are skipped
//! — they do not abort the walk for sibling plugins.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::WasmPluginError;
use crate::manifest::PluginManifest;

/// A single plugin candidate that survived manifest parsing + validation.
/// Trust enforcement (Tasks 6+) happens **after** discovery — the presence
/// of an entry here does not mean the plugin is trusted to instantiate.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    /// Parsed `plugin.toml`.
    pub manifest: PluginManifest,
    /// Absolute path to the plugin's directory (where `plugin.toml` and
    /// `plugin.wasm` live).
    pub dir: PathBuf,
    /// Which of the four well-known locations the plugin was found in.
    pub source_scope: SourceScope,
}

/// Where on disk a plugin was discovered, in precedence order
/// (earliest variant = highest priority).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceScope {
    /// `<project>/.savvagent/plugins/`
    ProjectSavvagent,
    /// `<project>/.claude/plugins/`
    ProjectClaude,
    /// `~/.savvagent/plugins/`
    UserSavvagent,
    /// `~/.claude/plugins/`
    UserClaude,
}

/// Result of one full discovery pass — the deduped plugin set plus any
/// warnings the walker accumulated (malformed manifests, unreadable
/// directories, etc.). Warnings are intentionally `String` so callers can
/// route them straight to `tracing::warn!` or the status bar without
/// touching [`WasmPluginError`] internals.
pub struct Discovery {
    /// Plugin candidates, one per id, in unspecified order (callers
    /// typically sort by id before display).
    pub plugins: Vec<DiscoveredPlugin>,
    /// Best-effort, human-readable warnings collected during the walk.
    pub warnings: Vec<String>,
}

/// Discover plugins from the four standard paths.
///
/// `project_root` is the directory returned by the same project-root
/// resolver `SAVVAGENT.md` uses (walk up for `.git/` or `.savvagent/`).
/// `home_dir` is `dirs::home_dir()` in production; injectable for tests.
/// Either argument can be `None` to skip its tier (useful when there's no
/// project context, or for headless smoke tests with no HOME).
pub fn discover(project_root: Option<&Path>, home_dir: Option<&Path>) -> Discovery {
    let mut by_id: HashMap<String, DiscoveredPlugin> = HashMap::new();
    let mut warnings = Vec::new();

    let paths: Vec<(Option<PathBuf>, SourceScope)> = vec![
        (
            project_root.map(|p| p.join(".savvagent/plugins")),
            SourceScope::ProjectSavvagent,
        ),
        (
            project_root.map(|p| p.join(".claude/plugins")),
            SourceScope::ProjectClaude,
        ),
        (
            home_dir.map(|h| h.join(".savvagent/plugins")),
            SourceScope::UserSavvagent,
        ),
        (
            home_dir.map(|h| h.join(".claude/plugins")),
            SourceScope::UserClaude,
        ),
    ];

    for (maybe_dir, scope) in paths {
        let Some(dir) = maybe_dir else { continue };
        if !dir.is_dir() {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                warnings.push(format!("[plugins] read_dir {dir:?}: {e}"));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warnings.push(format!("[plugins] skipped one entry in {dir:?}: {e}"));
                    continue;
                }
            };
            let plugin_dir = entry.path();
            if !plugin_dir.is_dir() {
                continue;
            }
            let manifest_path = plugin_dir.join("plugin.toml");
            if !manifest_path.is_file() {
                continue;
            }
            let id_from_dir = match plugin_dir.file_name().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            match PluginManifest::load(&manifest_path, &id_from_dir) {
                Ok(m) => {
                    let entry = DiscoveredPlugin {
                        manifest: m,
                        dir: plugin_dir,
                        source_scope: scope,
                    };
                    // first-wins: later scopes don't overwrite an entry
                    // that an earlier scope already supplied.
                    by_id.entry(id_from_dir).or_insert(entry);
                }
                Err(WasmPluginError::Manifest(p, why)) => {
                    warnings.push(format!("[plugins] skipped {p:?}: {why}"));
                }
                Err(e) => {
                    warnings.push(format!("[plugins] skipped {plugin_dir:?}: {e}"));
                }
            }
        }
    }

    let plugins = by_id.into_values().collect();
    Discovery { plugins, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_plugin(dir: &Path, id: &str, world: &str) {
        let plugin_dir = dir.join(id);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let mut f = std::fs::File::create(plugin_dir.join("plugin.toml")).unwrap();
        write!(
            f,
            r#"
[plugin]
id = "{id}"
name = "{id}"
version = "0.1.0"
world = "{world}"
savvagent = "^0.18"
"#
        )
        .unwrap();
    }

    #[test]
    fn project_beats_user() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(project.join(".savvagent/plugins")).unwrap();
        std::fs::create_dir_all(home.join(".savvagent/plugins")).unwrap();

        write_plugin(
            &project.join(".savvagent/plugins"),
            "acme.demo",
            "plugin-static",
        );
        write_plugin(
            &home.join(".savvagent/plugins"),
            "acme.demo",
            "plugin-static",
        );

        let d = discover(Some(&project), Some(&home));
        assert_eq!(d.plugins.len(), 1);
        assert_eq!(d.plugins[0].source_scope, SourceScope::ProjectSavvagent);
    }

    #[test]
    fn savvagent_beats_claude_within_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(project.join(".savvagent/plugins")).unwrap();
        std::fs::create_dir_all(project.join(".claude/plugins")).unwrap();

        write_plugin(
            &project.join(".savvagent/plugins"),
            "acme.demo",
            "plugin-static",
        );
        write_plugin(
            &project.join(".claude/plugins"),
            "acme.demo",
            "plugin-static",
        );

        let d = discover(Some(&project), None);
        assert_eq!(d.plugins.len(), 1);
        assert_eq!(d.plugins[0].source_scope, SourceScope::ProjectSavvagent);
    }

    #[test]
    fn invalid_manifest_warns_but_does_not_block_others() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(project.join(".savvagent/plugins")).unwrap();
        write_plugin(
            &project.join(".savvagent/plugins"),
            "good.demo",
            "plugin-static",
        );

        let bad_dir = project.join(".savvagent/plugins/bad.demo");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(bad_dir.join("plugin.toml"), "not toml at all = ::").unwrap();

        let d = discover(Some(&project), None);
        assert_eq!(d.plugins.len(), 1);
        assert_eq!(d.plugins[0].manifest.plugin.id, "good.demo");
        assert!(!d.warnings.is_empty());
    }
}
