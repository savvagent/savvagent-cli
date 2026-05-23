//! `PreToolUseGate` impl that walks the per-event hooks for
//! `PreToolUse`, runs each matching hook sequentially via
//! `runner::run_one`, and returns the first `Block` (or `Allow` if
//! none).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::{PreToolDecision, PreToolUseGate};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::plugin::builtin::user_hooks::decision::HookDecision;
use crate::plugin::builtin::user_hooks::discovery::{HookEvent, HooksIndex};
use crate::plugin::builtin::user_hooks::payload;
use crate::plugin::builtin::user_hooks::runner;

/// The gate object shared between `App` and the plugin. Holds the
/// hooks index plus a session id and a callback that builds the
/// transcript path (which can change over the session).
pub struct UserHooksPreToolGate {
    pub hooks: Arc<RwLock<HooksIndex>>,
    pub session_id: String,
    pub project_root: PathBuf,
    pub transcript_path: Arc<RwLock<PathBuf>>,
}

#[async_trait]
impl PreToolUseGate for UserHooksPreToolGate {
    async fn check(&self, tool_name: &str, input: &Value) -> PreToolDecision {
        let idx = self.hooks.read().await;
        let Some(groups) = idx.by_event.get(&HookEvent::PreToolUse) else {
            return PreToolDecision::Allow;
        };
        let transcript = self.transcript_path.read().await.clone();
        let ctx = payload::HookContext {
            session_id: &self.session_id,
            transcript_path: &transcript,
            cwd: &self.project_root,
        };
        let payload = payload::pre_tool_use(&ctx, tool_name, input);
        for group in groups {
            if !group.matcher.is_match(tool_name) {
                continue;
            }
            for cmd in &group.commands {
                let (decision, warnings, _stdout, _stderr) = runner::run_one(
                    HookEvent::PreToolUse,
                    &cmd.command,
                    cmd.timeout,
                    &payload,
                    &self.project_root,
                )
                .await;
                for w in &warnings {
                    tracing::warn!("user-hooks: {w}");
                }
                match decision {
                    HookDecision::Block { reason, .. } => {
                        return PreToolDecision::Block(reason);
                    }
                    HookDecision::Continue { .. } => continue,
                }
            }
        }
        PreToolDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allow_when_no_pre_tool_use_hooks() {
        let g = UserHooksPreToolGate {
            hooks: Arc::new(RwLock::new(HooksIndex::default())),
            session_id: "sid".into(),
            project_root: PathBuf::from("/tmp"),
            transcript_path: Arc::new(RwLock::new(PathBuf::from("/t.json"))),
        };
        assert_eq!(
            g.check("run", &serde_json::json!({})).await,
            PreToolDecision::Allow
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn block_on_exit_2_hook() {
        use crate::plugin::builtin::user_hooks::config::HookCommand;
        use crate::plugin::builtin::user_hooks::discovery::CompiledGroup;
        use crate::plugin::builtin::user_hooks::matcher::CompiledMatcher;

        let mut idx = HooksIndex::default();
        let group = CompiledGroup {
            matcher: CompiledMatcher::compile("*").expect("compile *"),
            commands: vec![HookCommand {
                type_field: "command".into(),
                command: "echo nope >&2; exit 2".into(),
                timeout: 60,
            }],
            source: PathBuf::from("test"),
        };
        idx.by_event
            .entry(HookEvent::PreToolUse)
            .or_default()
            .push(group);

        let g = UserHooksPreToolGate {
            hooks: Arc::new(RwLock::new(idx)),
            session_id: "sid".into(),
            project_root: PathBuf::from("/tmp"),
            transcript_path: Arc::new(RwLock::new(PathBuf::from("/t.json"))),
        };
        assert_eq!(
            g.check("run", &serde_json::json!({})).await,
            PreToolDecision::Block("nope".into())
        );
    }
}
