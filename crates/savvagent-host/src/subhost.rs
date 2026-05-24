//! `SubHost` — a subagent execution context. Owns its own session
//! state, system prompt, model selection, and tool filter; shares the
//! parent's `ProviderClient`, `ToolRegistry`, `PreToolUseGate`,
//! permissions, and sandbox config via `Arc`.
//!
//! See `docs/superpowers/specs/2026-05-23-user-agents-design.md` §2.

use std::collections::HashSet;
use std::sync::Arc;

use savvagent_protocol::ToolDef;
use tokio_util::sync::CancellationToken;

use crate::Host;
use crate::scoped_registry::ScopedToolRegistry;
use crate::tools::{NetOverride, SubagentContext, ToolCallContext, ToolCallOutcome};

tokio::task_local! {
    /// The name of the currently-executing subagent, if any. Set by
    /// [`SubHost::dispatch_tool`] for the lifetime of the gate +
    /// dispatch call so cross-crate plugins can read the value
    /// without an explicit signature.
    ///
    /// Carried as `Option<String>` so the absence-of-subagent case
    /// (parent turn) is `Some(None)` inside a scope and `Err(_)`
    /// outside any scope, both of which `try_with(...).ok().flatten()`
    /// collapses to `None`.
    pub static SUBAGENT_NAME: Option<String>;
}

const DEFAULT_MAX_DEPTH: u8 = 3;

/// Read `SAVVAGENT_AGENT_MAX_DEPTH` (default 3). Parse failures fall
/// back to the default with no warning.
pub fn max_depth_from_env() -> u8 {
    std::env::var("SAVVAGENT_AGENT_MAX_DEPTH")
        .ok()
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(DEFAULT_MAX_DEPTH)
}

/// Sub-Host configuration. Built by `TaskToolHandler` from an
/// `AgentSpec` and a parent `ToolCallContext`.
///
/// Fields are `pub(crate)` because the subagent loop and helpers live
/// inside `savvagent-host`; external constructors go through
/// [`SubHost::new`].
pub struct SubHost {
    pub(crate) parent: Arc<Host>,
    pub(crate) ctx: SubagentContext,
    pub(crate) system_prompt: String,
    pub(crate) model: Option<String>,
    pub(crate) tools: ScopedToolRegistry,
    pub(crate) tool_defs: Vec<ToolDef>,
    pub(crate) cancellation: CancellationToken,
    /// Optional channel used to emit lifecycle events (currently just
    /// [`TurnEvent::SubagentStop`]) into the parent's per-turn
    /// `TurnEvent` stream. `None` in tests or contexts that don't
    /// need lifecycle visibility.
    pub(crate) events: Option<tokio::sync::mpsc::Sender<crate::TurnEvent>>,
}

impl SubHost {
    /// Build a `SubHost` over `parent`'s shared resources.
    ///
    /// `allowed_names` is the per-subagent tool allowlist, applied at
    /// dispatch time by [`ScopedToolRegistry`]. `tool_defs` is the
    /// pre-filtered slice of `ToolDef`s exposed to the model — callers
    /// (typically `TaskToolHandler`) own the filtering policy.
    ///
    /// Async because pulling the parent's `Arc<ToolRegistry>` out of
    /// its `Mutex<Option<_>>` requires the async lock.
    ///
    /// Returns:
    /// - `Err(SubHostError::DepthExceeded)` if `ctx.depth` exceeds
    ///   [`max_depth_from_env`].
    /// - `Err(SubHostError::HostShutDown)` if the parent host has
    ///   already been shut down (no `Arc<ToolRegistry>` to share).
    #[allow(clippy::too_many_arguments)] // Builder-shaped ctor; refactor deferred to follow-up.
    pub async fn new(
        parent: Arc<Host>,
        ctx: SubagentContext,
        system_prompt: String,
        model: Option<String>,
        allowed_names: HashSet<String>,
        tool_defs: Vec<ToolDef>,
        cancellation: CancellationToken,
        events: Option<tokio::sync::mpsc::Sender<crate::TurnEvent>>,
    ) -> Result<Self, SubHostError> {
        // Depth check BEFORE pulling the registry (cheaper to fail fast).
        if ctx.depth > max_depth_from_env() {
            return Err(SubHostError::DepthExceeded);
        }
        let registry = parent
            .tool_registry_arc()
            .await
            .ok_or(SubHostError::HostShutDown)?;
        let tools = ScopedToolRegistry::new(registry, allowed_names);
        Ok(Self {
            parent,
            ctx,
            system_prompt,
            model,
            tools,
            tool_defs,
            cancellation,
            events,
        })
    }

