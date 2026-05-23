//! Serde types for the Claude-Code-compatible `settings.json` hooks block.
//!
//! Top-level keys other than `hooks` are ignored (forward-compat). Unknown
//! event names under `hooks` parse cleanly with a warn-log; they're
//! preserved so a future map-event-to-HookKind pass can address them.

use serde::Deserialize;

/// The portion of `settings.json` we care about.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SettingsFile {
    #[serde(default)]
    pub hooks: HooksMap,
}

/// `hooks.{EventName} -> Vec<MatcherGroup>`. Untyped event keys so we
/// can warn-log unknowns at index-build time.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct HooksMap(pub std::collections::BTreeMap<String, Vec<MatcherGroup>>);

/// A `(matcher, hooks)` group within an event's array.
#[derive(Debug, Clone, Deserialize)]
pub struct MatcherGroup {
    /// Glob pattern over the tool name; ignored for non-tool events.
    /// Defaults to `"*"` if absent.
    #[serde(default = "default_matcher")]
    pub matcher: String,
    /// The shell commands to run when this group matches.
    pub hooks: Vec<HookCommand>,
}

fn default_matcher() -> String {
    "*".into()
}

/// One shell command to invoke.
#[derive(Debug, Clone, Deserialize)]
pub struct HookCommand {
    /// Currently the only supported type is `"command"`. Other values
    /// warn-log and skip this entry.
    #[serde(default = "default_type", rename = "type")]
    pub type_field: String,
    /// The command line (passed to `sh -c`).
    pub command: String,
    /// Per-hook timeout in seconds. Default 60.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_type() -> String {
    "command".into()
}

fn default_timeout() -> u64 {
    60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_settings_parses() {
        let s: SettingsFile = serde_json::from_str("{}").unwrap();
        assert!(s.hooks.0.is_empty());
    }

    #[test]
    fn ignores_unknown_top_level_keys() {
        let src = r#"{ "permissions": { "x": 1 }, "hooks": {} }"#;
        let s: SettingsFile = serde_json::from_str(src).unwrap();
        assert!(s.hooks.0.is_empty());
    }

    #[test]
    fn parses_full_pre_tool_use() {
        let src = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "tool-fs:write_file",
                        "hooks": [
                            { "type": "command", "command": "/p/check.sh", "timeout": 30 }
                        ]
                    }
                ]
            }
        }"#;
        let s: SettingsFile = serde_json::from_str(src).unwrap();
        let groups = s.hooks.0.get("PreToolUse").expect("PreToolUse present");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].matcher, "tool-fs:write_file");
        assert_eq!(groups[0].hooks.len(), 1);
        assert_eq!(groups[0].hooks[0].command, "/p/check.sh");
        assert_eq!(groups[0].hooks[0].timeout, 30);
        assert_eq!(groups[0].hooks[0].type_field, "command");
    }

    #[test]
    fn missing_matcher_defaults_to_star() {
        let src = r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "x" } ] } ] } }"#;
        let s: SettingsFile = serde_json::from_str(src).unwrap();
        let groups = s.hooks.0.get("Stop").unwrap();
        assert_eq!(groups[0].matcher, "*");
    }

    #[test]
    fn missing_timeout_defaults_to_60() {
        let src = r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "x" } ] } ] } }"#;
        let s: SettingsFile = serde_json::from_str(src).unwrap();
        assert_eq!(s.hooks.0.get("Stop").unwrap()[0].hooks[0].timeout, 60);
    }

    #[test]
    fn unknown_event_name_parses_into_map() {
        // Unknowns are preserved at parse time; discovery warn-logs them
        // when building the per-HookKind index in Task 4.
        let src = r#"{ "hooks": { "Notification": [] } }"#;
        let s: SettingsFile = serde_json::from_str(src).unwrap();
        assert!(s.hooks.0.contains_key("Notification"));
    }
}
