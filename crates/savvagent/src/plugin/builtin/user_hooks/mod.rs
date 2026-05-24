//! `internal:user-hooks` — discovers and dispatches Claude-Code-compatible
//! user shell hooks from `settings.json`. See
//! `docs/superpowers/specs/2026-05-22-user-hooks-design.md`.

mod config;
mod decision;
pub mod discovery;
mod matcher;
mod payload;
pub mod pre_tool_gate;
mod runner;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, SlashSpec,
    StyledLine,
};
use serde_json::json;
use tokio::sync::RwLock;

use crate::plugin::builtin::provider_common::BuiltinHookPlugin;
use crate::plugin::builtin::user_hooks::decision::HookDecision;
use crate::plugin::builtin::user_hooks::discovery::{HookEvent, HooksIndex};
use crate::plugin::builtin::user_hooks::payload::HookContext;
use crate::plugin::builtin::user_hooks::pre_tool_gate::UserHooksPreToolGate;

/// Built-in plugin that exposes user-authored shell hooks.
///
/// # v1 limitations on `PostToolUse`
///
/// `HostEvent::ToolCallEnd` currently carries only `{ call_id, success }`
/// — there is no `tool_name`, `tool_input`, or `tool_response` payload.
/// As a result:
///
/// * `PostToolUse` hooks only fire when their `matcher` matches `"*"`
///   (tool-specific matchers are skipped — they have nothing to match
///   against).
/// * The stdin payload uses sentinel values: `tool_name = "<unknown>"`,
///   `tool_input = {}`, and `tool_response = { "success": <bool> }`.
///
/// Lifting these requires enriching `HostEvent::ToolCallEnd` with the
/// tool's name and IO buffers; tracked as a follow-up.
pub struct UserHooksPlugin {
    pub hooks: Arc<RwLock<HooksIndex>>,
    pub session_id: String,
    pub project_root: PathBuf,
    pub transcript_path: Arc<RwLock<PathBuf>>,
    cached_gate: Option<Arc<UserHooksPreToolGate>>,
    /// Tracks whether a previous `Stop` hook in the current stop-cycle
    /// returned `Block`. Flips to `true` after any `Block` decision in
    /// `dispatch_stop`; reset to `false` on the next `TurnStart` (which
    /// marks a fresh agent turn). The value is passed verbatim as the
    /// `stop_hook_active` field of the next `Stop` payload — matching the
    /// Claude Code contract so user hooks can detect "the agent already
    /// tried to stop once" and avoid infinite block loops once the host
    /// gains a re-run-on-Stop-block mechanism.
    prev_stop_blocked: bool,
}

impl UserHooksPlugin {
    /// Construct a new [`UserHooksPlugin`].
    pub fn new(
        hooks: Arc<RwLock<HooksIndex>>,
        session_id: String,
        project_root: PathBuf,
        transcript_path: Arc<RwLock<PathBuf>>,
    ) -> Self {
        Self {
            hooks,
            session_id,
            project_root,
            transcript_path,
            cached_gate: None,
            prev_stop_blocked: false,
        }
    }

    fn gate_arc(&mut self) -> Arc<UserHooksPreToolGate> {
        if let Some(g) = self.cached_gate.as_ref() {
            return g.clone();
        }
        let g = Arc::new(UserHooksPreToolGate {
            hooks: self.hooks.clone(),
            session_id: self.session_id.clone(),
            project_root: self.project_root.clone(),
            transcript_path: self.transcript_path.clone(),
        });
        self.cached_gate = Some(g.clone());
        g
    }

