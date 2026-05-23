//! `PreToolUseGate` — savvagent-internal trait for gating tool dispatch.
//!
//! This is NOT part of the WIT-portable plugin surface; it lives in
//! `savvagent-host` and is consulted by the `Host` before
//! `ToolRegistry::call_with_bash_net_override`. The user-hooks plugin
//! implements it; future hooks (e.g. subagent-level gates) may too.

use async_trait::async_trait;
use serde_json::Value;

/// Synchronous gate consulted before each tool dispatch.
#[async_trait]
pub trait PreToolUseGate: Send + Sync {
    /// Decide whether to allow a tool call.
    ///
    /// Implementations should be best-effort and fail open: any panic
    /// the caller might recover from translates to `Allow` rather than
    /// stalling the TUI.
    async fn check(&self, tool_name: &str, input: &Value) -> PreToolDecision;
}

/// Decision returned by a [`PreToolUseGate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreToolDecision {
    /// Allow the tool call to proceed.
    Allow,
    /// Block the call. `reason` is surfaced as the tool result and as a
    /// `[blocked]` PushNote to the user.
    Block(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct AllowAll;

    #[async_trait]
    impl PreToolUseGate for AllowAll {
        async fn check(&self, _name: &str, _input: &Value) -> PreToolDecision {
            PreToolDecision::Allow
        }
    }

    #[tokio::test]
    async fn allow_gate_returns_allow() {
        let g = AllowAll;
        assert_eq!(
            g.check("run", &json!({"cmd": "ls"})).await,
            PreToolDecision::Allow
        );
    }

    struct DenyAll;

    #[async_trait]
    impl PreToolUseGate for DenyAll {
        async fn check(&self, name: &str, _input: &Value) -> PreToolDecision {
            PreToolDecision::Block(format!("deny {name}"))
        }
    }

    #[tokio::test]
    async fn deny_gate_returns_block_with_reason() {
        let g = DenyAll;
        match g.check("run", &json!({})).await {
            PreToolDecision::Block(r) => assert_eq!(r, "deny run"),
            _ => panic!(),
        }
    }
}
