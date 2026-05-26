//! End-to-end integration tests for the static-world adapter.
//!
//! Each test loads `tests/fixtures/static.wasm` (a real wasm component
//! built from `tests/fixtures-src/static`), instantiates it via
//! `StaticAdapter::new`, and exercises one trait method. The fixture lives
//! in source form alongside this test so it can be rebuilt via
//! `just build-fixtures`; the binary is committed so day-to-day `cargo
//! test` doesn't require the wasm toolchain.

use std::sync::Arc;

use savvagent_plugin::{Effect, HostEvent, Plugin, ProviderId};
use savvagent_plugin_wasm::adapter::StaticAdapter;
use savvagent_plugin_wasm::host_imports::theme;
use savvagent_plugin_wasm::manifest::PluginManifest;

const PLUGIN_TOML: &str = r#"
[plugin]
id = "fixture.static"
name = "fixture-static"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#;

/// Stage the fixture's `plugin.toml` and `plugin.wasm` under a temp dir.
/// Returns `(dir, parsed manifest)`. The dir is kept alive for the
/// duration of the test through the `_dir` binding the caller pins.
fn stage_fixture(_dir: &tempfile::TempDir) -> Arc<PluginManifest> {
    let plugin_dir = _dir.path();
    std::fs::write(plugin_dir.join("plugin.toml"), PLUGIN_TOML).expect("write plugin.toml");
    let wasm_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("static.wasm");
    std::fs::copy(&wasm_src, plugin_dir.join("plugin.wasm")).expect("copy static.wasm");
    Arc::new(
        PluginManifest::load(&plugin_dir.join("plugin.toml"), "fixture.static")
            .expect("parse plugin.toml"),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn static_adapter_handle_slash_echo() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let theme = theme::provider(Vec::new());
    let mut adapter = StaticAdapter::new(dm, tmp.path(), theme)
        .await
        .expect("StaticAdapter::new");

    let effects = adapter
        .handle_slash("echo", vec!["hello".into(), "world".into()])
        .await
        .expect("handle_slash");
    assert_eq!(effects.len(), 1, "fixture emits exactly one PushNote");
    match &effects[0] {
        Effect::PushNote { line } => {
            assert_eq!(line.spans.len(), 1);
            assert_eq!(line.spans[0].text, "hello world");
        }
        other => panic!("expected PushNote, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn static_adapter_handle_slash_unknown_returns_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let theme = theme::provider(Vec::new());
    let mut adapter = StaticAdapter::new(dm, tmp.path(), theme)
        .await
        .expect("StaticAdapter::new");

    let effects = adapter
        .handle_slash("unknown", Vec::new())
        .await
        .expect("handle_slash");
    assert!(
        effects.is_empty(),
        "unknown slash returns no effects from fixture"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn static_adapter_on_event_returns_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let theme = theme::provider(Vec::new());
    let mut adapter = StaticAdapter::new(dm, tmp.path(), theme)
        .await
        .expect("StaticAdapter::new");

    let effects = adapter
        .on_event(HostEvent::TurnStart { turn_id: 1 })
        .await
        .expect("on_event");
    assert!(effects.is_empty());

    // Also exercise a different variant to confirm the JSON-encoded
    // payload doesn't trap.
    let pid = ProviderId::new("anthropic").expect("valid id");
    let effects = adapter
        .on_event(HostEvent::ProviderRegistered {
            id: pid,
            display_name: "Anthropic".into(),
        })
        .await
        .expect("on_event");
    assert!(effects.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn static_adapter_manifest_caches_and_reports_runtime_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let theme = theme::provider(Vec::new());
    let adapter = StaticAdapter::new(dm, tmp.path(), theme)
        .await
        .expect("StaticAdapter::new");

    // First call lands the cache.
    let m1 = adapter.manifest();
    // `<org>.<name>` ⇒ `<org>:<name>` per `convert::disk_id_to_plugin_id`.
    assert_eq!(m1.id.as_str(), "fixture:static");
    assert_eq!(m1.name, "fixture-static");
    assert_eq!(m1.version, "0.1.0");
    assert_eq!(m1.contributions.slash_commands.len(), 1);
    assert_eq!(m1.contributions.slash_commands[0].name, "echo");
    assert_eq!(m1.contributions.hooks.len(), 1);

    // Second call returns the same data without re-entering wasm. The
    // contract is "cheap and side-effect-free"; clone-equality is the
    // observable signature.
    let m2 = adapter.manifest();
    assert_eq!(m1.id, m2.id);
    assert_eq!(m1.name, m2.name);
}

#[tokio::test(flavor = "multi_thread")]
async fn static_adapter_themes_is_empty_for_fixture() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let theme = theme::provider(Vec::new());
    let adapter = StaticAdapter::new(dm, tmp.path(), theme)
        .await
        .expect("StaticAdapter::new");
    assert!(adapter.themes().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn static_adapter_handle_slash_reuses_instance_across_calls() {
    // The adapter caches a single Store + PluginStatic; sequential
    // handle_slash calls must reuse them rather than re-instantiating per
    // call. Test by issuing two calls and verifying both return the
    // expected echo response (a re-instantiated adapter would still pass
    // this test, but a *broken* one that dropped the instance would surface
    // as a wasm trap on the second call).
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let theme = theme::provider(Vec::new());
    let mut adapter = StaticAdapter::new(dm, tmp.path(), theme)
        .await
        .expect("StaticAdapter::new");

    let first = adapter
        .handle_slash("echo", vec!["one".into()])
        .await
        .expect("first call");
    let second = adapter
        .handle_slash("echo", vec!["two".into()])
        .await
        .expect("second call");

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    match (&first[0], &second[0]) {
        (Effect::PushNote { line: a }, Effect::PushNote { line: b }) => {
            assert_eq!(a.spans[0].text, "one");
            assert_eq!(b.spans[0].text, "two");
        }
        other => panic!("expected two PushNote effects, got {other:?}"),
    }
}