    /// Dispatch `PostToolUse` hooks. See struct-level docs for the v1
    /// limitations (only `"*"`-matching groups run; sentinel tool
    /// payload).
    async fn dispatch_post_tool_use(&mut self, success: bool) -> Result<Vec<Effect>, PluginError> {
        let idx = self.hooks.read().await;
        let Some(groups) = idx.by_event.get(&HookEvent::PostToolUse) else {
            return Ok(vec![]);
        };
        let groups = groups.clone();
        drop(idx);

        let transcript = self.transcript_path.read().await.clone();
        let ctx = HookContext {
            session_id: &self.session_id,
            transcript_path: &transcript,
            cwd: &self.project_root,
        };
        let payload = payload::post_tool_use(
            &ctx,
            "<unknown>",
            &json!({}),
            &json!({ "success": success }),
            None,
        );

        let mut effects: Vec<Effect> = Vec::new();
        for group in &groups {
            // v1: tool name/IO aren't in `ToolCallEnd`, so we can only
            // dispatch hooks whose matcher catches `"*"`. Tool-specific
            // matchers are skipped pending a richer payload.
            if !group.matcher.is_match("*") {
                continue;
            }
            for cmd in &group.commands {
                let (decision, warnings, stdout, stderr) = runner::run_one(
                    HookEvent::PostToolUse,
                    &cmd.command,
                    cmd.timeout,
                    &payload,
                    &self.project_root,
                )
                .await;
                for w in &warnings {
                    effects.push(Effect::PushNote {
                        line: StyledLine::plain(format!("[warn] {w}")),
                    });
                }
                match decision {
                    HookDecision::Continue {
                        suppress_output, ..
                    } => {
                        if !suppress_output {
                            let so = stdout.trim_end();
                            if !so.is_empty() {
                                effects.push(Effect::PushNote {
                                    line: StyledLine::plain(so.to_string()),
                                });
                            }
                            let se = stderr.trim_end();
                            if !se.is_empty() {
                                effects.push(Effect::PushNote {
                                    line: StyledLine::plain(se.to_string()),
                                });
                            }
                        }
                    }
                    HookDecision::Block { reason, .. } => {
                        // PostToolUse cannot block per spec. Demote
                        // to a warning note and continue the chain.
                        effects.push(Effect::PushNote {
                            line: StyledLine::plain(format!(
                                "[warn] PostToolUse hooks cannot block; ignoring Block from `{}`: {reason}",
                                cmd.command
                            )),
                        });
                    }
                }
            }
        }
        Ok(effects)
    }

    /// Dispatch `SessionStart` hooks. Source is hardcoded to `"startup"`
    /// in v1 (we don't distinguish resume/clear). All groups run — the
    /// matcher field is ignored for non-tool events.
    ///
    /// Emits `Effect::RegisterPreToolGate` as the first effect so the
    /// runtime installs the gate on the host before any tool call can
    /// happen. This is the one-shot path: HostStarting fires exactly
    /// once per host lifecycle, so the gate is registered exactly once.
    async fn dispatch_session_start(&mut self) -> Result<Vec<Effect>, PluginError> {
        let mut effects: Vec<Effect> = vec![Effect::RegisterPreToolGate {
            plugin_id: PluginId::new("internal:user-hooks").expect("valid built-in id"),
        }];

        let idx = self.hooks.read().await;
        let Some(groups) = idx.by_event.get(&HookEvent::SessionStart) else {
            return Ok(effects);
        };
        let groups = groups.clone();
        drop(idx);

        let transcript = self.transcript_path.read().await.clone();
        let ctx = HookContext {
            session_id: &self.session_id,
            transcript_path: &transcript,
            cwd: &self.project_root,
        };
        let payload = payload::session_start(&ctx, "startup");

        for group in &groups {
            // SessionStart is not a tool event; matcher is ignored.
            for cmd in &group.commands {
                let (decision, warnings, stdout, stderr) = runner::run_one(
                    HookEvent::SessionStart,
                    &cmd.command,
                    cmd.timeout,
                    &payload,
                    &self.project_root,
                )
                .await;
                for w in &warnings {
                    effects.push(Effect::PushNote {
                        line: StyledLine::plain(format!("[warn] {w}")),
                    });
                }
                match decision {
                    HookDecision::Continue {
                        suppress_output, ..
                    } => {
                        if !suppress_output {
                            let so = stdout.trim_end();
                            if !so.is_empty() {
                                effects.push(Effect::PushNote {
                                    line: StyledLine::plain(so.to_string()),
                                });
                            }
                            let se = stderr.trim_end();
                            if !se.is_empty() {
                                effects.push(Effect::PushNote {
                                    line: StyledLine::plain(se.to_string()),
                                });
                            }
                        }
                    }
                    HookDecision::Block { reason, .. } => {
                        // SessionStart cannot block startup. Demote to
                        // a warning note and continue.
                        effects.push(Effect::PushNote {
                            line: StyledLine::plain(format!(
                                "[warn] SessionStart hooks cannot block; ignoring Block from `{}`: {reason}",
                                cmd.command
                            )),
                        });
                    }
                }
            }
        }
        Ok(effects)
    }

