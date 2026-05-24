//! `task` in-process tool handler. The parent model calls this with a
//! `subagent_type` chosen from the discovered agent index; the handler
//! resolves the agent, builds a [`SubHost`], drives it to completion,
//! and returns the final assistant text wrapped as a `Value::String`.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::{SubHost, SubagentContext, ToolCallContext, max_depth_from_env};
use savvagent_plugin::{InProcessToolHandler, InProcessToolHandlerArc};
use savvagent_protocol::ToolDef;
use serde::Deserialize;
use serde_json::Value;

use crate::plugin::builtin::user_agents::index::AgentIndex;
use crate::plugin::builtin::user_agents::spec::ToolsScope;

/// JSON shape expected from the model when it calls the `task` tool.
#[derive(Debug, Deserialize)]
struct TaskInput {
    /// 3-5 word task label, used by the TUI's collapsible task block
    /// (Task 22 surfaces this). Accepted today so model calls that
    /// include it don't fail validation.
    #[allow(dead_code)]
    description: String,
    /// The actual prompt forwarded to the subagent as the first user
    /// message.
    prompt: String,
    /// The agent slug, drawn from the `task` tool's enum schema (built
    /// from [`AgentIndex::names_snapshot`]).
    subagent_type: String,
}

/// In-process handler for the `task` tool. Holds an [`AgentIndex`]
/// (shared with the discovery side) so each invocation resolves the
/// caller's `subagent_type` against the current agent map.
pub struct TaskToolHandler {
    index: AgentIndex,
}

impl TaskToolHandler {
    /// Build a handler over an existing [`AgentIndex`]. The index is
    /// cheap to clone (it wraps an `Arc<RwLock<_>>`).
    pub fn new(index: AgentIndex) -> Self {
        Self { index }
    }
}

#[async_trait]
impl InProcessToolHandler for TaskToolHandler {
    async fn call(
        &self,
        input: Value,
        ctx: Arc<dyn std::any::Any + Send + Sync>,
    ) -> Result<Value, String> {
        let input: TaskInput =
            serde_json::from_value(input).map_err(|e| format!("task: invalid input: {e}"))?;

        let tool_ctx = ctx
            .downcast_ref::<ToolCallContext>()
            .ok_or_else(|| "task: ToolCallContext missing in tool context".to_string())?;

        let spec = self
            .index
            .get(&input.subagent_type)
            .await
            .ok_or_else(|| format!("unknown subagent_type: {}", input.subagent_type))?;

        // Compute depth. Parent's first call → depth 1; nested → +1 from parent ctx.
        let parent_depth = tool_ctx.subagent.as_ref().map(|s| s.depth).unwrap_or(0);
        let next_depth = parent_depth + 1;

        // Reuse the parent's session id when this is a nested subagent
        // so hooks can correlate across levels. The top-level case
        // (no `subagent`) reads the host's session_id field — wired
        // through by Task 21 via the in-process dispatch path.
        let parent_session_id = match tool_ctx.subagent.as_ref() {
            Some(s) => s.parent_session_id.clone(),
            None => tool_ctx.host.session_id(),
        };

        let sub_ctx = SubagentContext {
            depth: next_depth,
            agent_name: spec.name.clone(),
            parent_session_id,
        };

        // Compute the per-subagent tool view + allowlist from the
        // parent's full registry view (stdio defs + any in-process
        // tools, incl. `task` itself).
        let parent_defs = match tool_ctx.host.tool_registry_arc().await {
            Some(reg) => reg.tool_defs().await,
            None => return Err("task: host has been shut down".into()),
        };
        let (allowed, defs) = filter_tools(&spec.tools, &parent_defs, next_depth);

        // Child cancellation token off the parent turn's token so an
        // aborted parent turn cancels every subagent it spawned.
        let cancellation = tool_ctx.cancellation.child_token();

        let sub = SubHost::new(
            Arc::clone(&tool_ctx.host),
            sub_ctx,
            spec.body.clone(),
            spec.model.clone(),
            allowed,
            defs,
            cancellation,
            None, // events — Task 23 wires SubagentStreamEvent later.
        )
        .await
        .map_err(|e| format!("task: {e}"))?;

        match sub.run_subagent(input.prompt).await {
            Ok(text) => Ok(Value::String(text)),
            Err(e) => Err(format!("subagent {}: {e}", input.subagent_type)),
        }
    }
}

