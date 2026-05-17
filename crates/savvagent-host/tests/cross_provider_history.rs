//! End-to-end test for Phase 3 cross-provider history namespacing.
//!
//! Sets up a `Host` with two registered providers (Anthropic + Gemini) that
//! point at locally-spawned axum fake-vendor servers (modeled on the Phase 2
//! `support` module). Turn 1 routes to Gemini via the `@gemini` prefix and
//! the fake Gemini server returns a `functionCall` part on its first call —
//! the host's translator surfaces it as a `ToolUse` block, runs the tool
//! (the in-test tool registry has no `list_dir` server, so the call fails
//! with `"unknown tool: list_dir"` and the host synthesizes a `ToolResult`
//! with `is_error: true`) and the conversation iterates. The fake Gemini
//! server's second call returns plain text, ending turn 1.
//!
//! Turn 2 routes to Anthropic via `@anthropic` and we assert what the
//! Anthropic fake **received**: its request body must include the Gemini-
//! prefixed `tool_use_id` (the synthesized + namespaced id from turn 1)
//! in **both** the `tool_use.id` slot and the `tool_result.tool_use_id`
//! slot. That round-trip preservation is the Phase 3 namespacing
//! contract.
//!
//! ## Tool-execution strategy
//!
//! Strategy (B) from the Task 11 plan: the host's tool registry is empty,
//! so the requested `list_dir` call falls through to `"unknown tool"` and
//! the host produces a `ToolResult { is_error: true }`. The conversation
//! continues to the next iteration where Gemini returns plain text. The
//! test only cares about the id round-trip, not the tool's success.
//! Adding a real tool server would add a binary-spawn dependency without
//! strengthening the assertion under test.
//!
//! ## Non-streaming path
//!
//! The fake servers only speak JSON (not SSE), so the test drives the
//! host via `run_turn` (non-streaming) rather than `run_turn_streaming`.
//! Routing, prefix parsing, ID namespacing, and history transit still
//! run as they would in production — only the token-delta forwarder is
//! skipped. The `RouteSelected` event is not asserted because the
//! captured-body inspector already proves cross-provider history flowed
//! through end-to-end.

mod support;

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use savvagent_host::{
    Host, HostConfig, PermissionDecision, PermissionPolicy, ProviderEndpoint, ProviderRegistration,
    StartupConnectPolicy,
    capabilities::{CostTier, ModelCapabilities, ProviderCapabilities},
};
use savvagent_mcp::{InProcessProviderClient, ProviderClient};
use savvagent_protocol::{ContentBlock, ProviderId};
use serde_json::{Value, json};
use std::sync::Mutex;

use support::{
    FakeState, anthropic_body_has_foreign_id, anthropic_success_response, spawn_fake_anthropic,
};

/// Two-call sequence for a fake Gemini server: first call returns the
/// `functionCall` tool-use response; every subsequent call returns plain
/// text so the second iteration of turn 1 terminates with `end_turn`.
#[derive(Clone)]
struct SequencedGeminiState {
    /// Number of requests received so far. Used to pick which canned
    /// response to return.
    calls: Arc<Mutex<u32>>,
    /// First-call response (carries the `functionCall` part).
    first: Value,
    /// Second-and-later response (plain text, `end_turn`-equivalent).
    rest: Value,
}

impl SequencedGeminiState {
    fn new(first: Value, rest: Value) -> Self {
        Self {
            calls: Arc::new(Mutex::new(0)),
            first,
            rest,
        }
    }
}

async fn sequenced_gemini_handler(
    state: State<SequencedGeminiState>,
    Json(_body): Json<Value>,
) -> Response {
    let response = {
        let mut calls = state.0.calls.lock().unwrap();
        let n = *calls;
        *calls += 1;
        if n == 0 {
            state.0.first.clone()
        } else {
            state.0.rest.clone()
        }
    };
    (StatusCode::OK, Json(response)).into_response()
}

/// Spin a fake Gemini server that hands out `first` on the first request
/// and `rest` on every subsequent request. Mirrors the route table used
/// by `support::spawn_fake_gemini` so the provider's HTTP client finds it.
async fn spawn_sequenced_gemini(first: Value, rest: Value) -> String {
    let state = SequencedGeminiState::new(first, rest);
    let app = Router::new()
        .route(
            "/v1beta/models/{model_with_action}",
            post(sequenced_gemini_handler),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("sequenced fake gemini died: {e}");
        }
    });
    format!("http://{addr}")
}

/// Gemini success response with a single `functionCall` part. The host's
/// Gemini translator surfaces this as a `ContentBlock::ToolUse` (see
/// `crates/provider-gemini/src/translate.rs::part_to_spp`), and the
/// namespacing layer in `run_turn_inner` rewrites the synthesized id to
/// carry the `gemini:` prefix before commit.
///
/// Note on `tool_id`: Gemini's API has no id field on `functionCall`, so
/// the SPP-side id is synthesized by `synthesize_tool_use_id(name, idx)`
/// (= `"gemini-list_dir-0"`). The host then re-namespaces it to
/// `"gemini:gemini-list_dir-0"` — NOT to the `"toolu_abc_123"` literal
/// the plan template suggested. The test below adapts: it asserts the
/// id round-trips with the **actual** synthesized form, not a contrived
/// one.
fn gemini_tool_use_response(tool_name: &str) -> Value {
    json!({
        "responseId": "gem_test_tool_0",
        "modelVersion": "gemini-test",
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{
                    "functionCall": {
                        "name": tool_name,
                        "args": { "path": "." }
                    }
                }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 1,
            "totalTokenCount": 11
        }
    })
}