    /// Dispatch `UserPromptSubmit` hooks. All groups run — the matcher
    /// field is ignored for non-tool events. Block decisions short-circuit
    /// the chain and surface as `Effect::CancelPendingTurn`. `Continue`
    /// decisions with `additional_context` emit
    /// `Effect::PrependToPendingPrompt`.
    async fn dispatch_user_prompt_submit(
        &mut self,
        prompt: &str,
    ) -> Result<Vec<Effect>, PluginError> {
        let idx = self.hooks.read().await;
        let Some(groups) = idx.by_event.get(&HookEvent::UserPromptSubmit) else {
            return Ok(vec![]);
        };
        let groups = groups.clone();
        drop(idx);

        let transcript = self.transcript_path.read().await.clone();
        let ctx = HookContext {
            session_id: &self.session_id,
            transcript_path: &transcript,
            cwd: &self.project_root,
        };
        let payload = payload::user_prompt_submit(&ctx, prompt);

        let mut effects: Vec<Effect> = Vec::new();
        for group in &groups {
            // UserPromptSubmit is not a tool event; matcher is ignored.
            for cmd in &group.commands {
                let (decision, warnings, _stdout, _stderr) = runner::run_one(
                    HookEvent::UserPromptSubmit,
                    &cmd.command,
                    cmd.timeout,
                    &payload,
                    &self.project_root,
                )
                .await;
                for w in &warnings {
                    effects.push(Effect::PushNote {
                        line: StyledLine::plain(format!("[warn] {w}")),
                    });
                }
                match decision {
                    HookDecision::Block { reason, .. } => {
                        effects.push(Effect::CancelPendingTurn { reason });
                        return Ok(effects);
                    }
                    HookDecision::Continue {
                        additional_context, ..
                    } => {
                        if let Some(extra) = additional_context {
                            if !extra.is_empty() {
                                effects.push(Effect::PrependToPendingPrompt { text: extra });
                            }
                        }
                    }
                }
            }
        }
        Ok(effects)
    }

    /// Dispatch `Stop` hooks. Block decisions short-circuit and surface
    /// as `Effect::CancelPendingTurn`. `additional_context` is ignored
    /// for `Stop` (per spec the turn is ending, so prepending a prompt
    /// prefix would be meaningless).
    ///
    /// `stop_hook_active` is `true` when a previous `Stop` hook in the
    /// current stop-cycle already returned `Block`. The flag is captured
    /// at the start of dispatch (so all hooks within one `TurnEnd` see
    /// the same value) and updated to `true` if any hook in this
    /// invocation blocks. `TurnStart` resets it (see `on_event`).
    async fn dispatch_stop(&mut self, success: bool) -> Result<Vec<Effect>, PluginError> {
        // `success` is reserved for a future stop-on-failure variant;
        // v1 payload does not expose it.
        let _ = success;
        let stop_hook_active = self.prev_stop_blocked;
        let idx = self.hooks.read().await;
        let Some(groups) = idx.by_event.get(&HookEvent::Stop) else {
            return Ok(vec![]);
        };
        let groups = groups.clone();
        drop(idx);

        let transcript = self.transcript_path.read().await.clone();
        let ctx = HookContext {
            session_id: &self.session_id,
            transcript_path: &transcript,
            cwd: &self.project_root,
        };
        let payload = payload::stop(&ctx, stop_hook_active);

        let mut effects: Vec<Effect> = Vec::new();
        for group in &groups {
            // Stop is not a tool event; matcher is ignored.
            for cmd in &group.commands {
                let (decision, warnings, _stdout, _stderr) = runner::run_one(
                    HookEvent::Stop,
                    &cmd.command,
                    cmd.timeout,
                    &payload,
                    &self.project_root,
                )
                .await;
                for w in &warnings {
                    effects.push(Effect::PushNote {
                        line: StyledLine::plain(format!("[warn] {w}")),
                    });
                }
                match decision {
                    HookDecision::Block { reason, .. } => {
                        self.prev_stop_blocked = true;
                        effects.push(Effect::CancelPendingTurn { reason });
                        return Ok(effects);
                    }
                    HookDecision::Continue { .. } => {
                        // Stop hooks can't inject prompt context — the
                        // turn is ending. Silently drop additionalContext
                        // and any stdout/stderr surfacing.
                    }
                }
            }
        }
        Ok(effects)
    }

