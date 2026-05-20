//! `internal:tool-fs-summary` — renders one-line summaries for the four
//! `tool-fs` tool names (`read_file`, `write_file`, `list_dir`, `glob`).
//!
//! Parses arguments and results back into the typed `*Input` / `*Output`
//! structs from the `tool-fs` crate so summaries break at build time if a
//! tool's schema changes upstream.

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Manifest, Plugin, PluginId, PluginKind, StyledSpan, TextMods, ThemeColor,
    ToolSummarySpec, styled::pretty_bytes,
};
use tool_fs::{
    GlobInput, GlobOutput, ListDirInput, ListDirOutput, ReadFileInput, ReadFileOutput,
    WriteFileInput, WriteFileOutput,
};

/// Plugin renders summaries for the `tool-fs` tool names.
pub struct ToolFsSummaryPlugin;

impl ToolFsSummaryPlugin {
    /// Construct a new plugin instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ToolFsSummaryPlugin {
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
impl Plugin for ToolFsSummaryPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.tool_summaries = vec![
            ToolSummarySpec {
                tool_name: "read_file".into(),
            },
            ToolSummarySpec {
                tool_name: "write_file".into(),
            },
            ToolSummarySpec {
                tool_name: "list_dir".into(),
            },
            ToolSummarySpec {
                tool_name: "glob".into(),
            },
        ];
        Manifest {
            id: PluginId::new("internal:tool-fs-summary").expect("valid built-in id"),
            name: "tool-fs summaries".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Renders conversation-log summaries for tool-fs tools".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    fn summarize_tool_call(&self, name: &str, args: &serde_json::Value) -> Option<Vec<StyledSpan>> {
        match name {
            "read_file" => {
                let input: ReadFileInput = serde_json::from_value(args.clone()).ok()?;
                let mut out = vec![
                    span("read_file ", ThemeColor::Fg),
                    span(input.path, ThemeColor::Success),
                ];
                if let Some(max) = input.max_bytes {
                    out.push(span(
                        format!(" (max {})", pretty_bytes(max)),
                        ThemeColor::Muted,
                    ));
                }
                Some(out)
            }
            "write_file" => {
                let input: WriteFileInput = serde_json::from_value(args.clone()).ok()?;
                let line_count = input.content.lines().count();
                let mut out = vec![
                    span("write_file ", ThemeColor::Fg),
                    span(input.path, ThemeColor::Success),
                    span(format!(" ({line_count} lines)"), ThemeColor::Muted),
                ];
                if input.create_dirs {
                    out.push(span(" --create-dirs", ThemeColor::Muted));
                }
                Some(out)
            }
            "list_dir" => {
                let input: ListDirInput = serde_json::from_value(args.clone()).ok()?;
                let mut out = vec![
                    span("list_dir ", ThemeColor::Fg),
                    span(input.path, ThemeColor::Success),
                ];
                if input.recursive {
                    out.push(span(" --recursive", ThemeColor::Muted));
                }
                Some(out)
            }
            "glob" => {
                let input: GlobInput = serde_json::from_value(args.clone()).ok()?;
                let mut out = vec![
                    span("glob ", ThemeColor::Fg),
                    span(input.pattern, ThemeColor::Success),
                ];
                if let Some(root) = input.root {
                    if root != "." {
                        out.push(span(format!(" in {root}"), ThemeColor::Muted));
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }

    fn summarize_tool_result(&self, name: &str, result_text: &str) -> Option<Vec<StyledSpan>> {
        match name {
            "read_file" => {
                let out: ReadFileOutput = serde_json::from_str(result_text).ok()?;
                let line_count = out.content.lines().count();
                Some(vec![
                    span(pretty_bytes(out.bytes), ThemeColor::Success),
                    span(format!(" · {line_count} lines"), ThemeColor::Muted),
                ])
            }
            "write_file" => {
                let out: WriteFileOutput = serde_json::from_str(result_text).ok()?;
                Some(vec![
                    span("wrote ", ThemeColor::Fg),
                    span(pretty_bytes(out.bytes_written), ThemeColor::Success),
                ])
            }
            "list_dir" => {
                let out: ListDirOutput = serde_json::from_str(result_text).ok()?;
                let mut spans = vec![
                    span(out.entries.len().to_string(), ThemeColor::Success),
                    span(" entries", ThemeColor::Fg),
                ];
                if out.truncated {
                    spans.push(span(" (truncated)", ThemeColor::Muted));
                }
                Some(spans)
            }
            "glob" => {
                let out: GlobOutput = serde_json::from_str(result_text).ok()?;
                let mut spans = vec![
                    span(out.matches.len().to_string(), ThemeColor::Success),
                    span(" matches", ThemeColor::Fg),
                ];
                if out.truncated {
                    spans.push(span(" (truncated)", ThemeColor::Muted));
                }
                Some(spans)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn join(spans: &[StyledSpan]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn manifest_claims_four_tool_fs_names() {
        let m = ToolFsSummaryPlugin::new().manifest();
        let names: Vec<&str> = m
            .contributions
            .tool_summaries
            .iter()
            .map(|s| s.tool_name.as_str())
            .collect();
        assert_eq!(names, vec!["read_file", "write_file", "list_dir", "glob"]);
    }

    #[test]
    fn read_file_call_renders_path() {
        let p = ToolFsSummaryPlugin::new();
        let spans = p
            .summarize_tool_call("read_file", &serde_json::json!({"path": "src/main.rs"}))
            .unwrap();
        assert_eq!(join(&spans), "read_file src/main.rs");
    }

    #[test]
    fn read_file_call_renders_path_with_max_bytes() {
        let p = ToolFsSummaryPlugin::new();
        let spans = p
            .summarize_tool_call(
                "read_file",
                &serde_json::json!({"path": "big.bin", "max_bytes": 1048576}),
            )
            .unwrap();
        assert_eq!(join(&spans), "read_file big.bin (max 1.0 MiB)");
    }

    #[test]
    fn read_file_result_renders_size_and_lines() {
        let p = ToolFsSummaryPlugin::new();
        let result = serde_json::json!({
            "path": "src/main.rs",
            "bytes": 1234,
            "content": "fn main() {}\nfn other() {}\n"
        })
        .to_string();
        let spans = p.summarize_tool_result("read_file", &result).unwrap();
        assert_eq!(join(&spans), "1.2 KiB · 2 lines");
    }

    #[test]
    fn write_file_call_includes_line_count_and_create_dirs() {
        let p = ToolFsSummaryPlugin::new();
        let spans = p
            .summarize_tool_call(
                "write_file",
                &serde_json::json!({
                    "path": "out.txt",
                    "content": "a\nb\nc\n",
                    "create_dirs": true
                }),
            )
            .unwrap();
        assert_eq!(join(&spans), "write_file out.txt (3 lines) --create-dirs");
    }

    #[test]
    fn write_file_result_renders_bytes_written() {
        let p = ToolFsSummaryPlugin::new();
        let result = serde_json::json!({"path": "out.txt", "bytes_written": 1024}).to_string();
        let spans = p.summarize_tool_result("write_file", &result).unwrap();
        assert_eq!(join(&spans), "wrote 1.0 KiB");
    }

    #[test]
    fn list_dir_call_and_result() {
        let p = ToolFsSummaryPlugin::new();
        let call = p
            .summarize_tool_call(
                "list_dir",
                &serde_json::json!({"path": "src", "recursive": true}),
            )
            .unwrap();
        assert_eq!(join(&call), "list_dir src --recursive");

        let result = serde_json::json!({
            "path": "src",
            "entries": [
                {"name": "main.rs", "path": "src/main.rs", "is_dir": false, "size_bytes": 100},
                {"name": "lib.rs", "path": "src/lib.rs", "is_dir": false, "size_bytes": 200}
            ],
            "truncated": false
        })
        .to_string();
        let r = p.summarize_tool_result("list_dir", &result).unwrap();
        assert_eq!(join(&r), "2 entries");
    }

    #[test]
    fn glob_call_and_result_with_truncated_flag() {
        let p = ToolFsSummaryPlugin::new();
        let call = p
            .summarize_tool_call("glob", &serde_json::json!({"pattern": "**/*.rs"}))
            .unwrap();
        assert_eq!(join(&call), "glob **/*.rs");

        let result = serde_json::json!({
            "pattern": "**/*.rs",
            "root": ".",
            "matches": ["a.rs", "b.rs", "c.rs"],
            "truncated": true
        })
        .to_string();
        let r = p.summarize_tool_result("glob", &result).unwrap();
        assert_eq!(join(&r), "3 matches (truncated)");
    }

    #[test]
    fn returns_none_for_unknown_tool() {
        let p = ToolFsSummaryPlugin::new();
        assert!(
            p.summarize_tool_call("unknown", &serde_json::json!({}))
                .is_none()
        );
        assert!(p.summarize_tool_result("unknown", "{}").is_none());
    }

    #[test]
    fn returns_none_on_args_parse_failure() {
        let p = ToolFsSummaryPlugin::new();
        // read_file requires a `path: String`. Missing field → parse fails.
        assert!(
            p.summarize_tool_call("read_file", &serde_json::json!({}))
                .is_none()
        );
    }

    #[test]
    fn returns_none_on_result_parse_failure() {
        let p = ToolFsSummaryPlugin::new();
        // read_file Output requires path, bytes, content.
        assert!(p.summarize_tool_result("read_file", "not json").is_none());
    }

    #[test]
    fn key_text_is_accent_colored() {
        let p = ToolFsSummaryPlugin::new();
        let spans = p
            .summarize_tool_call("read_file", &serde_json::json!({"path": "x"}))
            .unwrap();
        // The path is rendered with Success colour to make it stand out.
        let path_span = spans.iter().find(|s| s.text == "x").unwrap();
        assert_eq!(path_span.fg, Some(ThemeColor::Success));
    }
}
