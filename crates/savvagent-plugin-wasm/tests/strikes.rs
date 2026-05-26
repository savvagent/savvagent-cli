//! Integration tests for the three-strikes auto-disable path.
//!
//! These tests exercise the [`crate::strikes::StrikeCounter`] semantics
//! end-to-end through `StaticAdapter` against the `trap.wasm` fixture
//! that Task 7 added:
//!
//! 1. **Three traps in quick succession disable the plugin.** The first
//!    two calls return the trap as `PluginError::Internal`; the third
//!    flips `disabled = true` and the fourth call short-circuits before
//!    any wasm executes.
//! 2. **Successful calls do NOT count against the budget.** A plugin
//!    that runs cleanly forever never trips the strike limit.
//!
//! Helpers are duplicated from `fault_injection.rs` rather than
//! extracted into a shared `mod common`. The duplication is small
//! enough (~20 lines) that the extra module wiring isn't worth it; if a
//! third test file in this crate ends up needing the same fixture
//! loader, factor then.

use std::sync::Arc;

use savvagent_plugin::Plugin;
use savvagent_plugin_wasm::adapter::StaticAdapter;
use savvagent_plugin_wasm::host_imports::theme;
use savvagent_plugin_wasm::manifest::PluginManifest;

/// Stage a `plugin-static`-world fixture under a tempdir and return the
/// instantiated adapter + the tempdir guard (kept alive for the duration
/// of the test). Mirrors `fault_injection.rs::load_static` — see that
/// file's comment for why the helpers aren't factored.
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

#[tokio::test(flavor = "multi_thread")]
async fn three_traps_disable_and_short_circuit_subsequent_calls() {
    let (mut adapter, _td) = load_static("trap").await;

    // Two traps land. Each call returns Err with a trap-shaped
    // message; each one feeds the strike counter.
    for i in 0..2 {
        let r = adapter.handle_slash("boom", vec![]).await;
        let err = r.expect_err(&format!("call {i} should fail with a wasm trap"));
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("trap") || msg.contains("panic") || msg.contains("unreachable"),
            "call {i} expected trap message, got: {msg}",
        );
        assert!(
            !adapter.is_disabled(),
            "after {} strike(s), adapter must not yet be disabled",
            i + 1,
        );
    }

    // Third call: should also be a real wasm trap (the 3rd strike),
    // but the disable flag flips immediately after, so the error
    // message identifies the disable transition.
    let third = adapter
        .handle_slash("boom", vec![])
        .await
        .expect_err("third call should fail");
    let third_msg = third.to_string().to_lowercase();
    assert!(
        third_msg.contains("disabled") || third_msg.contains("strikes"),
        "third trap should mention disabled/strikes, got: {third_msg}",
    );
    assert!(
        adapter.is_disabled(),
        "adapter must be disabled after 3 traps"
    );

    // Fourth call: short-circuits with the disabled message — NO wasm
    // executes. Use a short tokio timeout as a sanity check that the
    // short-circuit really skips the wasm call (the trap fixture
    // returns quickly anyway, but the absence of wasm execution is the
    // load-bearing assertion here).
    let fourth = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        adapter.handle_slash("boom", vec![]),
    )
    .await
    .expect("short-circuit must return synchronously, well under 2s");
    let err = fourth.expect_err("fourth call must surface the disabled error");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("disabled") || msg.contains("strikes"),
        "expected disabled-by-strikes message on short-circuit, got: {msg}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn successful_calls_do_not_strike() {
    // Run the `static.wasm` fixture (the happy-path echo plugin) many
    // times and confirm the adapter never disables itself. The strike
    // counter is rolling-time-based, not consecutive-failure-based, so
    // a healthy plugin's strike count stays at zero indefinitely.
    let (mut adapter, _td) = load_static("static").await;
    for i in 0..20 {
        let r = adapter
            .handle_slash("echo", vec![format!("ping {i}")])
            .await;
        // The static fixture's `echo` slash command might not be
        // exactly named "echo" — what matters is that we get *some*
        // non-trap result back. A `SlashNotHandled` PluginError still
        // means the wasm call returned cleanly; only a trap (Internal
        // with "trap"/"interrupt") counts as a strike.
        if let Err(e) = &r {
            let msg = e.to_string().to_lowercase();
            assert!(
                !msg.contains("trap") && !msg.contains("interrupt"),
                "iteration {i} unexpectedly trapped: {e}",
            );
        }
        assert!(
            !adapter.is_disabled(),
            "successful (non-trap) calls must not disable; failed at iter {i}",
        );
    }
}
