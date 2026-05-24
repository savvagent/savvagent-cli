//! `internal:tool-task-summary` — renders one-line summaries for the
//! `task` tool (the in-process subagent dispatch tool from
//! `internal:user-agents`).
//!
//! v1 ships a minimum-viable summary; collapsible expansion of the nested
//! subagent transcript is intentionally deferred (see Task 22 plan).

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Manifest, Plugin, PluginId, PluginKind, StyledSpan, TextMods, ThemeColor,
    ToolSummarySpec,
};
use serde::Deserialize;

/// Maximum number of characters of the task `description` rendered in the
/// call summary. Mirrors the 3-5 word label hinted by the tool's JSON
/// schema, but allows a bit more headroom for descriptive labels.
const DESCRIPTION_MAX_CHARS: usize = 60;

/// Maximum number of characters of the tool result text rendered in the
/// result summary. Tasks can produce long transcripts; this is a
/// one-glance signal, not a debug dump.
const RESULT_MAX_CHARS: usize = 80;

/// Plugin rendering summaries for the `task` tool name.
pub struct ToolTaskSummaryPlugin;

impl ToolTaskSummaryPlugin {
    /// Construct a new plugin instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ToolTaskSummaryPlugin {
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

/// Subset of `TaskInput` (from `internal:user-agents`) needed for the
/// one-line summary. We only deserialize what we render, with `#[serde(default)]`
/// so a missing field falls back to a placeholder rather than dropping the
/// whole summary.
#[derive(Debug, Deserialize)]
struct TaskCallInput {
    #[serde(default)]
    description: String,
    #[serde(default)]
    subagent_type: String,
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[async_trait]
impl Plugin for ToolTaskSummaryPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.tool_summaries = vec![ToolSummarySpec {
            tool_name: "task".into(),
        }];
        Manifest {
            id: PluginId::new("internal:tool-task-summary").expect("valid built-in id"),
            name: "tool-task summaries".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Renders conversation-log summaries for the task (subagent) tool".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    fn summarize_tool_call(&self, name: &str, args: &serde_json::Value) -> Option<Vec<StyledSpan>> {
        if name != "task" {
            return None;
        }
        let input: TaskCallInput = serde_json::from_value(args.clone()).ok()?;
        let agent = if input.subagent_type.is_empty() {
            "<unknown>".to_string()
        } else {
            input.subagent_type
        };
        let desc = if input.description.is_empty() {
            "<no description>".to_string()
        } else {
            truncate(&input.description, DESCRIPTION_MAX_CHARS)
        };
        Some(vec![
            span("task ", ThemeColor::Fg),
            span(agent, ThemeColor::Accent),
            span(" · ", ThemeColor::Muted),
            span(format!("\"{desc}\""), ThemeColor::Success),
        ])
    }