/// Gemini success response with a plain text part (no `functionCall`).
/// Used for the second iteration of turn 1, where the host has sent the
/// tool_result back and Gemini terminates with text.
fn gemini_text_response() -> Value {
    json!({
        "responseId": "gem_test_done_0",
        "modelVersion": "gemini-test",
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{ "text": "I found Cargo.toml and src." }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 12,
            "candidatesTokenCount": 6,
            "totalTokenCount": 18
        }
    })
}

fn caps(model: &str) -> ProviderCapabilities {
    ProviderCapabilities::new(
        vec![ModelCapabilities {
            id: model.into(),
            display_name: model.into(),
            supports_vision: false,
            supports_audio: false,
            context_window: 0,
            cost_tier: CostTier::Standard,
        }],
        model.into(),
    )
    .expect("valid caps")
}

async fn build_two_provider_host(anth_base: String, gemini_base: String) -> Host {
    let anth_id = ProviderId::new("anthropic").unwrap();
    let gem_id = ProviderId::new("gemini").unwrap();

    let anth_handler = Arc::new(provider_anthropic::provider_for_tests(anth_base));
    let gem_handler = Arc::new(provider_gemini::provider_for_tests(gemini_base));

    let anth_client: Arc<dyn ProviderClient + Send + Sync> =
        Arc::new(InProcessProviderClient::new(anth_handler));
    let gem_client: Arc<dyn ProviderClient + Send + Sync> =
        Arc::new(InProcessProviderClient::new(gem_handler));

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "claude-test",
    );
    // Use a transient permissions policy so the test never touches
    // `~/.savvagent/permissions.toml`.
    let project_root = std::env::temp_dir().join("savvagent-cross-provider-history-test");
    cfg.policy = Some(PermissionPolicy::transient(project_root.clone()));
    cfg.project_root = project_root;
    cfg.providers = vec![
        ProviderRegistration::new(anth_id, "Anthropic", anth_client, caps("claude-test")),
        ProviderRegistration::new(gem_id, "Gemini", gem_client, caps("gemini-test")),
    ];
    cfg.startup_connect = StartupConnectPolicy::All;

    Host::start(cfg).await.expect("host starts")
}

#[tokio::test]
async fn cross_provider_history_namespaces_tool_use_id() {
    // -- Spin the fake Gemini server: first call -> tool_use, then text.
    let gemini_base =
        spawn_sequenced_gemini(gemini_tool_use_response("list_dir"), gemini_text_response()).await;

    // -- Spin the fake Anthropic server (always text).
    let anth_state = FakeState::new(anthropic_success_response());
    let anth_base = spawn_fake_anthropic(&anth_state).await;

    // -- Build a host that knows about both.
    let host = build_two_provider_host(anth_base, gemini_base).await;

    // -- Pre-register Allow for list_dir; without this, the streaming
    //    test would park on a permission prompt. The host's empty tool
    //    registry still returns "unknown tool" — that's fine; the host
    //    synthesizes an error tool_result and the conversation continues.
    //    See `feedback_streaming_test_permissions.md`.
    host.add_session_rule(
        "list_dir",
        &json!({ "path": "." }),
        PermissionDecision::Allow,
    )
    .await;

    // -- Turn 1: route to Gemini via @-prefix. Gemini emits a functionCall
    //    on iteration 1, the host runs (and errors) the tool, then Gemini
    //    returns plain text on iteration 2 and the turn ends.
    //    Non-streaming (`run_turn`) so the fake server can speak JSON.
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        host.run_turn("@gemini list the cwd"),
    )
    .await
    .expect("turn 1 did not hang")
    .expect("turn 1 completes");
    assert!(
        outcome.iterations >= 2,
        "expected at least 2 iterations on turn 1 (tool_use then end_turn), got {}",
        outcome.iterations
    );

    // -- Sanity: history must contain a `gemini:`-prefixed ToolUse id.
    //    Gemini's API has no id field, so the SPP id is synthesized by
    //    `synthesize_tool_use_id("list_dir", 0)` = `"gemini-list_dir-0"`,
    //    and the host's namespacing layer prefixes it with `"gemini:"`.
    let history = host.messages().await;
    let foreign_id = history
        .iter()
        .find_map(|m| {
            m.content.iter().find_map(|b| match b {
                ContentBlock::ToolUse { id, .. } if id.starts_with("gemini:") => Some(id.clone()),
                _ => None,
            })
        })
        .unwrap_or_else(|| {
            panic!(
                "history must contain a gemini-prefixed ToolUse id after turn 1; got {history:#?}"
            )
        });

    // -- Turn 2: route to Anthropic via @-prefix.
    let _outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        host.run_turn("@anthropic what did you find?"),
    )
    .await
    .expect("turn 2 did not hang")
    .expect("turn 2 completes");

    // -- The critical assertion: Anthropic's request body must carry the
    //    Gemini-namespaced id in BOTH `tool_use.id` and
    //    `tool_result.tool_use_id`. This proves the namespacing contract
    //    survives end-to-end.
    let body = anth_state
        .captured_body()
        .await
        .expect("anthropic received a request in turn 2");
    assert!(
        anthropic_body_has_foreign_id(&body, &foreign_id),
        "anthropic must see {foreign_id} in BOTH tool_use.id and tool_result.tool_use_id; \
         body was {body:#?}"
    );
}