/// Build the per-subagent tool allowlist + `ToolDef` slice from the
/// agent's `tools:` frontmatter and the parent's current tool defs.
///
/// Behavior:
/// - `Inherit` → all parent tools (minus `task` if `depth >= max`).
/// - `Empty` → only `task` (and only if `depth < max`; at the cap, the
///   subagent has no tools at all).
/// - `Allowed(set)` → that set + `task` (if `depth < max`), filtered
///   to only names actually present in the parent's `tool_defs()`.
fn filter_tools(
    scope: &ToolsScope,
    parent: &[ToolDef],
    depth: u8,
) -> (HashSet<String>, Vec<ToolDef>) {
    let max_depth = max_depth_from_env();
    let include_task = depth < max_depth;

    match scope {
        ToolsScope::Inherit => {
            let allowed: HashSet<String> = parent
                .iter()
                .filter(|d| include_task || d.name != "task")
                .map(|d| d.name.clone())
                .collect();
            let defs = parent
                .iter()
                .filter(|d| include_task || d.name != "task")
                .cloned()
                .collect();
            (allowed, defs)
        }
        ToolsScope::Empty => {
            let mut allowed = HashSet::new();
            let mut defs = Vec::new();
            if include_task && let Some(t) = parent.iter().find(|d| d.name == "task") {
                allowed.insert("task".to_string());
                defs.push(t.clone());
            }
            (allowed, defs)
        }
        ToolsScope::Allowed(set) => {
            let mut allowed: HashSet<String> = set.iter().cloned().collect();
            if include_task {
                allowed.insert("task".into());
            }
            let defs = parent
                .iter()
                .filter(|d| allowed.contains(&d.name))
                .cloned()
                .collect();
            (allowed, defs)
        }
    }
}

/// Build the `ToolDef` exposed to the model for the `task` tool. The
/// `subagent_type` enum is the live snapshot of discovered agent names,
/// so a model call referring to a stale name fails JSON-schema
/// validation before reaching the handler.
pub async fn build_tool_def(index: &AgentIndex) -> ToolDef {
    let names = index.names_snapshot().await;
    ToolDef {
        name: "task".into(),
        description:
            "Spawn a subagent to handle a focused task. Returns the subagent's final response."
                .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["description", "prompt", "subagent_type"],
            "properties": {
                "description": { "type": "string", "description": "3-5 word task label" },
                "prompt": { "type": "string", "description": "The task for the subagent" },
                "subagent_type": { "type": "string", "enum": names }
            }
        }),
    }
}

/// Convenience: wrap [`TaskToolHandler`] in the newtype that
/// `Effect::RegisterInProcessTool` accepts.
pub fn handler_arc(index: AgentIndex) -> InProcessToolHandlerArc {
    InProcessToolHandlerArc::new(TaskToolHandler::new(index))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: format!("{name} description"),
            input_schema: serde_json::json!({}),
        }
    }

    #[test]
    fn filter_inherit_keeps_all_with_task() {
        let parent = vec![
            def("tool-fs:read_file"),
            def("tool-grep:search"),
            def("task"),
        ];
        let (allowed, defs) = filter_tools(&ToolsScope::Inherit, &parent, 1);
        // Depth 1 < default max (3) → task included.
        assert_eq!(allowed.len(), 3);
        assert_eq!(defs.len(), 3);
        assert!(allowed.contains("task"));
    }

    #[test]
    fn filter_empty_keeps_only_task_when_under_depth() {
        let parent = vec![def("tool-fs:read_file"), def("task")];
        let (allowed, defs) = filter_tools(&ToolsScope::Empty, &parent, 1);
        assert_eq!(allowed.len(), 1);
        assert!(allowed.contains("task"));
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "task");
    }

    #[test]
    fn filter_allowed_keeps_subset_plus_task() {
        let parent = vec![
            def("tool-fs:read_file"),
            def("tool-grep:search"),
            def("task"),
        ];
        let mut allowlist = HashSet::new();
        allowlist.insert("tool-fs:read_file".to_string());
        let (allowed, defs) = filter_tools(&ToolsScope::Allowed(allowlist), &parent, 1);
        assert!(allowed.contains("tool-fs:read_file"));
        assert!(allowed.contains("task"));
        assert!(!allowed.contains("tool-grep:search"));
        assert_eq!(defs.len(), 2);
    }

    #[test]
    fn filter_depth_at_max_strips_task() {
        let parent = vec![def("tool-fs:read_file"), def("task")];
        let max = max_depth_from_env();
        let (allowed, defs) = filter_tools(&ToolsScope::Inherit, &parent, max);
        assert!(!allowed.contains("task"));
        assert!(!defs.iter().any(|d| d.name == "task"));
        assert!(allowed.contains("tool-fs:read_file"));
    }
}
