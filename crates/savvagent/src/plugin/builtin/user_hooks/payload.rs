//! Builds the stdin JSON payload each hook receives. Shape matches
//! Claude Code's hook contract.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::plugin::builtin::user_hooks::discovery::HookEvent;

/// Per-call context shared across all payloads.
#[derive(Debug, Clone)]
pub struct HookContext<'a> {
    pub session_id: &'a str,
    pub transcript_path: &'a Path,
    pub cwd: &'a Path,
}

/// Build a `PreToolUse` stdin payload.
///
/// When `subagent` is `Some(name)`, the payload includes a `"subagent"`
/// field so user hooks can distinguish subagent tool calls from
/// parent-turn calls. Parent-turn calls pass `None`.
pub fn pre_tool_use(
    ctx: &HookContext<'_>,
    tool_name: &str,
    tool_input: &Value,
    subagent: Option<&str>,
) -> Value {
    let mut payload = base(ctx, HookEvent::PreToolUse).extend(&[
        ("tool_name", json!(tool_name)),
        ("tool_input", tool_input.clone()),
    ]);
    if let Some(name) = subagent {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("subagent".into(), json!(name));
        }
    }
    payload
}

/// Build a `PostToolUse` stdin payload.
///
/// See [`pre_tool_use`] for the `subagent` parameter contract.
pub fn post_tool_use(
    ctx: &HookContext<'_>,
    tool_name: &str,
    tool_input: &Value,
    tool_response: &Value,
    subagent: Option<&str>,
) -> Value {
    let mut payload = base(ctx, HookEvent::PostToolUse).extend(&[
        ("tool_name", json!(tool_name)),
        ("tool_input", tool_input.clone()),
        ("tool_response", tool_response.clone()),
    ]);
    if let Some(name) = subagent {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("subagent".into(), json!(name));
        }
    }
    payload
}

/// Build a `UserPromptSubmit` stdin payload.
pub fn user_prompt_submit(ctx: &HookContext<'_>, prompt: &str) -> Value {
    base(ctx, HookEvent::UserPromptSubmit).extend(&[("prompt", json!(prompt))])
}

/// Build a `SessionStart` stdin payload.
pub fn session_start(ctx: &HookContext<'_>, source: &str) -> Value {
    base(ctx, HookEvent::SessionStart).extend(&[("source", json!(source))])
}

/// Build a `Stop` stdin payload.
pub fn stop(ctx: &HookContext<'_>, stop_hook_active: bool) -> Value {
    base(ctx, HookEvent::Stop).extend(&[("stop_hook_active", json!(stop_hook_active))])
}

/// Build a `SubagentStop` stdin payload.
///
/// Emitted when a subagent (Task tool invocation) finishes its turn
/// cleanly with `end_turn`. The `agent_name` identifies which
/// subagent definition produced the run; `stop_hook_active` mirrors
/// the parent-turn `Stop` flag so hooks can detect loop-back chains.
pub fn subagent_stop(ctx: &HookContext<'_>, agent_name: &str, stop_hook_active: bool) -> Value {
    base(ctx, HookEvent::SubagentStop).extend(&[
        ("subagent", json!(agent_name)),
        ("stop_hook_active", json!(stop_hook_active)),
    ])
}

fn event_name(event: HookEvent) -> &'static str {
    match event {
        HookEvent::PreToolUse => "PreToolUse",
        HookEvent::PostToolUse => "PostToolUse",
        HookEvent::UserPromptSubmit => "UserPromptSubmit",
        HookEvent::SessionStart => "SessionStart",
        HookEvent::Stop => "Stop",
        HookEvent::SubagentStop => "SubagentStop",
    }
}

struct Builder(Map<String, Value>);

fn base(ctx: &HookContext<'_>, event: HookEvent) -> Builder {
    let mut m = Map::new();
    m.insert("session_id".into(), json!(ctx.session_id));
    m.insert(
        "transcript_path".into(),
        json!(ctx.transcript_path.display().to_string()),
    );
    m.insert("cwd".into(), json!(ctx.cwd.display().to_string()));
    m.insert("hook_event_name".into(), json!(event_name(event)));
    Builder(m)
}

