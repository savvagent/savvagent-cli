//! Spawns a shell hook, writes the JSON payload to its stdin, awaits
//! with timeout, and returns a `HookDecision`.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::plugin::builtin::user_hooks::decision::{HookDecision, parse_outcome};
use crate::plugin::builtin::user_hooks::discovery::HookEvent;

/// Run one hook command with the given stdin payload and timeout.
/// Returns `(decision, warnings, stdout, stderr)` so the caller can
/// surface stdout/stderr to the user as `PushNote`s when desired.
pub async fn run_one(
    event: HookEvent,
    command: &str,
    timeout_secs: u64,
    payload: &Value,
    project_root: &Path,
) -> (HookDecision, Vec<String>, String, String) {
    let mut warnings = Vec::new();
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("SAVVAGENT_PROJECT_DIR", project_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                HookDecision::Continue {
                    additional_context: None,
                    suppress_output: false,
                },
                vec![format!("hook `{command}`: spawn failed: {e}")],
                String::new(),
                String::new(),
            );
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let bytes = serde_json::to_vec(payload).unwrap_or_default();
        let _ = stdin.write_all(&bytes).await;
        let _ = stdin.shutdown().await;
    }

    let wait = child.wait_with_output();
    let output = match timeout(Duration::from_secs(timeout_secs), wait).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            warnings.push(format!("hook `{command}`: wait failed: {e}"));
            return (
                HookDecision::Continue {
                    additional_context: None,
                    suppress_output: false,
                },
                warnings,
                String::new(),
                String::new(),
            );
        }
        Err(_) => {
            warnings.push(format!("hook `{command}`: timed out after {timeout_secs}s"));
            return (
                HookDecision::Continue {
                    additional_context: None,
                    suppress_output: false,
                },
                warnings,
                String::new(),
                String::new(),
            );
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let exit_code = output.status.code().unwrap_or(-1);

    let decision = parse_outcome(event, exit_code, &stdout, &stderr, &mut warnings);

    // Convert exit-2 on non-block-capable events into a warning instead
    // of a block. Preserve the hook author's `suppress_output` flag so
    // explicit silence is honoured on the demoted Continue.
    let decision = match (event, &decision) {
        (
            HookEvent::PostToolUse,
            HookDecision::Block {
                suppress_output, ..
            },
        )
        | (
            HookEvent::SessionStart,
            HookDecision::Block {
                suppress_output, ..
            },
        ) => {
            warnings.push(format!(
                "hook `{command}` exited 2 on non-block-capable event {event:?}; treating as warning"
            ));
            HookDecision::Continue {
                additional_context: None,
                suppress_output: *suppress_output,
            }
        }
        _ => decision,
    };

    (decision, warnings, stdout, stderr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn payload() -> Value {
        json!({ "hook_event_name": "Stop" })
    }

    fn root() -> PathBuf {
        PathBuf::from("/tmp")
    }

    #[tokio::test]
    async fn exit_zero_continues() {
        let (d, w, _, _) = run_one(HookEvent::Stop, "true", 5, &payload(), &root()).await;
        assert!(matches!(d, HookDecision::Continue { .. }));
        assert!(w.is_empty());
    }

    #[tokio::test]
    async fn exit_2_blocks_with_stderr() {
        let (d, _w, _, _) = run_one(
            HookEvent::PreToolUse,
            "echo nope >&2; exit 2",
            5,
            &payload(),
            &root(),
        )
        .await;
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "nope"),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn timeout_warns_and_continues() {
        let (d, w, _, _) = run_one(HookEvent::Stop, "sleep 10", 1, &payload(), &root()).await;
        assert!(matches!(d, HookDecision::Continue { .. }));
        assert!(w.iter().any(|s| s.contains("timed out")));
    }

    #[tokio::test]
    async fn missing_binary_does_not_panic() {
        let (d, _w, _stdout, _stderr) = run_one(
            HookEvent::PreToolUse,
            "/no/such/binary",
            5,
            &payload(),
            &root(),
        )
        .await;
        // sh -c spawns sh fine; the inner command exits non-zero
        // (typically 127). That is non-blocking; chain continues.
        assert!(matches!(d, HookDecision::Continue { .. }));
    }

    #[tokio::test]
    async fn exit_2_on_session_start_warns_not_blocks() {
        let (d, w, _, _) = run_one(
            HookEvent::SessionStart,
            "echo bad >&2; exit 2",
            5,
            &payload(),
            &root(),
        )
        .await;
        assert!(matches!(d, HookDecision::Continue { .. }));
        assert!(w.iter().any(|s| s.contains("non-block-capable")));
    }

    #[tokio::test]
    async fn structured_stdout_takes_precedence() {
        let cmd = r#"echo '{"continue":false,"stopReason":"structured"}'; exit 0"#;
        let (d, _w, _, _) = run_one(HookEvent::Stop, cmd, 5, &payload(), &root()).await;
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "structured"),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn project_dir_env_is_set() {
        let (_d, _w, stdout, _stderr) = run_one(
            HookEvent::Stop,
            r#"echo "$SAVVAGENT_PROJECT_DIR""#,
            5,
            &payload(),
            &root(),
        )
        .await;
        assert!(stdout.trim() == "/tmp" || stdout.trim().ends_with("/tmp"));
    }
}
