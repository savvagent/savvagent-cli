//! Walks the four well-known `settings.json` paths and merges hook lists
//! into a per-event index keyed by `HookEvent`.
//!
//! Precedence order (sequential execution within an event respects this):
//! 1. `<project>/.savvagent/settings.json`
//! 2. `<project>/.claude/settings.json`
//! 3. `~/.savvagent/settings.json`
//! 4. `~/.claude/settings.json`

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::plugin::builtin::user_hooks::config::{HookCommand, MatcherGroup, SettingsFile};
use crate::plugin::builtin::user_hooks::matcher::CompiledMatcher;

/// All `HookEvent` variants we map to a `HookKind` today. Strings
/// referencing names outside this set warn-log at index-build time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    UserPromptSubmit,
    SessionStart,
    Stop,
    SubagentStop,
}

impl HookEvent {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "PreToolUse" => Some(HookEvent::PreToolUse),
            "PostToolUse" => Some(HookEvent::PostToolUse),
            "UserPromptSubmit" => Some(HookEvent::UserPromptSubmit),
            "SessionStart" => Some(HookEvent::SessionStart),
            "Stop" => Some(HookEvent::Stop),
            "SubagentStop" => Some(HookEvent::SubagentStop),
            _ => None,
        }
    }
}

/// A compiled-and-validated matcher group ready for dispatch.
#[derive(Debug, Clone)]
pub struct CompiledGroup {
    pub matcher: CompiledMatcher,
    pub commands: Vec<HookCommand>,
    /// Source path retained for diagnostics; not yet surfaced to the user.
    #[allow(dead_code)]
    pub source: PathBuf,
}

/// The per-event index the runtime uses.
#[derive(Debug, Default, Clone)]
pub struct HooksIndex {
    pub by_event: BTreeMap<HookEvent, Vec<CompiledGroup>>,
    pub warnings: Vec<String>,
}

/// Walk all four directories with precedence and produce the merged
/// index. Missing files are silently ignored; malformed files warn-log.
pub fn walk_all(project_root: &Path, home: &Path) -> HooksIndex {
    let paths: [PathBuf; 4] = [
        project_root.join(".savvagent").join("settings.json"),
        project_root.join(".claude").join("settings.json"),
        home.join(".savvagent").join("settings.json"),
        home.join(".claude").join("settings.json"),
    ];
    let mut index = HooksIndex::default();
    for path in paths {
        load_one(&path, &mut index);
    }
    index
}

fn load_one(path: &Path, index: &mut HooksIndex) {
    if !path.exists() {
        return;
    }
    let contents = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            index.warnings.push(format!("{}: {e}", path.display()));
            return;
        }
    };
    let parsed: SettingsFile = match serde_json::from_str(&contents) {
        Ok(p) => p,
        Err(e) => {
            index
                .warnings
                .push(format!("{}: malformed JSON: {e}", path.display()));
            return;
        }
    };
    for (event_name, groups) in parsed.hooks.0.iter() {
        let Some(event) = HookEvent::parse(event_name) else {
            index.warnings.push(format!(
                "{}: hooks.{event_name} is reserved or unknown; ignoring",
                path.display()
            ));
            continue;
        };
        for group in groups {
            compile_and_push(path, event, group, index);
        }
    }
}

