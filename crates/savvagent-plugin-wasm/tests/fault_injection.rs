//! Fault-injection integration tests for the wasm plugin adapters.
//!
//! Each test loads one of Task 7's four fault fixtures (`trap.wasm`,
//! `timeout.wasm`, `denied-host.wasm`, `denied-account.wasm`) and asserts
//! that the host adapter surfaces the corresponding failure mode through
//! its public trait surface — `PluginError` for the static adapter,
//! `ProviderError` for the provider adapter.
//!
//! ## Why fault fixtures live in their own file
//!
//! `static_adapter.rs`, `interactive_adapter.rs`, and `provider_adapter.rs`
//! exercise the happy paths against a single fixture each. The four fault
//! fixtures all fail-deliberately, so co-locating them here keeps the
//! happy-path tests clean while making the failure-mode coverage easy to
//! audit in one file.
//!
//! ## What's tested, what isn't
//!
//! - **trap**: `handle_slash("boom", ..)` emits a wasm `unreachable`
//!   instruction. `StaticAdapter` must surface it as
//!   `PluginError::Internal(...)` containing the trap reason; we assert
//!   on the case-insensitive substring set "trap" / "panic" /
//!   "unreachable".
//! - **timeout**: `handle_slash("forever", ..)` busy-loops indefinitely.
//!   This test is `#[ignore]`-marked in v0.18.0 because the engine does
//!   NOT enable `epoch_interruption` — without it, an async wasm call
//!   never yields and `tokio::time::timeout` cannot cancel it (the host
//!   future is never re-polled long enough to observe cancellation).
//!   Task 8 will land `epoch_interruption(true)` + an epoch-bump driver,
//!   at which point this test flips on as part of the normal suite. The
//!   fixture and test body are in place so Task 8 only has to flip the
//!   `#[ignore]` off after wiring the interrupt path.
//! - **denied-host**: the provider plugin calls
//!   `http.fetch("https://evil.example/x")` against a manifest whose
//!   `[security] allowed-hosts` is `["api.example.com"]`. The host
//!   denies; the fixture surfaces the denial as a `ProviderError`
//!   whose message mentions "DeniedHost" + the rejected host string.
//! - **denied-account**: the provider plugin calls
//!   `keyring.get("not-listed")` against a manifest whose
//!   `[security] keyring-accounts` is `["allowed"]`. The host denies
//!   without touching any real OS keyring backend; the fixture surfaces
//!   the denial as a `ProviderError` whose message mentions "Denied"
//!   + the account name.
//!
//! Deferred to a later iteration: **bad-export** (manifest declares an
//! export the wasm doesn't actually have). The loader doesn't
//! cross-check disk-manifest exports against actual wasm exports today;
//! the cross-check belongs in a post-v0.18.0 iteration. See the
//! `manifest.rs` comment for the marker.

use std::sync::Arc;
use std::time::{Duration, Instant};

use savvagent_mcp::ProviderClient;
use savvagent_plugin::Plugin;
use savvagent_plugin_wasm::adapter::{StaticAdapter, WasmProviderClient};
use savvagent_plugin_wasm::host_imports::theme;
use savvagent_plugin_wasm::manifest::PluginManifest;
use savvagent_protocol::{CompleteRequest, ContentBlock, Message, Role};

/// Stage a `plugin-static`-world fixture under a tempdir and return the
/// instantiated adapter + the tempdir guard (kept alive for the duration
/// of the test).
async fn load_static(name: &str) -> (StaticAdapter, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let toml = format!(
        r#"
[plugin]
id = "fixture.{name}"
name = "fixture-{name}"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#
    );
    std::fs::write(tmp.path().join("plugin.toml"), toml).expect("write plugin.toml");

    let wasm_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(format!("{name}.wasm"));
    std::fs::copy(&wasm_src, tmp.path().join("plugin.wasm")).expect("copy wasm fixture");

    let dm = Arc::new(
        PluginManifest::load(&tmp.path().join("plugin.toml"), &format!("fixture.{name}"))
            .expect("parse plugin.toml"),
    );
    let theme = theme::provider(Vec::new());
    let adapter = StaticAdapter::new(dm, tmp.path(), theme)
        .await
        .expect("StaticAdapter::new");
    (adapter, tmp)
}