impl Builder {
    fn extend(mut self, pairs: &[(&str, Value)]) -> Value {
        for (k, v) in pairs {
            self.0.insert((*k).to_string(), v.clone());
        }
        Value::Object(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx() -> (PathBuf, PathBuf, HookContext<'static>) {
        let transcript: &'static Path = Box::leak(PathBuf::from("/t/123.json").into_boxed_path());
        let cwd: &'static Path = Box::leak(PathBuf::from("/cwd").into_boxed_path());
        (
            transcript.to_path_buf(),
            cwd.to_path_buf(),
            HookContext {
                session_id: "sid",
                transcript_path: transcript,
                cwd,
            },
        )
    }

    #[test]
    fn pre_tool_use_payload_has_all_fields() {
        let (_, _, c) = ctx();
        let v = pre_tool_use(&c, "tool-fs:write_file", &json!({ "path": "/etc" }), None);
        assert_eq!(v["session_id"], "sid");
        assert_eq!(v["transcript_path"], "/t/123.json");
        assert_eq!(v["cwd"], "/cwd");
        assert_eq!(v["hook_event_name"], "PreToolUse");
        assert_eq!(v["tool_name"], "tool-fs:write_file");
        assert_eq!(v["tool_input"]["path"], "/etc");
    }

    #[test]
    fn post_tool_use_includes_response() {
        let (_, _, c) = ctx();
        let v = post_tool_use(&c, "run", &json!({}), &json!({ "ok": true }), None);
        assert_eq!(v["hook_event_name"], "PostToolUse");
        assert_eq!(v["tool_response"]["ok"], true);
    }

    #[test]
    fn pre_tool_use_payload_omits_subagent_for_parent_turn() {
        use std::path::Path;
        let ctx = HookContext {
            session_id: "s",
            transcript_path: Path::new("/tmp/t.json"),
            cwd: Path::new("/proj"),
        };
        let payload = pre_tool_use(&ctx, "tool-fs:read_file", &json!({"path": "x"}), None);
        assert!(payload.get("subagent").is_none());
    }

    #[test]
    fn pre_tool_use_payload_includes_subagent_when_set() {
        use std::path::Path;
        let ctx = HookContext {
            session_id: "s",
            transcript_path: Path::new("/tmp/t.json"),
            cwd: Path::new("/proj"),
        };
        let payload = pre_tool_use(
            &ctx,
            "tool-fs:read_file",
            &json!({"path": "x"}),
            Some("code-reviewer"),
        );
        assert_eq!(payload.get("subagent"), Some(&json!("code-reviewer")));
    }

    #[test]
    fn post_tool_use_payload_includes_subagent_when_set() {
        use std::path::Path;
        let ctx = HookContext {
            session_id: "s",
            transcript_path: Path::new("/tmp/t.json"),
            cwd: Path::new("/proj"),
        };
        let payload = post_tool_use(
            &ctx,
            "tool-fs:read_file",
            &json!({"path": "x"}),
            &json!({"content": "data"}),
            Some("explorer"),
        );
        assert_eq!(payload.get("subagent"), Some(&json!("explorer")));
    }

    #[test]
    fn subagent_stop_payload_shape() {
        use std::path::Path;
        let ctx = HookContext {
            session_id: "s",
            transcript_path: Path::new("/tmp/t.json"),
            cwd: Path::new("/proj"),
        };
        let payload = subagent_stop(&ctx, "code-reviewer", false);
        assert_eq!(payload.get("hook_event_name"), Some(&json!("SubagentStop")));
        assert_eq!(payload.get("subagent"), Some(&json!("code-reviewer")));
        assert_eq!(payload.get("stop_hook_active"), Some(&json!(false)));
    }

    #[test]
    fn user_prompt_submit_includes_prompt() {
        let (_, _, c) = ctx();
        let v = user_prompt_submit(&c, "hello");
        assert_eq!(v["hook_event_name"], "UserPromptSubmit");
        assert_eq!(v["prompt"], "hello");
    }

    #[test]
    fn session_start_has_source() {
        let (_, _, c) = ctx();
        let v = session_start(&c, "startup");
        assert_eq!(v["source"], "startup");
    }

    #[test]
    fn stop_has_loop_flag() {
        let (_, _, c) = ctx();
        let v = stop(&c, true);
        assert_eq!(v["stop_hook_active"], true);
    }
}
