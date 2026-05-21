//! End-to-end resource notification test.
//!
//! Spawns the `resource-tool` fixture as the only stdio tool of a real
//! `Host`, drives a turn whose tool call invokes `trigger_update`, and
//! asserts the host surfaces a `TurnEvent::ResourceUpdated` for each of
//! the fixture's two `test://updated/payload-*` URIs.
//!
//! The fixture binary path is resolved by shelling out to `cargo build`
//! for the fixture's manifest and then probing the workspace's target
//! directory. Cargo doesn't expose `CARGO_BIN_EXE_<name>` for binaries
//! owned by `path` dev-dependencies (only for binaries owned by the
//! crate-under-test), so we explicitly build the fixture from the test
//! itself.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use savvagent_host::{
    Host, HostConfig, PermissionDecision, PermissionPolicy, ProviderEndpoint, ToolEndpoint,
    TurnEvent,
};
use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ContentBlock, ProviderError, StopReason, StreamEvent, Usage,
};
use tokio::sync::mpsc;

/// In-process provider that scripts a single `trigger_update` tool call
/// on the first iteration and `end_turn` on every subsequent iteration.
/// Mirrors the `ScriptedProvider` pattern used in `session.rs`'s
/// in-tree tests, but lives here so the test crate stays self-contained.
struct ScriptedToolUseProvider {
    calls: AtomicUsize,
}

impl ScriptedToolUseProvider {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ProviderClient for ScriptedToolUseProvider {
    async fn complete(
        &self,
        req: CompleteRequest,
        _events: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let (content, stop_reason) = if n == 0 {
            (
                vec![ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "trigger_update".into(),
                    input: serde_json::json!({}),
                }],
                StopReason::ToolUse,
            )
        } else {
            (
                vec![ContentBlock::Text {
                    text: "done".into(),
                }],
                StopReason::EndTurn,
            )
        };
        Ok(CompleteResponse {
            id: format!("msg-{n}"),
            model: req.model,
            content,
            stop_reason,
            stop_sequence: None,
            usage: Usage::default(),
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_publishes_resources_and_host_emits_turn_events() {
    let fixture_bin = build_fixture_binary();

    let project_root = tempfile::tempdir().expect("tempdir");
    let project_root_path = project_root.path().to_path_buf();

    let config = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "inproc://test".into(),
        },
        "test-model".to_string(),
    )
    .with_project_root(project_root_path.clone())
    .with_policy(PermissionPolicy::transient(project_root_path))
    .with_tool(ToolEndpoint::Stdio {
        command: fixture_bin,
        args: Vec::new(),
    });

    let provider: Box<dyn ProviderClient + Send + Sync> = Box::new(ScriptedToolUseProvider::new());
    let host = Host::with_components(config, provider)
        .await
        .expect("host starts with resource-tool fixture");

    // Default policy returns `Ask` for any tool we don't ship a built-in
    // verdict for. A streaming turn would block on the modal and dead-
    // lock the test runner, so pre-register Allow for `trigger_update`.
    host.add_session_rule(
        "trigger_update",
        &serde_json::json!({}),
        PermissionDecision::Allow,
    )
    .await;

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(128);
    let _outcome = host
        .run_turn_streaming("invoke trigger_update", tx)
        .await
        .expect("turn runs to completion");

    // `run_turn_streaming` returns only after the loop ends, but the
    // `resource_pump` task that converts `ResourceEvent`s into
    // `TurnEvent::ResourceUpdated` runs on a separate task. Drain
    // anything already queued, then keep recv-ing with a small overall
    // deadline so we don't race the pump's last flush.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut uris: Vec<String> = Vec::new();
    loop {
        // Stop once we've seen both URIs.
        if uris.iter().any(|u| u == "test://updated/payload-1")
            && uris.iter().any(|u| u == "test://updated/payload-2")
        {
            break;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(TurnEvent::ResourceUpdated { uri, .. })) => uris.push(uri),
            Ok(Some(_other)) => continue, // non-resource events: ignore.
            Ok(None) => break,            // channel closed.
            Err(_) => break,              // timed out waiting for the next event.
        }
    }

    uris.sort();
    uris.dedup();
    assert_eq!(
        uris,
        vec![
            "test://updated/payload-1".to_string(),
            "test://updated/payload-2".to_string(),
        ],
        "host must surface TurnEvent::ResourceUpdated for both fixture URIs"
    );
}

/// Build the `resource-tool` fixture binary (idempotent) and return its
/// absolute path. We can't rely on Cargo's `CARGO_BIN_EXE_<name>` env
/// var — that's only set for binaries owned by the crate-under-test,
/// not for binaries owned by a `path` dev-dependency — so we shell out
/// to `cargo build` ourselves. After a successful build, the binary
/// lives at `<workspace_target>/debug/resource-tool` (or the platform
/// `.exe` equivalent), which we locate by walking up from the running
/// test executable's directory.
fn build_fixture_binary() -> PathBuf {
    // CARGO_MANIFEST_DIR points at `crates/savvagent-host`. The fixture
    // manifest is at `<manifest_dir>/tests/fixtures/resource-tool/Cargo.toml`.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_manifest = manifest_dir
        .join("tests")
        .join("fixtures")
        .join("resource-tool")
        .join("Cargo.toml");
    assert!(
        fixture_manifest.exists(),
        "fixture manifest must exist at {fixture_manifest:?}"
    );

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(&cargo)
        .args([
            "build",
            "--quiet",
            "--bin",
            "resource-tool",
            "--manifest-path",
        ])
        .arg(&fixture_manifest)
        .status()
        .expect("invoke cargo build for resource-tool fixture");
    assert!(status.success(), "cargo build for resource-tool failed");

    // Locate the freshly-built binary. The test binary lives at
    // `<target>/debug/deps/resources_integration-<hash>`; the fixture's
    // binary lives at `<target>/debug/resource-tool` (the workspace
    // shares a single target dir across all bins).
    let test_bin = std::env::current_exe().expect("current_exe");
    let target_debug = ancestor_named(&test_bin, "debug").unwrap_or_else(|| {
        panic!(
            "current_exe path {test_bin:?} must have a `debug` ancestor; \
             test layouts other than the standard `target/debug/deps/<bin>` \
             aren't supported here"
        )
    });
    let bin_name = if cfg!(windows) {
        "resource-tool.exe"
    } else {
        "resource-tool"
    };
    let fixture_bin = target_debug.join(bin_name);
    assert!(
        fixture_bin.exists(),
        "resource-tool binary must exist at {fixture_bin:?} after cargo build"
    );
    fixture_bin
}

/// Return the first ancestor of `path` whose final component equals
/// `name`. Used to climb out of `target/debug/deps/` up to
/// `target/debug/`.
fn ancestor_named(path: &Path, name: &str) -> Option<PathBuf> {
    path.ancestors()
        .find(|p| p.file_name().and_then(|s| s.to_str()) == Some(name))
        .map(PathBuf::from)
}