/// Stage a `plugin-provider`-world fixture with the given `allowed-hosts`
/// + `keyring-accounts` security lists.
async fn load_provider_with_security(
    name: &str,
    allowed_hosts: &[&str],
    keyring_accounts: &[&str],
) -> (WasmProviderClient, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let hosts_toml = allowed_hosts
        .iter()
        .map(|h| format!("\"{h}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let accts_toml = keyring_accounts
        .iter()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let toml = format!(
        r#"
[plugin]
id = "fixture.{name}"
name = "fixture-{name}"
version = "0.1.0"
world = "plugin-provider"
savvagent = "^0.18"

[exports]
provider-id = "fixture-{name}"

[security]
allowed-hosts = [{hosts_toml}]
keyring-accounts = [{accts_toml}]
"#
    );
    std::fs::write(tmp.path().join("plugin.toml"), toml).expect("write plugin.toml");

    let wasm_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(format!("{name}.wasm"));
    std::fs::copy(&wasm_src, tmp.path().join("plugin.wasm")).expect("copy wasm fixture");

    let dm = Arc::new(
        PluginManifest::load(&tmp.path().join("plugin.toml"), &format!("fixture.{name}"))
            .expect("parse plugin.toml"),
    );
    let client = WasmProviderClient::new(dm, tmp.path())
        .await
        .expect("WasmProviderClient::new");
    (client, tmp)
}

/// Canned `CompleteRequest` reused by every provider fault test. The
/// fixtures ignore the request fields and always fail; this is just a
/// well-formed body that crosses the WIT boundary cleanly.
fn canned_complete_request() -> CompleteRequest {
    CompleteRequest {
        model: "fixture-model-1".into(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "ping".into(),
            }],
        }],
        system: None,
        tools: vec![],
        temperature: None,
        top_p: None,
        max_tokens: 16,
        stop_sequences: vec![],
        stream: false,
        thinking: None,
        metadata: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn trap_surfaces_as_plugin_error() {
    // The static adapter must convert a wasm `unreachable` trap into
    // `PluginError::Internal(...)` without crashing the host. Stringify
    // the error and assert on a stable substring — wasmtime's exact
    // wording ("wasm `unreachable` instruction executed") can drift,
    // but any of "trap" / "panic" / "unreachable" should always
    // appear.
    let (mut adapter, _td) = load_static("trap").await;
    let err = adapter
        .handle_slash("boom", Vec::new())
        .await
        .expect_err("handle_slash('boom') must trap");

    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("trap") || msg.contains("panic") || msg.contains("unreachable"),
        "expected trap/panic/unreachable in error string, got: {msg}",
    );
}

/// `timeout.wasm` is shipped and active in v0.18.0 — Task 8 enabled the
/// engine's epoch interruption and wired per-call deadlines, so the
/// fixture's `handle_slash("forever", ..)` busy-loop now traps with
/// `Trap::Interrupt` after `runtime.call_timeout_ms / EPOCH_TICK` ticks.
///
/// We don't set a custom `[runtime] call-timeout-ms` in the test
/// manifest, so the default 5s kicks in: the call should trap within
/// 5-6 seconds (default + one bumper-tick worst-case). We wrap the call
/// in a generous 15-second `tokio::time::timeout` purely as a CI safety
/// net so a regression in the epoch-bumper doesn't hang the whole
/// test binary.
#[tokio::test(flavor = "multi_thread")]
async fn timeout_can_be_cancelled_by_host() {
    let (mut adapter, _td) = load_static("timeout").await;

    let start = Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(15),
        adapter.handle_slash("forever", Vec::new()),
    )
    .await;
    let elapsed = start.elapsed();

    // The host-side timeout must not fire: the epoch-bumper trap is
    // expected to surface as a `PluginError::Internal` before 15s
    // elapses.
    let inner = outcome.expect("host-side safety-net timeout must not fire");
    let err = inner.expect_err("forever-loop must trap, not return Ok");

    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("trap") || msg.contains("interrupt") || msg.contains("epoch"),
        "expected trap/interrupt/epoch substring in error string, got: {msg}",
    );

    // Default `call-timeout-ms` is 5000; the bumper runs every 100ms.
    // Worst case the trap lands at `5000 + 100 = 5100ms`. A generous
    // 12s upper bound catches regressions without being flaky on slow
    // runners.
    assert!(
        elapsed < Duration::from_secs(12),
        "wasm trap should fire near call_timeout_ms (5s) + 1 tick; got {elapsed:?}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn denied_host_returns_capability_denied() {
    // Manifest's `allowed-hosts` is ["api.example.com"]; the fixture
    // calls `http.fetch("https://evil.example/x")`. The host must
    // reject before any network I/O; the fixture surfaces the
    // `HttpError::DeniedHost(...)` into a `ProviderError` whose message
    // we can match on.
    let (client, _td) =
        load_provider_with_security("denied-host", &["api.example.com"], &["allowed"]).await;

    let err = client
        .complete(canned_complete_request(), None)
        .await
        .expect_err("complete() must surface the denied-host error");

    let dbg = format!("{err:?}").to_lowercase();
    assert!(
        dbg.contains("denied") || dbg.contains("evil") || dbg.contains("host"),
        "expected denied/evil/host substring in ProviderError, got: {dbg}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn denied_account_returns_capability_denied() {
    // Manifest's `keyring-accounts` is ["allowed"]; the fixture calls
    // `keyring.get("not-listed")`. The host's allow-list check rejects
    // *before* any OS keyring backend is consulted, so this test runs
    // safely on CI runners without a configured Secret Service / D-Bus
    // session. The fixture surfaces the `KeyringError::Denied(...)`
    // into a `ProviderError` whose message we can match on.
    let (client, _td) = load_provider_with_security(
        "denied-account",
        &["api.example.com"],
        &["allowed"], // does NOT include "not-listed"
    )
    .await;

    let err = client
        .complete(canned_complete_request(), None)
        .await
        .expect_err("complete() must surface the denied-account error");

    let dbg = format!("{err:?}").to_lowercase();
    assert!(
        dbg.contains("denied") || dbg.contains("not-listed") || dbg.contains("keyring"),
        "expected denied/not-listed/keyring substring in ProviderError, got: {dbg}",
    );
}