    /// Drive the subagent loop to its `end_turn`. Returns the final
    /// assistant text or an error.
    ///
    /// Mirrors `Host::run_turn_inner`'s shape but with:
    ///
    /// - A local message vector (the subagent's history is private —
    ///   it never touches the parent's `SessionState`).
    /// - A per-call tool allowlist (`ScopedToolRegistry`).
    /// - A child `CancellationToken` so the parent can abort the
    ///   subagent without affecting its own turn.
    /// - `events: None` on `provider.complete` — Task 23 wires
    ///   private subagent streaming.
    pub async fn run_subagent(&self, prompt: String) -> Result<String, SubHostError> {
        use savvagent_protocol::{CompleteRequest, ContentBlock, Message, Role, StopReason};

        let mut messages: Vec<Message> = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: prompt }],
        }];

        // Take a single lease for the whole subagent loop. The lease's
        // RAII guard keeps the provider client alive even if the parent
        // pool entry is drained concurrently — same discipline as
        // `Host::run_turn_inner`.
        let lease = self
            .parent
            .active_provider_lease()
            .await
            .map_err(|e| SubHostError::Provider(e.to_string()))?;
        let client = Arc::clone(lease.client());

        let model = match &self.model {
            Some(m) => m.clone(),
            None => self.parent.current_model_snapshot().await,
        };

        loop {
            if self.cancellation.is_cancelled() {
                return Err(SubHostError::Cancelled);
            }

            let req = CompleteRequest {
                model: model.clone(),
                messages: messages.clone(),
                system: Some(self.system_prompt.clone()),
                tools: self.tool_defs.clone(),
                temperature: None,
                top_p: None,
                max_tokens: self.parent.config().max_tokens,
                stop_sequences: Vec::new(),
                stream: false,
                thinking: None,
                metadata: None,
            };

            let resp = client
                .complete(req, None)
                .await
                .map_err(|e| SubHostError::Provider(e.to_string()))?;

            // Echo the assistant turn into local history so the next
            // `complete` sees it.
            messages.push(Message {
                role: Role::Assistant,
                content: resp.content.clone(),
            });

            match resp.stop_reason {
                StopReason::EndTurn => {
                    let result = finalize_text(&resp.content);
                    if result.is_ok() {
                        self.emit_subagent_stop().await;
                    }
                    return result;
                }
                StopReason::ToolUse => {
                    let calls = extract_tool_calls(&resp.content);
                    if calls.is_empty() {
                        // Provider quirk: `stop_reason == ToolUse` but no
                        // `tool_use` block. Treat as end_turn (content is
                        // authoritative).
                        let result = finalize_text(&resp.content);
                        if result.is_ok() {
                            self.emit_subagent_stop().await;
                        }
                        return result;
                    }
                    let mut results: Vec<ContentBlock> = Vec::with_capacity(calls.len());
                    for call in &calls {
                        results.push(self.dispatch_tool(call).await);
                    }
                    messages.push(Message {
                        role: Role::User,
                        content: results,
                    });
                }
                other => {
                    return Err(SubHostError::Provider(format!(
                        "subagent: unexpected stop_reason {other:?}"
                    )));
                }
            }
        }
    }

    /// Emit a [`TurnEvent::SubagentStop`] for this subagent's
    /// `(agent_name, success=true)` if an event sender was provided at
    /// construction time. Best-effort: a closed receiver is ignored.
    async fn emit_subagent_stop(&self) {
        if let Some(events) = &self.events {
            let _ = events
                .send(crate::TurnEvent::SubagentStop {
                    agent_name: self.ctx.agent_name.clone(),
                    success: true,
                })
                .await;
        }
    }

    /// Dispatch a single tool call. Honors the per-subagent allowlist,
    /// the parent's `PreToolUseGate`, and the in-process vs. stdio
    /// routing on the parent's `ToolRegistry`.
    ///
    /// The allowlist check runs *outside* [`SUBAGENT_NAME`]'s scope —
    /// it's a purely local guard that doesn't reach any cross-crate
    /// hook surface, and keeping it out of the scope makes the
    /// task-local's lifetime exactly the surface where it is observed
    /// (the gate's payload builder and any in-process tool handler).
    async fn dispatch_tool(&self, call: &PendingToolCall) -> savvagent_protocol::ContentBlock {
        use savvagent_protocol::ContentBlock;

        // 1. Per-subagent allowlist check. A model that hallucinates a
        //    tool name from training data trips here.
        if !self.tools.allows(&call.name) {
            return ContentBlock::ToolResult {
                tool_use_id: call.id.clone(),
                content: vec![ContentBlock::Text {
                    text: format!("{} not available to this subagent", call.name),
                }],
                is_error: true,
            };
        }

        let agent = Some(self.ctx.agent_name.clone());
        SUBAGENT_NAME.scope(agent, self.dispatch_inner(call)).await
    }

    /// Gate + dispatch portion of [`SubHost::dispatch_tool`]. Always
    /// invoked inside a [`SUBAGENT_NAME`] scope, so any code on its
    /// call path (the parent's `PreToolUseGate`, in-process tool
    /// handlers) can read the subagent name via the task-local.
    async fn dispatch_inner(&self, call: &PendingToolCall) -> savvagent_protocol::ContentBlock {
        use savvagent_protocol::ContentBlock;

        // 2. PreToolUseGate — shared with the parent's gate.
        if let Some(blocked) = self
            .parent
            .check_pre_tool_gate(&call.name, &call.input)
            .await
        {
            return outcome_to_tool_result(&call.id, blocked);
        }

        // 3. Dispatch on the parent's `ToolRegistry`. In-process tools
        //    take an `Arc<ToolCallContext>` so the handler can see we
        //    are running inside a subagent.
        let registry = self.tools.inner();
        if registry.in_process_has(&call.name).await {
            let ctx_value = Arc::new(ToolCallContext {
                host: Arc::clone(&self.parent),
                subagent: Some(self.ctx.clone()),
                cancellation: self.cancellation.child_token(),
            });
            let ctx: Arc<dyn std::any::Any + Send + Sync> = ctx_value;
            match registry
                .call_in_process(&call.name, call.input.clone(), ctx)
                .await
            {
                Ok(v) => ContentBlock::ToolResult {
                    tool_use_id: call.id.clone(),
                    content: vec![ContentBlock::Text {
                        text: match v {
                            serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        },
                    }],
                    is_error: false,
                },
                Err(e) => ContentBlock::ToolResult {
                    tool_use_id: call.id.clone(),
                    content: vec![ContentBlock::Text { text: e }],
                    is_error: true,
                },
            }
        } else {
            let outcome = registry
                .call_with_bash_net_override(&call.name, call.input.clone(), NetOverride::Inherit)
                .await;
            outcome_to_tool_result(&call.id, outcome)
        }
    }
}

