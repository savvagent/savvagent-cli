//! End-to-end integration tests for the provider-world adapter.
//!
//! Each test loads `tests/fixtures/provider.wasm` (a real wasm component
//! built from `tests/fixtures-src/provider`), instantiates it via
//! [`WasmProviderClient::new`], and exercises one `ProviderClient`
//! method. The fixture lives in source form alongside this test so it can
//! be rebuilt via `just build-fixtures`; the binary is committed so
//! day-to-day `cargo test` doesn't require the wasm toolchain.
//!
//! Every test uses `#[tokio::test]` (single-thread is fine; the provider
//! adapter doesn't use `block_in_place` because `ProviderClient`'s trait
//! surface is already fully async).
//!
//! Task 7 will add denied-host / denied-account fault tests via dedicated
//! fault fixtures; here we focus on the **success path** — `list_models`,
//! `complete` (with and without streaming), and `count_tokens`.

use std::sync::Arc;

use savvagent_mcp::ProviderClient;
use savvagent_plugin_wasm::adapter::WasmProviderClient;
use savvagent_plugin_wasm::adapter::provider::CountTokensRequest;
use savvagent_plugin_wasm::manifest::PluginManifest;
use savvagent_protocol::{CompleteRequest, ContentBlock, Message, Role, StreamEvent};
use tokio::sync::mpsc;

const PLUGIN_TOML: &str = r#"
[plugin]
id = "fixture.provider"
name = "fixture-provider"
version = "0.1.0"
world = "plugin-provider"
savvagent = "^0.18"

[exports]
provider-id = "fixture"

[security]
allowed-hosts = ["127.0.0.1"]
keyring-accounts = ["fixture"]
"#;

/// Stage the fixture's `plugin.toml` and `plugin.wasm` under a temp dir.
fn stage_fixture(dir: &tempfile::TempDir) -> Arc<PluginManifest> {
    let plugin_dir = dir.path();
    std::fs::write(plugin_dir.join("plugin.toml"), PLUGIN_TOML).expect("write plugin.toml");
    let wasm_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("provider.wasm");
    std::fs::copy(&wasm_src, plugin_dir.join("plugin.wasm")).expect("copy provider.wasm");
    Arc::new(
        PluginManifest::load(&plugin_dir.join("plugin.toml"), "fixture.provider")
            .expect("parse plugin.toml"),
    )
}

/// One canned request reused across tests. The fixture echoes back the
/// requested model id, so the asserts can confirm the request actually
/// crossed the WIT boundary.
fn canned_request() -> CompleteRequest {
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

#[tokio::test]
async fn provider_list_models_returns_fixture_model() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let client = WasmProviderClient::new(dm, tmp.path())
        .await
        .expect("WasmProviderClient::new");

    let resp = client.list_models().await.expect("list_models");
    assert_eq!(resp.models.len(), 1, "fixture advertises one model");
    assert_eq!(resp.models[0].id, "fixture-model-1");
    assert_eq!(
        resp.models[0].display_name.as_deref(),
        Some("Fixture Model")
    );
    assert_eq!(resp.models[0].context_window, Some(4096));
    assert_eq!(resp.default_model_id.as_deref(), Some("fixture-model-1"));
}

#[tokio::test]
async fn provider_complete_emits_event_and_returns_response() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let client = WasmProviderClient::new(dm, tmp.path())
        .await
        .expect("WasmProviderClient::new");

    let (tx, mut rx) = mpsc::channel(8);
    let resp = client
        .complete(canned_request(), Some(tx))
        .await
        .expect("complete");

    // Response shape: model echoed, single Text block with "hi",
    // stop_reason = end_turn.
    assert_eq!(resp.model, "fixture-model-1");
    assert_eq!(resp.content.len(), 1);
    match &resp.content[0] {
        ContentBlock::Text { text } => assert_eq!(text, "hi"),
        other => panic!("expected Text block, got {other:?}"),
    }
    assert_eq!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn,
        "fixture returns end_turn"
    );

    // Stream channel: fixture emits exactly one ContentBlockDelta.
    let evt = rx.recv().await.expect("at least one stream event");
    match evt {
        StreamEvent::ContentBlockDelta {
            index,
            delta: savvagent_protocol::BlockDelta::TextDelta { text },
        } => {
            assert_eq!(index, 0);
            assert_eq!(text, "hi");
        }
        other => panic!("expected ContentBlockDelta::TextDelta, got {other:?}"),
    }

    // After the call returns, the sender is dropped — the receiver
    // should observe `None` rather than block. (Sender lives inside the
    // per-call Store, which is dropped when complete returns.)
    let next = rx.recv().await;
    assert!(
        next.is_none(),
        "no more events expected after complete returns"
    );
}

#[tokio::test]
async fn provider_complete_without_emitter_returns_response() {
    // Same fixture, no stream channel attached: the response must still
    // come back, and the fixture's `emit-stream-event` call inside the
    // wasm must not surface as a host-side error (it should silently
    // drop, per `ProgressState::disabled`).
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let client = WasmProviderClient::new(dm, tmp.path())
        .await
        .expect("WasmProviderClient::new");

    let resp = client
        .complete(canned_request(), None)
        .await
        .expect("complete without emitter");
    assert_eq!(resp.model, "fixture-model-1");
    assert_eq!(resp.content.len(), 1);
}

#[tokio::test]
async fn provider_count_tokens_returns_canned_response() {
    // `count-tokens` is an inherent method on `WasmProviderClient`, not
    // on the `ProviderClient` trait surface. It exists so the host can
    // call into the plugin's token estimator if it wants to; the
    // fixture returns a fixed `input-tokens = 7` so the host can assert
    // the round-trip works.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let client = WasmProviderClient::new(dm, tmp.path())
        .await
        .expect("WasmProviderClient::new");

    let resp = client
        .count_tokens(CountTokensRequest {
            model: "fixture-model-1".into(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "ping".into(),
                }],
            }],
        })
        .await
        .expect("count_tokens");
    assert_eq!(resp.input_tokens, 7);
}

#[tokio::test]
async fn provider_disk_manifest_exposes_provider_id() {
    // The adapter caches the manifest at construction; the
    // `disk_manifest()` accessor surfaces the parsed `[exports]
    // provider-id` Task 9's `PROVIDERS` extender will read.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let client = WasmProviderClient::new(dm, tmp.path())
        .await
        .expect("WasmProviderClient::new");
    let manifest = client.disk_manifest();
    assert_eq!(manifest.plugin.id, "fixture.provider");
    assert_eq!(manifest.exports.provider_id.as_deref(), Some("fixture"));
}

#[tokio::test]
async fn provider_two_sequential_completes_succeed() {
    // The adapter constructs a fresh Store per call — exercise the
    // path that this doesn't leak resources or fail-on-second.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let client = WasmProviderClient::new(dm, tmp.path())
        .await
        .expect("WasmProviderClient::new");

    let r1 = client
        .complete(canned_request(), None)
        .await
        .expect("first");
    let r2 = client
        .complete(canned_request(), None)
        .await
        .expect("second");
    assert_eq!(r1.model, "fixture-model-1");
    assert_eq!(r2.model, "fixture-model-1");
}
