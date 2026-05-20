//! `internal:tool-bash-summary` — renders summaries for `tool-bash`'s `run`.

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Manifest, Plugin, PluginId, PluginKind, StyledSpan, TextMods, ThemeColor,
    ToolSummarySpec,
};
use tool_bash::{RunInput, RunOutput};

/// Maximum number of characters of the command's first line shown in the
/// summary. The full command can be inspected via the in-progress permission
/// prompt; this is a one-glance signal, not a debug dump.
const COMMAND_MAX_CHARS: usize = 60;

/// Plugin rendering summaries for the `tool-bash` tool.
pub struct ToolBashSummaryPlugin;

impl ToolBashSummaryPlugin {
    /// Construct a new instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ToolBashSummaryPlugin {
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

fn truncate_command(cmd: &str) -> String {
    // First line only — multi-line scripts are summarized by their first line.
    let first = cmd.lines().next().unwrap_or("");
    let count = first.chars().count();
    let is_multiline = cmd.contains('\n');
    if count <= COMMAND_MAX_CHARS && !is_multiline {
        first.to_string()
    } else {
        let mut t: String = first.chars().take(COMMAND_MAX_CHARS).collect();
        t.push('…');
        t
    }
}

#[async_trait]
impl Plugin for ToolBashSummaryPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.tool_summaries = vec![ToolSummarySpec {
            tool_name: "run".into(),
        }];
        Manifest {
            id: PluginId::new("internal:tool-bash-summary").expect("valid built-in id"),
            name: "tool-bash summaries".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Renders conversation-log summaries for the tool-bash run command".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    fn summarize_tool_call(&self, name: &str, args: &serde_json::Value) -> Option<Vec<StyledSpan>> {
        if name != "run" {
            return None;
        }
        let input: RunInput = serde_json::from_value(args.clone()).ok()?;
        Some(vec![
            span("bash $ ", ThemeColor::Fg),
            span(truncate_command(&input.command), ThemeColor::Success),
        ])
    }

    fn summarize_tool_result(&self, name: &str, result_text: &str) -> Option<Vec<StyledSpan>> {
        if name != "run" {
            return None;
        }
        let out: RunOutput = serde_json::from_str(result_text).ok()?;
        let mut spans = vec![
            span("exit ", ThemeColor::Fg),
            span(out.exit_code.to_string(), ThemeColor::Success),
            span(format!(" in {}ms", out.elapsed_ms), ThemeColor::Muted),
        ];
        if out.timed_out {
            spans.push(span(" (timed out)", ThemeColor::Warning));
        }
        if out.stdout_truncated {
            spans.push(span(" (stdout truncated)", ThemeColor::Muted));
        }
        if out.stderr_truncated {
            spans.push(span(" (stderr truncated)", ThemeColor::Muted));
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
    fn manifest_claims_run() {
        let m = ToolBashSummaryPlugin::new().manifest();
        assert_eq!(m.contributions.tool_summaries.len(), 1);
        assert_eq!(m.contributions.tool_summaries[0].tool_name, "run");
    }

    #[test]
    fn run_call_renders_first_line_of_command() {
        let p = ToolBashSummaryPlugin::new();
        let spans = p
            .summarize_tool_call("run", &serde_json::json!({"command": "ls -la"}))
            .unwrap();
        assert_eq!(join(&spans), "bash $ ls -la");
    }

    #[test]
    fn run_call_truncates_long_command() {
        let p = ToolBashSummaryPlugin::new();
        let long = "a".repeat(120);
        let spans = p
            .summarize_tool_call("run", &serde_json::json!({"command": long}))
            .unwrap();
        let expected = format!("bash $ {}…", "a".repeat(COMMAND_MAX_CHARS));
        assert_eq!(join(&spans), expected);
    }

    #[test]
    fn run_call_keeps_only_first_line_of_multiline_command() {
        let p = ToolBashSummaryPlugin::new();
        let spans = p
            .summarize_tool_call(
                "run",
                &serde_json::json!({"command": "set -e\necho hi\nexit 0"}),
            )
            .unwrap();
        assert_eq!(join(&spans), "bash $ set -e…");
    }

    #[test]
    fn run_result_renders_exit_and_elapsed() {
        let p = ToolBashSummaryPlugin::new();
        let result = serde_json::json!({
            "exit_code": 0,
            "stdout": "hi\n",
            "stderr": "",
            "elapsed_ms": 230,
            "stdout_truncated": false,
            "stderr_truncated": false,
            "timed_out": false
        })
        .to_string();
        let spans = p.summarize_tool_result("run", &result).unwrap();
        assert_eq!(join(&spans), "exit 0 in 230ms");
    }

    #[test]
    fn run_result_shows_timeout_and_truncation_flags() {
        let p = ToolBashSummaryPlugin::new();
        let result = serde_json::json!({
            "exit_code": 124,
            "stdout": "...",
            "stderr": "",
            "elapsed_ms": 5000,
            "stdout_truncated": true,
            "stderr_truncated": false,
            "timed_out": true
        })
        .to_string();
        let spans = p.summarize_tool_result("run", &result).unwrap();
        assert_eq!(
            join(&spans),
            "exit 124 in 5000ms (timed out) (stdout truncated)"
        );
    }

    #[test]
    fn returns_none_for_unknown_tool() {
        let p = ToolBashSummaryPlugin::new();
        assert!(
            p.summarize_tool_call("read_file", &serde_json::json!({}))
                .is_none()
        );
    }

    #[test]
    fn returns_none_on_args_parse_failure() {
        let p = ToolBashSummaryPlugin::new();
        // `command` is required.
        assert!(
            p.summarize_tool_call("run", &serde_json::json!({}))
                .is_none()
        );
    }

    #[test]
    fn run_call_handles_trailing_newline() {
        let p = ToolBashSummaryPlugin::new();
        let spans = p
            .summarize_tool_call("run", &serde_json::json!({"command": "ls -la\n"}))
            .unwrap();
        // `\n` is treated as a multi-line marker, so the result IS truncated
        // with `…` to signal the trailing content was elided.
        assert_eq!(join(&spans), "bash $ ls -la…");
    }
}
