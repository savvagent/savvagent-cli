//! Hook outcome decision types + parser for the Claude-Code-compatible
//! structured-JSON stdout protocol.

use serde::Deserialize;

use crate::plugin::builtin::user_hooks::discovery::HookEvent;

/// Final per-hook outcome, after considering exit code AND any parsed
/// JSON on stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookDecision {
    /// Hook proceeded cleanly. `additional_context` is `Some` only for
    /// `UserPromptSubmit` returning `hookSpecificOutput.additionalContext`.
    Continue {
        additional_context: Option<String>,
        suppress_output: bool,
    },
    /// Hook blocked the chain. `reason` becomes the user-visible note.
    Block {
        reason: String,
        suppress_output: bool,
    },
}

#[derive(Debug, Deserialize, Default)]
struct StructuredOutput {
    #[serde(default, rename = "continue")]
    cont: Option<bool>,
    #[serde(default, rename = "stopReason")]
    stop_reason: Option<String>,
    #[serde(default, rename = "suppressOutput")]
    suppress_output: Option<bool>,
    #[serde(default, rename = "hookSpecificOutput")]
    hook_specific: Option<HookSpecific>,
    // Legacy fields.
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct HookSpecific {
    #[serde(default, rename = "hookEventName")]
    hook_event_name: Option<String>,
    #[serde(default, rename = "permissionDecision")]
    permission_decision: Option<String>,
    #[serde(default, rename = "permissionDecisionReason")]
    permission_decision_reason: Option<String>,
    #[serde(default, rename = "additionalContext")]
    additional_context: Option<String>,
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

/// Parse the hook's stdout AND combine with the exit code to produce
/// the final decision. Invalid JSON falls back to the exit-code-only
/// outcome. `warnings` collects any non-fatal anomalies (mismatched
/// `hookEventName`, unknown `permissionDecision`).
pub fn parse_outcome(
    event: HookEvent,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    warnings: &mut Vec<String>,
) -> HookDecision {
    // Try structured stdout first.
    let parsed: Option<StructuredOutput> = serde_json::from_str(stdout.trim()).ok();
    if let Some(p) = parsed {
        let cont = p.cont.unwrap_or(true);
        let suppress = p.suppress_output.unwrap_or(false);
        let mut additional: Option<String> = None;

        // Event-specific decision overrides via hookSpecificOutput.
        if let Some(hs) = p.hook_specific {
            if let Some(name) = &hs.hook_event_name {
                if name != event_name(event) {
                    warnings.push(format!(
                        "hookSpecificOutput.hookEventName `{name}` does not match firing event `{}`; ignoring hookSpecificOutput",
                        event_name(event)
                    ));
                } else {
                    match event {
                        HookEvent::PreToolUse => match hs.permission_decision.as_deref() {
                            Some("allow") => {
                                return HookDecision::Continue {
                                    additional_context: None,
                                    suppress_output: suppress,
                                };
                            }
                            Some("deny") => {
                                return HookDecision::Block {
                                    reason: hs
                                        .permission_decision_reason
                                        .unwrap_or_else(|| "denied by hook".into()),
                                    suppress_output: suppress,
                                };
                            }
                            Some("ask") => {
                                warnings.push(
                                    "permissionDecision=`ask` is not supported in v1; treating as `deny`"
                                        .into(),
                                );
                                return HookDecision::Block {
                                    reason: hs.permission_decision_reason.unwrap_or_else(|| {
                                        "ask requested (not supported in v1)".into()
                                    }),
                                    suppress_output: suppress,
                                };
                            }
                            Some(other) => {
                                warnings.push(format!(
                                    "unknown permissionDecision `{other}`; ignoring"
                                ));
                            }
                            None => {}
                        },
                        HookEvent::UserPromptSubmit => {
                            additional = hs.additional_context;
                        }
                        _ => {}
                    }
                }
            }
        }

        if !cont {
            let reason = p
                .stop_reason
                .or_else(|| p.reason.clone())
                .unwrap_or_else(|| "blocked by user hook".into());
            return HookDecision::Block {
                reason,
                suppress_output: suppress,
            };
        }
        // Legacy: decision=="block" / "approve"
        if let Some(d) = p.decision.as_deref() {
            warnings.push(
                "legacy `decision` field is deprecated; prefer `continue` + `stopReason` or `hookSpecificOutput`"
                    .into(),
            );
            if d == "block" {
                return HookDecision::Block {
                    reason: p.reason.unwrap_or_else(|| "blocked by user hook".into()),
                    suppress_output: suppress,
                };
            }
        }
        return HookDecision::Continue {
            additional_context: additional,
            suppress_output: suppress,
        };
    }
    // No structured stdout — exit-code-only.
    if exit_code == 2 {
        let reason = if stderr.trim().is_empty() {
            "blocked by user hook".to_string()
        } else {
            stderr.trim().to_string()
        };
        return HookDecision::Block {
            reason,
            suppress_output: false,
        };
    }
    HookDecision::Continue {
        additional_context: None,
        suppress_output: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_0_no_stdout_continues() {
        let mut w = Vec::new();
        let d = parse_outcome(HookEvent::Stop, 0, "", "", &mut w);
        assert!(matches!(d, HookDecision::Continue { .. }));
        assert!(w.is_empty());
    }

    #[test]
    fn exit_2_no_stdout_blocks_with_stderr() {
        let mut w = Vec::new();
        let d = parse_outcome(
            HookEvent::PreToolUse,
            2,
            "",
            "writes to .git/ forbidden\n",
            &mut w,
        );
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "writes to .git/ forbidden"),
            _ => panic!("expected Block"),
        }
    }

    #[test]
    fn structured_continue_false_blocks() {
        let mut w = Vec::new();
        let stdout = r#"{"continue":false,"stopReason":"nope"}"#;
        let d = parse_outcome(HookEvent::Stop, 0, stdout, "", &mut w);
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "nope"),
            _ => panic!(),
        }
    }

    #[test]
    fn permission_decision_allow_continues() {
        let mut w = Vec::new();
        let stdout =
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}"#;
        let d = parse_outcome(HookEvent::PreToolUse, 0, stdout, "", &mut w);
        assert!(matches!(d, HookDecision::Continue { .. }));
    }

    #[test]
    fn permission_decision_deny_blocks_with_reason() {
        let mut w = Vec::new();
        let stdout = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"path forbidden"}}"#;
        let d = parse_outcome(HookEvent::PreToolUse, 0, stdout, "", &mut w);
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "path forbidden"),
            _ => panic!(),
        }
    }

    #[test]
    fn permission_decision_ask_warns_and_blocks() {
        let mut w = Vec::new();
        let stdout =
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}"#;
        let d = parse_outcome(HookEvent::PreToolUse, 0, stdout, "", &mut w);
        assert!(matches!(d, HookDecision::Block { .. }));
        assert!(w.iter().any(|s| s.contains("ask")));
    }

    #[test]
    fn user_prompt_submit_additional_context_passes_through() {
        let mut w = Vec::new();
        let stdout = r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"extra"}}"#;
        let d = parse_outcome(HookEvent::UserPromptSubmit, 0, stdout, "", &mut w);
        match d {
            HookDecision::Continue {
                additional_context, ..
            } => {
                assert_eq!(additional_context.as_deref(), Some("extra"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn mismatched_hook_event_name_warns_and_ignores() {
        let mut w = Vec::new();
        let stdout =
            r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","permissionDecision":"deny"}}"#;
        let d = parse_outcome(HookEvent::PreToolUse, 0, stdout, "", &mut w);
        assert!(matches!(d, HookDecision::Continue { .. }));
        assert!(w.iter().any(|s| s.contains("PostToolUse")));
    }

    #[test]
    fn invalid_json_falls_back_to_exit_code() {
        let mut w = Vec::new();
        let d = parse_outcome(HookEvent::PreToolUse, 2, "{ not json", "stderr msg", &mut w);
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "stderr msg"),
            _ => panic!(),
        }
    }

    #[test]
    fn legacy_decision_block_warns() {
        let mut w = Vec::new();
        let stdout = r#"{"decision":"block","reason":"legacy"}"#;
        let d = parse_outcome(HookEvent::Stop, 0, stdout, "", &mut w);
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "legacy"),
            _ => panic!(),
        }
        assert!(w.iter().any(|s| s.contains("legacy")));
    }
}