    fn summarize_tool_result(&self, name: &str, result_text: &str) -> Option<Vec<StyledSpan>> {
        if name != "task" {
            return None;
        }
        // The task tool returns plain text (the subagent's final
        // assistant message), not a JSON `*Output` struct, so we don't
        // try to deserialize. Empty/whitespace-only payloads fall through
        // to the host's default rendering.
        let trimmed = result_text.trim();
        if trimmed.is_empty() {
            return None;
        }
        // Render only the first line; long multi-line agent outputs are
        // condensed to "first line … (N more lines)".
        let line_count = trimmed.lines().count();
        let first_line = trimmed.lines().next().unwrap_or("");
        let snippet = truncate(first_line, RESULT_MAX_CHARS);
        let mut spans = vec![span(snippet, ThemeColor::Muted)];
        if line_count > 1 {
            spans.push(span(
                format!(
                    " (+{} more line{})",
                    line_count - 1,
                    if line_count > 2 { "s" } else { "" }
                ),
                ThemeColor::Muted,
            ));
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
    fn manifest_contributes_task_summary() {
        let m = ToolTaskSummaryPlugin::new().manifest();
        let names: Vec<&str> = m
            .contributions
            .tool_summaries
            .iter()
            .map(|s| s.tool_name.as_str())
            .collect();
        assert_eq!(names, vec!["task"]);
        assert_eq!(m.id.as_str(), "internal:tool-task-summary");
    }

    #[test]
    fn summarize_tool_call_renders_agent_and_description() {
        let p = ToolTaskSummaryPlugin::new();
        let args = serde_json::json!({
            "description": "review the auth diff",
            "prompt": "...",
            "subagent_type": "code-reviewer"
        });
        let spans = p.summarize_tool_call("task", &args).expect("summary");
        let combined = join(&spans);
        assert!(combined.contains("task"));
        assert!(combined.contains("code-reviewer"));
        assert!(combined.contains("review the auth diff"));
    }

    #[test]
    fn summarize_tool_call_other_tool_returns_none() {
        let p = ToolTaskSummaryPlugin::new();
        let args = serde_json::json!({});
        assert!(p.summarize_tool_call("read_file", &args).is_none());
    }

    #[test]
    fn summarize_tool_call_missing_fields_uses_placeholders() {
        let p = ToolTaskSummaryPlugin::new();
        // Both `description` and `subagent_type` default to "" via #[serde(default)],
        // so the call should still render rather than returning None.
        let spans = p
            .summarize_tool_call("task", &serde_json::json!({}))
            .expect("placeholder summary");
        let combined = join(&spans);
        assert!(combined.contains("<unknown>"));
        assert!(combined.contains("<no description>"));
    }

    #[test]
    fn summarize_tool_call_truncates_long_description() {
        let p = ToolTaskSummaryPlugin::new();
        let long_desc = "a".repeat(200);
        let spans = p
            .summarize_tool_call(
                "task",
                &serde_json::json!({
                    "description": long_desc,
                    "subagent_type": "reviewer"
                }),
            )
            .expect("summary");
        let combined = join(&spans);
        // The combined string should be much shorter than the raw description.
        assert!(combined.chars().count() < 200);
        assert!(combined.contains('…'));
    }

    #[test]
    fn summarize_tool_result_truncates_long_text() {
        let p = ToolTaskSummaryPlugin::new();
        let long = "a".repeat(200);
        let spans = p.summarize_tool_result("task", &long).expect("summary");
        let combined = join(&spans);
        assert!(combined.chars().count() < 200);
        assert!(combined.ends_with('…'));
    }

    #[test]
    fn summarize_tool_result_short_text_is_passed_through() {
        let p = ToolTaskSummaryPlugin::new();
        let spans = p
            .summarize_tool_result("task", "all good")
            .expect("summary");
        assert_eq!(join(&spans), "all good");
    }

    #[test]
    fn summarize_tool_result_other_tool_returns_none() {
        let p = ToolTaskSummaryPlugin::new();
        assert!(p.summarize_tool_result("read_file", "anything").is_none());
    }

    #[test]
    fn summarize_tool_result_empty_returns_none() {
        let p = ToolTaskSummaryPlugin::new();
        assert!(p.summarize_tool_result("task", "   \n  ").is_none());
    }

    #[test]
    fn summarize_tool_result_multiline_shows_line_count() {
        let p = ToolTaskSummaryPlugin::new();
        let spans = p
            .summarize_tool_result("task", "first line\nsecond\nthird")
            .expect("summary");
        let combined = join(&spans);
        assert!(combined.starts_with("first line"));
        assert!(combined.contains("+2 more lines"));
    }

    #[test]
    fn agent_name_uses_accent_colour() {
        let p = ToolTaskSummaryPlugin::new();
        let spans = p
            .summarize_tool_call(
                "task",
                &serde_json::json!({
                    "description": "x",
                    "subagent_type": "code-reviewer"
                }),
            )
            .expect("summary");
        let agent_span = spans
            .iter()
            .find(|s| s.text == "code-reviewer")
            .expect("agent span");
        assert_eq!(agent_span.fg, Some(ThemeColor::Accent));
    }
}
