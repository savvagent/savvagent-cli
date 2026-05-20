//! `internal:tool-grep-summary` — renders summaries for `tool-grep`'s `search`.

use std::collections::HashSet;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Manifest, Plugin, PluginId, PluginKind, StyledSpan, TextMods, ThemeColor,
    ToolSummarySpec,
};
use tool_grep::{SearchInput, SearchOutput};

/// Plugin rendering summaries for the `tool-grep` `search` tool.
pub struct ToolGrepSummaryPlugin;

impl ToolGrepSummaryPlugin {
    /// Construct a new instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ToolGrepSummaryPlugin {
    fn default() -> Self {
        Self::new()
    }
}

fn span(text: impl Into<String>, fg: ThemeColor) -> StyledSpan {
    StyledSpan {
        text: text.into(),
        fg: Some(fg),
        bg: None,
        modifiers: TextMods::default(),
    }
}

#[async_trait]
impl Plugin for ToolGrepSummaryPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.tool_summaries = vec![ToolSummarySpec {
            tool_name: "search".into(),
        }];
        Manifest {
            id: PluginId::new("internal:tool-grep-summary").expect("valid built-in id"),
            name: "tool-grep summaries".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Renders conversation-log summaries for the tool-grep search command"
                .into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    fn summarize_tool_call(&self, name: &str, args: &serde_json::Value) -> Option<Vec<StyledSpan>> {
        if name != "search" {
            return None;
        }
        let input: SearchInput = serde_json::from_value(args.clone()).ok()?;
        let mut spans = vec![
            span("grep '", ThemeColor::Fg),
            span(input.pattern, ThemeColor::Success),
            span("'", ThemeColor::Fg),
        ];
        if let Some(path) = input.path {
            spans.push(span(format!(" in {path}"), ThemeColor::Muted));
        }
        if input.case_insensitive {
            spans.push(span(" -i", ThemeColor::Muted));
        }
        if input.multiline {
            spans.push(span(" --multiline", ThemeColor::Muted));
        }
        Some(spans)
    }

    fn summarize_tool_result(&self, name: &str, result_text: &str) -> Option<Vec<StyledSpan>> {
        if name != "search" {
            return None;
        }
        let out: SearchOutput = serde_json::from_str(result_text).ok()?;
        let unique_files: HashSet<&str> = out.matches.iter().map(|m| m.file.as_str()).collect();
        let mut spans = vec![
            span(out.matches.len().to_string(), ThemeColor::Success),
            span(" matches in ", ThemeColor::Fg),
            span(unique_files.len().to_string(), ThemeColor::Success),
            span(" files", ThemeColor::Fg),
        ];
        if out.truncated {
            spans.push(span(" (truncated)", ThemeColor::Muted));
        }
        Some(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn join(spans: &[StyledSpan]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn manifest_claims_search() {
        let m = ToolGrepSummaryPlugin::new().manifest();
        assert_eq!(m.contributions.tool_summaries.len(), 1);
        assert_eq!(m.contributions.tool_summaries[0].tool_name, "search");
    }

    #[test]
    fn search_call_renders_pattern() {
        let p = ToolGrepSummaryPlugin::new();
        let spans = p
            .summarize_tool_call("search", &serde_json::json!({"pattern": "TODO"}))
            .unwrap();
        assert_eq!(join(&spans), "grep 'TODO'");
    }

    #[test]
    fn search_call_renders_pattern_and_path_and_flags() {
        let p = ToolGrepSummaryPlugin::new();
        let spans = p
            .summarize_tool_call(
                "search",
                &serde_json::json!({
                    "pattern": "fn ",
                    "path": "src",
                    "case_insensitive": true,
                    "multiline": false
                }),
            )
            .unwrap();
        assert_eq!(join(&spans), "grep 'fn ' in src -i");
    }

    #[test]
    fn search_result_counts_matches_and_unique_files() {
        let p = ToolGrepSummaryPlugin::new();
        let result = serde_json::json!({
            "pattern": "fn ",
            "root": ".",
            "matches": [
                {"file": "a.rs", "line": 1, "column": 1, "text": "fn a() {}"},
                {"file": "a.rs", "line": 2, "column": 1, "text": "fn b() {}"},
                {"file": "b.rs", "line": 3, "column": 1, "text": "fn c() {}"}
            ],
            "truncated": false
        })
        .to_string();
        let spans = p.summarize_tool_result("search", &result).unwrap();
        assert_eq!(join(&spans), "3 matches in 2 files");
    }

    #[test]
    fn search_result_shows_truncated_flag() {
        let p = ToolGrepSummaryPlugin::new();
        let result = serde_json::json!({
            "pattern": "fn ",
            "root": ".",
            "matches": [
                {"file": "a.rs", "line": 1, "column": 1, "text": "fn a() {}"}
            ],
            "truncated": true
        })
        .to_string();
        let spans = p.summarize_tool_result("search", &result).unwrap();
        assert_eq!(join(&spans), "1 matches in 1 files (truncated)");
    }

    #[test]
    fn returns_none_for_unknown_tool() {
        let p = ToolGrepSummaryPlugin::new();
        assert!(
            p.summarize_tool_call("read_file", &serde_json::json!({}))
                .is_none()
        );
    }

    #[test]
    fn returns_none_on_args_parse_failure() {
        let p = ToolGrepSummaryPlugin::new();
        // `pattern` is required.
        assert!(
            p.summarize_tool_call("search", &serde_json::json!({}))
                .is_none()
        );
    }
}