    /// Dispatch `SubagentStop` hooks. Fires when a subagent's
    /// `SubHost` reaches a clean `end_turn`. Block decisions
    /// short-circuit but do NOT cancel any turn — the subagent has
    /// already returned. `stop_hook_active` is hardcoded to `false`
    /// in v1 (same as `dispatch_stop`; full re-prompt mechanism is a
    /// future follow-up).
    async fn dispatch_subagent_stop(
        &mut self,
        agent_name: &str,
        success: bool,
    ) -> Result<Vec<Effect>, PluginError> {
        // `success` is reserved for a future failure-aware variant;
        // v1 payload does not expose it.
        let _ = success;
        let idx = self.hooks.read().await;
        let Some(groups) = idx.by_event.get(&HookEvent::SubagentStop) else {
            return Ok(vec![]);
        };
        let groups = groups.clone();
        drop(idx);

        let transcript = self.transcript_path.read().await.clone();
        let ctx = HookContext {
            session_id: &self.session_id,
            transcript_path: &transcript,
            cwd: &self.project_root,
        };
        let payload = payload::subagent_stop(&ctx, agent_name, false);

        let mut effects: Vec<Effect> = Vec::new();
        for group in &groups {
            // SubagentStop is not a tool event; matcher is ignored.
            for cmd in &group.commands {
                let (decision, warnings, _stdout, _stderr) = runner::run_one(
                    HookEvent::SubagentStop,
                    &cmd.command,
                    cmd.timeout,
                    &payload,
                    &self.project_root,
                )
                .await;
                for w in &warnings {
                    effects.push(Effect::PushNote {
                        line: StyledLine::plain(format!("[warn] {w}")),
                    });
                }
                match decision {
                    HookDecision::Block { reason, .. } => {
                        // Subagent has already returned its result to
                        // the parent — a Block at this point can't
                        // unwind. Surface as a PushNote so the user
                        // sees the hook spoke up.
                        effects.push(Effect::PushNote {
                            line: StyledLine::plain(format!("[subagent-stop blocked] {reason}")),
                        });
                        return Ok(effects);
                    }
                    HookDecision::Continue { .. } => {
                        // No re-prompt mechanism in v1.
                    }
                }
            }
        }
        Ok(effects)
    }
}

#[async_trait]
impl Plugin for UserHooksPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "reload-hooks".into(),
            summary: "Rescan user-defined hooks (settings.json)".into(),
            args_hint: None,
            requires_arg: false,
        }];
        contributions.hooks = vec![
            savvagent_plugin::HookKind::ToolCallEnd,     // -> PostToolUse
            savvagent_plugin::HookKind::HostStarting,    // -> SessionStart
            savvagent_plugin::HookKind::PromptSubmitted, // -> UserPromptSubmit
            savvagent_plugin::HookKind::TurnStart,       // resets prev_stop_blocked
            savvagent_plugin::HookKind::TurnEnd,         // -> Stop
            savvagent_plugin::HookKind::SubagentStop,    // -> SubagentStop
        ];
        Manifest {
            id: PluginId::new("internal:user-hooks").expect("valid built-in id"),
            name: "User hooks".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Claude-Code-compatible settings.json hooks".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        if name != "reload-hooks" {
            return Ok(vec![]);
        }
        // Re-walk discovery on the same project_root + home (home is
        // dirs::home_dir() at the time of reload).
        let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        let new_idx =
            crate::plugin::builtin::user_hooks::discovery::walk_all(&self.project_root, &home);
        let warnings = new_idx.warnings.clone();
        *self.hooks.write().await = new_idx;
        let mut effs: Vec<Effect> = warnings
            .into_iter()
            .map(|w| Effect::PushNote {
                line: StyledLine::plain(format!("[warn] user-hooks: {w}")),
            })
            .collect();
        effs.push(Effect::ReindexPlugin {
            id: PluginId::new("internal:user-hooks").expect("valid built-in id"),
        });
        effs.push(Effect::PushNote {
            line: StyledLine::plain("user-hooks: reloaded"),
        });
        Ok(effs)
    }

    async fn on_event(
        &mut self,
        event: savvagent_plugin::HostEvent,
    ) -> Result<Vec<Effect>, PluginError> {
        use savvagent_plugin::HostEvent;
        match event {
            HostEvent::ToolCallEnd { success, .. } => self.dispatch_post_tool_use(success).await,
            HostEvent::HostStarting => self.dispatch_session_start().await,
            HostEvent::PromptSubmitted { text } => self.dispatch_user_prompt_submit(&text).await,
            HostEvent::TurnStart { .. } => {
                // Fresh agent turn — clear the Stop-blocked latch so the
                // next dispatch_stop sees stop_hook_active = false unless
                // a Block fires again inside this turn.
                self.prev_stop_blocked = false;
                Ok(vec![])
            }
            HostEvent::TurnEnd { success, .. } => self.dispatch_stop(success).await,
            HostEvent::SubagentStop {
                agent_name,
                success,
            } => self.dispatch_subagent_stop(&agent_name, success).await,
            _ => Ok(vec![]),
        }
    }
}