fn compile_and_push(path: &Path, event: HookEvent, group: &MatcherGroup, index: &mut HooksIndex) {
    let matcher = match CompiledMatcher::compile(&group.matcher) {
        Ok(m) => m,
        Err(why) => {
            index.warnings.push(format!("{}: {why}", path.display()));
            return;
        }
    };
    let mut commands = Vec::new();
    for h in &group.hooks {
        if h.type_field != "command" {
            index.warnings.push(format!(
                "{}: unsupported hook type `{}` (only \"command\" in v1)",
                path.display(),
                h.type_field
            ));
            continue;
        }
        commands.push(h.clone());
    }
    if commands.is_empty() {
        return;
    }
    index
        .by_event
        .entry(event)
        .or_default()
        .push(CompiledGroup {
            matcher,
            commands,
            source: path.to_path_buf(),
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn missing_files_returns_empty() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let idx = walk_all(proj.path(), home.path());
        assert!(idx.by_event.is_empty());
        assert!(idx.warnings.is_empty());
    }

    #[test]
    fn precedence_order_within_event() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write(
            &proj.path().join(".savvagent"),
            "settings.json",
            r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "A" } ] } ] } }"#,
        );
        write(
            &proj.path().join(".claude"),
            "settings.json",
            r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "B" } ] } ] } }"#,
        );
        write(
            &home.path().join(".savvagent"),
            "settings.json",
            r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "C" } ] } ] } }"#,
        );
        write(
            &home.path().join(".claude"),
            "settings.json",
            r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "D" } ] } ] } }"#,
        );

        let idx = walk_all(proj.path(), home.path());
        let groups = idx.by_event.get(&HookEvent::Stop).expect("Stop present");
        let cmds: Vec<&str> = groups
            .iter()
            .flat_map(|g| g.commands.iter().map(|c| c.command.as_str()))
            .collect();
        assert_eq!(cmds, vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn malformed_json_warns_other_files_load() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write(&proj.path().join(".savvagent"), "settings.json", "{ broken");
        write(
            &home.path().join(".savvagent"),
            "settings.json",
            r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "ok" } ] } ] } }"#,
        );

        let idx = walk_all(proj.path(), home.path());
        assert_eq!(idx.warnings.len(), 1);
        assert!(idx.warnings[0].contains("malformed JSON"));
        let groups = idx.by_event.get(&HookEvent::Stop).unwrap();
        assert_eq!(groups[0].commands[0].command, "ok");
    }

    #[test]
    fn unknown_event_warns_and_skips() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write(
            &proj.path().join(".savvagent"),
            "settings.json",
            r#"{ "hooks": { "Notification": [ { "hooks": [ { "command": "x" } ] } ], "Stop": [ { "hooks": [ { "command": "y" } ] } ] } }"#,
        );

        let idx = walk_all(proj.path(), home.path());
        assert_eq!(idx.warnings.len(), 1);
        assert!(idx.warnings[0].contains("Notification"));
        let groups = idx.by_event.get(&HookEvent::Stop).unwrap();
        assert_eq!(groups[0].commands[0].command, "y");
    }

    #[test]
    fn subagent_stop_event_is_recognized() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write(
            &proj.path().join(".savvagent"),
            "settings.json",
            r#"{ "hooks": { "SubagentStop": [ { "hooks": [ { "command": "z" } ] } ] } }"#,
        );

        let idx = walk_all(proj.path(), home.path());
        assert!(idx.warnings.is_empty());
        let groups = idx
            .by_event
            .get(&HookEvent::SubagentStop)
            .expect("SubagentStop indexed");
        assert_eq!(groups[0].commands[0].command, "z");
    }

    #[test]
    fn invalid_matcher_pattern_warns_and_skips_group() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write(
            &proj.path().join(".savvagent"),
            "settings.json",
            r#"{ "hooks": { "PreToolUse": [ { "matcher": "[bad", "hooks": [ { "command": "x" } ] }, { "matcher": "*", "hooks": [ { "command": "y" } ] } ] } }"#,
        );

        let idx = walk_all(proj.path(), home.path());
        assert_eq!(idx.warnings.len(), 1);
        assert!(idx.warnings[0].contains("invalid glob"));
        let groups = idx.by_event.get(&HookEvent::PreToolUse).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].commands[0].command, "y");
    }

    #[test]
    fn non_command_type_warns_and_skips_entry() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write(
            &proj.path().join(".savvagent"),
            "settings.json",
            r#"{ "hooks": { "Stop": [ { "hooks": [ { "type": "webhook", "command": "x" }, { "command": "y" } ] } ] } }"#,
        );

        let idx = walk_all(proj.path(), home.path());
        assert_eq!(idx.warnings.len(), 1);
        assert!(idx.warnings[0].contains("unsupported hook type"));
        let groups = idx.by_event.get(&HookEvent::Stop).unwrap();
        assert_eq!(groups[0].commands.len(), 1);
        assert_eq!(groups[0].commands[0].command, "y");
    }
}