/// Internal: a pending tool invocation parsed out of an assistant
/// response. Mirrors `ContentBlock::ToolUse` but as an owned struct so
/// we can pass it through the dispatch helpers without borrowing the
/// content vector.
struct PendingToolCall {
    id: String,
    name: String,
    input: serde_json::Value,
}

/// Internal: pull every `ToolUse` block out of an assistant response.
fn extract_tool_calls(content: &[savvagent_protocol::ContentBlock]) -> Vec<PendingToolCall> {
    use savvagent_protocol::ContentBlock;
    content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some(PendingToolCall {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Internal: concatenate every `Text` block in `content` with newline
/// separators. Errors with [`SubHostError::EmptyOutput`] if no text
/// blocks were present — the subagent contract requires a non-empty
/// final answer.
fn finalize_text(content: &[savvagent_protocol::ContentBlock]) -> Result<String, SubHostError> {
    use savvagent_protocol::ContentBlock;
    let mut out = String::new();
    for block in content {
        if let ContentBlock::Text { text } = block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    if out.is_empty() {
        Err(SubHostError::EmptyOutput)
    } else {
        Ok(out)
    }
}

/// Internal: convert a [`ToolCallOutcome`] into a `tool_result`
/// content block tagged with `tool_use_id`.
fn outcome_to_tool_result(id: &str, outcome: ToolCallOutcome) -> savvagent_protocol::ContentBlock {
    use savvagent_protocol::ContentBlock;
    ContentBlock::ToolResult {
        tool_use_id: id.to_string(),
        content: vec![ContentBlock::Text {
            text: outcome.payload,
        }],
        is_error: outcome.is_error,
    }
}

/// Errors produced by [`SubHost::run_subagent`].
#[derive(Debug, thiserror::Error)]
pub enum SubHostError {
    /// The subagent's `CancellationToken` was tripped.
    #[error("subagent cancelled")]
    Cancelled,
    /// `SAVVAGENT_AGENT_MAX_DEPTH` would be exceeded by this dispatch.
    #[error("subagent depth limit exceeded")]
    DepthExceeded,
    /// The subagent reached `end_turn` without producing assistant text.
    #[error("subagent produced no output")]
    EmptyOutput,
    /// The parent host has been shut down — no `Arc<ToolRegistry>` to share.
    #[error("parent host has been shut down")]
    HostShutDown,
    /// The provider client returned an error.
    #[error("provider error: {0}")]
    Provider(String),
    /// A tool dispatch (or its allowlist gate) returned an error.
    #[error("tool error: {0}")]
    Tool(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_host_error_variants_compile() {
        // Smoke test: each variant constructs.
        let _ = SubHostError::Cancelled;
        let _ = SubHostError::DepthExceeded;
        let _ = SubHostError::EmptyOutput;
        let _ = SubHostError::HostShutDown;
        let _ = SubHostError::Provider("p".into());
        let _ = SubHostError::Tool("t".into());
    }

    #[test]
    fn finalize_text_concatenates_text_blocks() {
        use savvagent_protocol::ContentBlock;
        let blocks = vec![
            ContentBlock::Text {
                text: "hello".into(),
            },
            ContentBlock::Text {
                text: "world".into(),
            },
        ];
        let out = finalize_text(&blocks).expect("text");
        assert_eq!(out, "hello\nworld");
    }

    #[test]
    fn finalize_text_empty_blocks_errors() {
        let out = finalize_text(&[]);
        assert!(matches!(out, Err(SubHostError::EmptyOutput)));
    }

    #[test]
    fn extract_tool_calls_filters_text() {
        use savvagent_protocol::ContentBlock;
        let blocks = vec![
            ContentBlock::Text {
                text: "thinking".into(),
            },
            ContentBlock::ToolUse {
                id: "1".into(),
                name: "tool".into(),
                input: serde_json::json!({}),
            },
        ];
        let calls = extract_tool_calls(&blocks);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "tool");
        assert_eq!(calls[0].id, "1");
    }
}