impl BuiltinHookPlugin for UserHooksPlugin {
    fn take_pre_tool_gate(&mut self) -> Option<Arc<dyn savvagent_host::PreToolUseGate>> {
        Some(self.gate_arc())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::{HookKind, HostEvent};

    fn stub_plugin() -> UserHooksPlugin {
        UserHooksPlugin::new(
            Arc::new(RwLock::new(HooksIndex::default())),
            String::new(),
            PathBuf::from("."),
            Arc::new(RwLock::new(PathBuf::new())),
        )
    }

    fn mk_plugin(idx: HooksIndex) -> UserHooksPlugin {
        UserHooksPlugin::new(
            Arc::new(RwLock::new(idx)),
            "sid".into(),
            PathBuf::from("/tmp"),
            Arc::new(RwLock::new(PathBuf::from("/t.json"))),
        )
    }

    #[test]
    fn manifest_has_reload_hooks() {
        let p = stub_plugin();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:user-hooks");
        let names: Vec<_> = m
            .contributions
            .slash_commands
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"reload-hooks"));
    }

    #[tokio::test]
    async fn no_hooks_still_emits_register_pre_tool_gate_on_host_starting() {
        // With no user-authored SessionStart hooks configured, HostStarting
        // must STILL emit `RegisterPreToolGate` so the runtime installs the
        // PreToolUse gate on the host. This is the one-shot wiring path.
        let mut p = mk_plugin(HooksIndex::default());
        let effs = p.on_event(HostEvent::HostStarting).await.unwrap();
        assert_eq!(
            effs.len(),
            1,
            "expected only RegisterPreToolGate; got {effs:?}"
        );
        assert!(matches!(
            effs[0],
            Effect::RegisterPreToolGate { ref plugin_id }
                if plugin_id.as_str() == "internal:user-hooks"
        ));
    }

    #[tokio::test]
    async fn ignores_unrelated_events() {
        // `Disconnect` isn't in `contributions.hooks` — the plugin should
        // route it through the catch-all arm of `on_event` and emit no
        // effects.
        use savvagent_plugin::ProviderId;
        let mut p = mk_plugin(HooksIndex::default());
        let effs = p
            .on_event(HostEvent::Disconnect {
                provider_id: ProviderId::new("anthropic").unwrap(),
                reason: "test".into(),
            })
            .await
            .unwrap();
        assert!(effs.is_empty());
    }

    /// `HostEvent::TurnStart` resets the `prev_stop_blocked` latch so the
    /// next `Stop` payload sees `stop_hook_active = false`. The arm emits
    /// no effects (the reset is internal state).
    #[tokio::test]
    async fn turn_start_resets_prev_stop_blocked_latch() {
        let mut p = mk_plugin(HooksIndex::default());
        p.prev_stop_blocked = true;
        let effs = p
            .on_event(HostEvent::TurnStart { turn_id: 1 })
            .await
            .unwrap();
        assert!(effs.is_empty(), "TurnStart must not emit effects");
        assert!(
            !p.prev_stop_blocked,
            "TurnStart should clear the stop-blocked latch"
        );
    }

    #[test]
    fn manifest_subscribes_to_expected_kinds() {
        let p = stub_plugin();
        let m = p.manifest();
        let mut kinds = m.contributions.hooks.clone();
        kinds.sort_by_key(|k| format!("{k:?}"));
        let mut expected = vec![
            HookKind::ToolCallEnd,
            HookKind::HostStarting,
            HookKind::PromptSubmitted,
            HookKind::TurnStart,
            HookKind::TurnEnd,
            HookKind::SubagentStop,
        ];
        expected.sort_by_key(|k| format!("{k:?}"));
        assert_eq!(kinds, expected);
    }

    /// Build a `HooksIndex` containing a single group for `event` whose
    /// sole hook runs `command` with `timeout` seconds. Matcher is `*`,
    /// which is harmless for non-tool events (matcher is ignored there).
    #[cfg(unix)]
    fn single_hook_index(event: HookEvent, command: &str) -> HooksIndex {
        use crate::plugin::builtin::user_hooks::config::HookCommand;
        use crate::plugin::builtin::user_hooks::discovery::CompiledGroup;
        use crate::plugin::builtin::user_hooks::matcher::CompiledMatcher;

        let group = CompiledGroup {
            matcher: CompiledMatcher::compile("*").expect("compile *"),
            commands: vec![HookCommand {
                type_field: "command".into(),
                command: command.into(),
                timeout: 5,
            }],
            source: PathBuf::from("test"),
        };
        let mut idx = HooksIndex::default();
        idx.by_event.entry(event).or_default().push(group);
        idx
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn user_prompt_submit_emits_prepend_on_additional_context() {
        // The decision parser recognises `hookSpecificOutput.additionalContext`
        // for UserPromptSubmit and returns
        // `Continue { additional_context: Some("extra"), .. }`.
        let cmd = r#"echo '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"extra"}}'"#;
        let idx = single_hook_index(HookEvent::UserPromptSubmit, cmd);
        let mut p = mk_plugin(idx);
        let effs = p
            .on_event(HostEvent::PromptSubmitted { text: "hi".into() })
            .await
            .unwrap();
        assert!(
            effs.iter().any(|e| matches!(
                e,
                Effect::PrependToPendingPrompt { text } if text == "extra"
            )),
            "expected PrependToPendingPrompt{{text=\"extra\"}} in {effs:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn user_prompt_submit_emits_cancel_on_block() {
        // Exit 2 with stderr → Block { reason: "denied" } per the
        // exit-code-only fallback in `parse_outcome`.
        let cmd = r#"echo 'denied' >&2; exit 2"#;
        let idx = single_hook_index(HookEvent::UserPromptSubmit, cmd);
        let mut p = mk_plugin(idx);
        let effs = p
            .on_event(HostEvent::PromptSubmitted { text: "hi".into() })
            .await
            .unwrap();
        assert!(
            effs.iter().any(|e| matches!(
                e,
                Effect::CancelPendingTurn { reason } if reason == "denied"
            )),
            "expected CancelPendingTurn{{reason=\"denied\"}} in {effs:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reload_emits_reindex_plugin_effect() {
        let mut p = mk_plugin(HooksIndex::default());
        let effs = p.handle_slash("reload-hooks", vec![]).await.unwrap();
        assert!(
            effs.iter()
                .any(|e| matches!(e, Effect::ReindexPlugin { .. })),
            "expected ReindexPlugin in {effs:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reload_ignores_other_slashes() {
        let mut p = mk_plugin(HooksIndex::default());
        let effs = p.handle_slash("not-reload-hooks", vec![]).await.unwrap();
        assert!(
            effs.is_empty(),
            "expected no effects for unrelated slash; got {effs:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reload_emits_reloaded_note_after_reindex() {
        let mut p = mk_plugin(HooksIndex::default());
        let effs = p.handle_slash("reload-hooks", vec![]).await.unwrap();
        let last = effs.last().expect("at least one effect");
        match last {
            Effect::PushNote { line } => {
                let joined: String = line.spans.iter().map(|s| s.text.as_str()).collect();
                assert!(
                    joined.contains("user-hooks: reloaded"),
                    "expected trailing PushNote with 'user-hooks: reloaded'; got {line:?}"
                );
            }
            other => panic!("expected trailing PushNote; got {other:?}"),
        }
    }

    // HOME_LOCK is std::Mutex (shared with sync tests) and must span the await.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    async fn reload_hooks_through_app_index_round_trip() {
        // Proves the shared-Arc contract: the App's `shared_idx` and the
        // plugin's `self.hooks` point at the SAME `Arc<RwLock<HooksIndex>>`,
        // so a write performed by the plugin during /reload-hooks is visible
        // to App-side readers.
        //
        // Setup is empty-on-both-sides: the App-side Arc starts as
        // `HooksIndex::default()` and the plugin is constructed from a
        // clone of that Arc. The only write into the index is the one
        // `handle_slash("reload-hooks", ..)` performs internally via
        // `*self.hooks.write().await = new_idx`. If the plugin had ever
        // allocated its own `Arc<RwLock<HooksIndex>>` instead of using the
        // passed-in handle, the App-side view would still be empty after
        // the reload and the final assertion would fail.

        use crate::test_helpers::{HOME_LOCK, HomeGuard};
        use std::sync::Arc;
        use tempfile::TempDir;
        use tokio::sync::RwLock;

        // Plugin's reload path calls `dirs::home_dir()` for the home walk;
        // pin HOME to a tempdir so the reload never picks up hooks from the
        // dev machine's real `~/.savvagent/settings.json`.
        let _lock = HOME_LOCK.lock().unwrap();
        let _home_guard = HomeGuard::new();

        let proj = TempDir::new().unwrap();
        std::fs::create_dir_all(proj.path().join(".savvagent")).unwrap();
        std::fs::write(
            proj.path().join(".savvagent/settings.json"),
            r#"{
                "hooks": {
                    "PreToolUse": [
                        {
                            "matcher": "*",
                            "hooks": [ { "command": "echo round-trip" } ]
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        // App-side Arc starts EMPTY — no pre-population. The reload below
        // is the sole writer.
        let shared_idx = Arc::new(RwLock::new(HooksIndex::default()));
        let shared_transcript = Arc::new(RwLock::new(std::path::PathBuf::from("/t.json")));

        // Plugin built around a clone of the same Arc as the App-side handle.
        let mut p = UserHooksPlugin::new(
            shared_idx.clone(),
            "test-session".into(),
            proj.path().to_path_buf(),
            shared_transcript.clone(),
        );

        // Sanity-check the precondition: nothing has been written yet.
        assert!(
            shared_idx.read().await.by_event.is_empty(),
            "precondition: App-side index must start empty so the post-reload \
             assertion below proves the plugin's write reached the shared Arc"
        );

        // The plugin's `handle_slash` performs `walk_all(self.project_root, home)`
        // and then `*self.hooks.write().await = new_idx`. `walk_all` scans
        // `project_root.join(".savvagent")` first, independent of the home
        // walk, so the PreToolUse group from the project tempdir lands in
        // the new index regardless of what `dirs::home_dir()` resolves to.
        let effs = p.handle_slash("reload-hooks", vec![]).await.unwrap();
        assert!(
            effs.iter()
                .any(|e| matches!(e, Effect::ReindexPlugin { .. })),
            "expected ReindexPlugin in {effs:?}"
        );

        // If `UserHooksPlugin::new` allocated its own
        // `Arc<RwLock<HooksIndex>>` instead of using the passed-in `hooks`,
        // this read would still see `HooksIndex::default()` and fail.
        let app_view = shared_idx.read().await;
        assert!(
            app_view
                .by_event
                .contains_key(&discovery::HookEvent::PreToolUse),
            "App-side shared Arc must observe the plugin's write; got {:?}",
            app_view.by_event.keys().collect::<Vec<_>>()
        );
    }

    // HOME_LOCK is std::Mutex (shared with sync tests) and must span the await.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    async fn e2e_pre_tool_use_block_short_circuits() {
        use crate::test_helpers::{HOME_LOCK, HomeGuard};

        // Pin HOME for the duration of the test so discovery::walk_all doesn't
        // pick up hooks from the dev machine's real ~/.savvagent/settings.json.
        let _lock = HOME_LOCK.lock().unwrap();
        let _home_guard = HomeGuard::new();

        let tmp = tempfile::TempDir::new().unwrap();
        let proj = tmp.path().to_path_buf();
        std::fs::create_dir_all(proj.join(".savvagent")).unwrap();
        std::fs::write(
            proj.join(".savvagent/settings.json"),
            r#"{
                "hooks": {
                    "PreToolUse": [
                        {
                            "matcher": "*",
                            "hooks": [ { "command": "echo deny >&2; exit 2" } ]
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        let home = tempfile::TempDir::new().unwrap();
        let idx = discovery::walk_all(&proj, home.path());
        let hooks = std::sync::Arc::new(tokio::sync::RwLock::new(idx));
        let transcript = std::sync::Arc::new(tokio::sync::RwLock::new(std::path::PathBuf::from(
            "/t.json",
        )));

        let gate = pre_tool_gate::UserHooksPreToolGate {
            hooks,
            session_id: "sid".into(),
            project_root: proj.clone(),
            transcript_path: transcript,
        };

        use savvagent_host::PreToolUseGate;
        let decision = gate.check("run", &serde_json::json!({})).await;
        match decision {
            savvagent_host::PreToolDecision::Block(reason) => {
                assert_eq!(reason, "deny");
            }
            _ => panic!("expected Block, got {decision:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_emits_cancel_on_block() {
        let cmd = r#"echo 'stop-block' >&2; exit 2"#;
        let idx = single_hook_index(HookEvent::Stop, cmd);
        let mut p = mk_plugin(idx);
        let effs = p
            .on_event(HostEvent::TurnEnd {
                turn_id: 1,
                success: true,
            })
            .await
            .unwrap();
        assert!(
            effs.iter().any(|e| matches!(
                e,
                Effect::CancelPendingTurn { reason } if reason == "stop-block"
            )),
            "expected CancelPendingTurn{{reason=\"stop-block\"}} in {effs:?}"
        );
    }

    /// End-to-end: a `SubagentStop` user shell hook fires when the
    /// dispatcher sees `HostEvent::SubagentStop`. The hook's stdin
    /// payload must include `hook_event_name=SubagentStop`,
    /// `subagent=<name>`, and `stop_hook_active=false`.
    ///
    /// This lives inline (rather than in `tests/`) because the
    /// `savvagent` crate is binary-only — there is no `lib.rs` to
    /// re-export `UserHooksPlugin` through. The test still exercises
    /// the full dispatch pipeline: settings.json -> discovery ->
    /// HooksIndex -> on_event(SubagentStop) -> shell hook execution.
    #[cfg(unix)]
    #[tokio::test]
    async fn subagent_stop_hook_fires_with_payload() {
        let project = tempfile::tempdir().unwrap();
        let stdout_capture = project.path().join("captured.json");

        // Hook reads stdin and writes it to a tempfile so we can
        // inspect what was actually passed.
        let cmd = format!(
            "cat > {} ; echo ok",
            stdout_capture.to_string_lossy().replace('\'', "'\\''"),
        );

        // Build a discovery-shaped HooksIndex directly so the test
        // doesn't depend on the on-disk discovery walker (which has
        // its own coverage). The index points the SubagentStop event
        // at our capture command.
        use crate::plugin::builtin::user_hooks::config::HookCommand;
        use crate::plugin::builtin::user_hooks::discovery::CompiledGroup;
        use crate::plugin::builtin::user_hooks::matcher::CompiledMatcher;

        let group = CompiledGroup {
            matcher: CompiledMatcher::compile("*").expect("compile *"),
            commands: vec![HookCommand {
                type_field: "command".into(),
                command: cmd,
                timeout: 5,
            }],
            source: project.path().to_path_buf(),
        };
        let mut idx = HooksIndex::default();
        idx.by_event
            .entry(HookEvent::SubagentStop)
            .or_default()
            .push(group);

        let mut p = UserHooksPlugin::new(
            Arc::new(RwLock::new(idx)),
            "test-session".into(),
            project.path().to_path_buf(),
            Arc::new(RwLock::new(project.path().join("transcript.json"))),
        );

        let _ = p
            .on_event(HostEvent::SubagentStop {
                agent_name: "code-reviewer".into(),
                success: true,
            })
            .await
            .expect("dispatch should not error");

        let captured = std::fs::read_to_string(&stdout_capture)
            .expect("hook should have written stdin to captured.json");
        let payload: serde_json::Value = serde_json::from_str(&captured).expect("payload is JSON");

        assert_eq!(payload["hook_event_name"], "SubagentStop");
        assert_eq!(payload["subagent"], "code-reviewer");
        assert_eq!(payload["stop_hook_active"], false);
    }

    /// After a `Stop` hook returns `Block`, the plugin sets the
    /// `prev_stop_blocked` latch. This is what the next `Stop` dispatch
    /// will pass as `stop_hook_active` in the payload.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_block_sets_prev_stop_blocked_latch() {
        let cmd = r#"echo 'first-block' >&2; exit 2"#;
        let idx = single_hook_index(HookEvent::Stop, cmd);
        let mut p = mk_plugin(idx);
        assert!(!p.prev_stop_blocked, "latch starts false");
        let _ = p
            .on_event(HostEvent::TurnEnd {
                turn_id: 1,
                success: true,
            })
            .await
            .unwrap();
        assert!(
            p.prev_stop_blocked,
            "latch should be true after a Block decision"
        );
    }

    /// Second `Stop` dispatch (without an intervening `TurnStart`) must
    /// pass `stop_hook_active: true` in the payload. The hook prints
    /// `<stop_hook_active>` to stderr and `exit 2`s; the Block reason
    /// carries that value back so we can assert it.
    #[cfg(unix)]
    #[tokio::test]
    async fn second_stop_payload_carries_stop_hook_active_true() {
        // The hook reads `stop_hook_active` from its stdin JSON and writes
        // it back to stderr verbatim, then `exit 2`s so the runner returns
        // a Block with that stderr as the reason. The Block also stays in
        // CancelPendingTurn — useful for asserting.
        //
        // Plain shell with `grep -oE` keeps the test free of jq/python.
        let cmd = r#"grep -oE '"stop_hook_active":(true|false)' >&2; exit 2"#;
        let idx = single_hook_index(HookEvent::Stop, cmd);
        let mut p = mk_plugin(idx);

        // First dispatch: latch starts false; payload carries `false`.
        let effs1 = p
            .on_event(HostEvent::TurnEnd {
                turn_id: 1,
                success: true,
            })
            .await
            .unwrap();
        assert!(
            effs1.iter().any(|e| matches!(
                e,
                Effect::CancelPendingTurn { reason }
                    if reason == r#""stop_hook_active":false"#
            )),
            "expected first-dispatch reason to confirm stop_hook_active=false; got {effs1:?}"
        );

        // Second dispatch (no intervening TurnStart): latch is true now.
        let effs2 = p
            .on_event(HostEvent::TurnEnd {
                turn_id: 2,
                success: true,
            })
            .await
            .unwrap();
        assert!(
            effs2.iter().any(|e| matches!(
                e,
                Effect::CancelPendingTurn { reason }
                    if reason == r#""stop_hook_active":true"#
            )),
            "expected second-dispatch reason to confirm stop_hook_active=true; got {effs2:?}"
        );
    }
}
