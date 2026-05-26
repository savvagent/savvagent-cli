//! Cross-vendor `tool_use_id` compatibility matrix.
//!
//! For every `(sender_provider, receiver_provider)` pair across the three
//! shipping vendors (Anthropic, Gemini, OpenAI), build a synthetic SPP
//! history whose `tool_use_id` is prefixed with `sender_provider` (the
//! shape Phase 3 will introduce via host-side namespacing) and submit a
//! `CompleteRequest` carrying it to `receiver_provider`. The test asserts
//! (a) the call returns `Ok` and (b) the outgoing request body satisfies
//! a vendor-specific structural assertion:
//!
//! - Anthropic / OpenAI: foreign id appears in BOTH canonical fields
//!   (round-trip preservation; see `*_body_has_foreign_id` inspectors).
//! - Gemini: API has no id field, so the receiver-side contract under
//!   test is that the translator's `id_to_name` lookup resolved the
//!   foreign id to the matching tool name (`list_dir`).
//!
//! Tests use the axum fake-vendor servers in `support/mod.rs`; live-vendor
//! twins are marked `#[ignore]` so PR CI does not need credentials.

#![allow(clippy::collapsible_if)]

mod support;

use savvagent_mcp::ProviderHandler;
use support::{
    FakeState, anthropic_body_has_foreign_id, anthropic_success_response, build_request,
    gemini_body_has_resolved_function_name, gemini_success_response, history_with_foreign_id,
    openai_body_has_foreign_id, openai_success_response, spawn_fake_anthropic, spawn_fake_gemini,
    spawn_fake_openai,
};

// ===========================================================================
// Anthropic receiver pairs
// ===========================================================================

#[tokio::test]
async fn anthropic_to_anthropic_control() {
    let state = FakeState::new(anthropic_success_response());
    let base = spawn_fake_anthropic(&state).await;
    let provider = provider_anthropic::provider_for_tests(base);

    let foreign_id = "anthropic:toolu_abc_123";
    let history = history_with_foreign_id("anthropic");
    let req = build_request("claude-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("anthropic accepts anthropic-prefixed tool_use_id");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake anthropic received a request");
    assert!(
        anthropic_body_has_foreign_id(&body, foreign_id),
        "anthropic body must carry {foreign_id} in BOTH tool_use.id and tool_result.tool_use_id; body was {body:#?}"
    );
}

#[tokio::test]
async fn gemini_to_anthropic() {
    let state = FakeState::new(anthropic_success_response());
    let base = spawn_fake_anthropic(&state).await;
    let provider = provider_anthropic::provider_for_tests(base);

    let foreign_id = "gemini:toolu_abc_123";
    let history = history_with_foreign_id("gemini");
    let req = build_request("claude-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("anthropic accepts gemini-prefixed tool_use_id");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake anthropic received a request");
    assert!(
        anthropic_body_has_foreign_id(&body, foreign_id),
        "anthropic body must carry {foreign_id} in BOTH tool_use.id and tool_result.tool_use_id; body was {body:#?}"
    );
}

#[tokio::test]
async fn openai_to_anthropic() {
    let state = FakeState::new(anthropic_success_response());
    let base = spawn_fake_anthropic(&state).await;
    let provider = provider_anthropic::provider_for_tests(base);

    let foreign_id = "openai:toolu_abc_123";
    let history = history_with_foreign_id("openai");
    let req = build_request("claude-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("anthropic accepts openai-prefixed tool_use_id");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake anthropic received a request");
    assert!(
        anthropic_body_has_foreign_id(&body, foreign_id),
        "anthropic body must carry {foreign_id} in BOTH tool_use.id and tool_result.tool_use_id; body was {body:#?}"
    );
}

// ===========================================================================
// Gemini receiver pairs
// ===========================================================================

#[tokio::test]
async fn anthropic_to_gemini() {
    let state = FakeState::new(gemini_success_response());
    let base = spawn_fake_gemini(&state).await;
    let provider = provider_gemini::provider_for_tests(base);

    let history = history_with_foreign_id("anthropic");
    let req = build_request("gemini-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("gemini accepts anthropic-prefixed tool_use_id in history");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake gemini received a request");
    assert!(
        gemini_body_has_resolved_function_name(&body, "list_dir"),
        "gemini translator must resolve the foreign tool_use_id back to `list_dir` via id_to_name; body was {body:#?}"
    );
}

#[tokio::test]
async fn gemini_to_gemini_control() {
    let state = FakeState::new(gemini_success_response());
    let base = spawn_fake_gemini(&state).await;
    let provider = provider_gemini::provider_for_tests(base);

    let history = history_with_foreign_id("gemini");
    let req = build_request("gemini-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("gemini accepts gemini-prefixed tool_use_id in history");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake gemini received a request");
    assert!(
        gemini_body_has_resolved_function_name(&body, "list_dir"),
        "gemini translator must resolve the foreign tool_use_id back to `list_dir` via id_to_name; body was {body:#?}"
    );
}

#[tokio::test]
async fn openai_to_gemini() {
    let state = FakeState::new(gemini_success_response());
    let base = spawn_fake_gemini(&state).await;
    let provider = provider_gemini::provider_for_tests(base);

    let history = history_with_foreign_id("openai");
    let req = build_request("gemini-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("gemini accepts openai-prefixed tool_use_id in history");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake gemini received a request");
    assert!(
        gemini_body_has_resolved_function_name(&body, "list_dir"),
        "gemini translator must resolve the foreign tool_use_id back to `list_dir` via id_to_name; body was {body:#?}"
    );
}

// ===========================================================================
// OpenAI receiver pairs
// ===========================================================================

#[tokio::test]
async fn anthropic_to_openai() {
    let state = FakeState::new(openai_success_response());
    let base = spawn_fake_openai(&state).await;
    let provider = provider_openai::provider_for_tests(base);

    let foreign_id = "anthropic:toolu_abc_123";
    let history = history_with_foreign_id("anthropic");
    let req = build_request("gpt-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("openai accepts anthropic-prefixed tool_use_id");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake openai received a request");
    assert!(
        openai_body_has_foreign_id(&body, foreign_id),
        "openai body must carry {foreign_id} in BOTH assistant.tool_calls[].id and tool-role.tool_call_id; body was {body:#?}"
    );
}

#[tokio::test]
async fn gemini_to_openai() {
    let state = FakeState::new(openai_success_response());
    let base = spawn_fake_openai(&state).await;
    let provider = provider_openai::provider_for_tests(base);

    let foreign_id = "gemini:toolu_abc_123";
    let history = history_with_foreign_id("gemini");
    let req = build_request("gpt-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("openai accepts gemini-prefixed tool_use_id");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake openai received a request");
    assert!(
        openai_body_has_foreign_id(&body, foreign_id),
        "openai body must carry {foreign_id} in BOTH assistant.tool_calls[].id and tool-role.tool_call_id; body was {body:#?}"
    );
}

#[tokio::test]
async fn openai_to_openai_control() {
    let state = FakeState::new(openai_success_response());
    let base = spawn_fake_openai(&state).await;
    let provider = provider_openai::provider_for_tests(base);

    let foreign_id = "openai:toolu_abc_123";
    let history = history_with_foreign_id("openai");
    let req = build_request("gpt-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("openai accepts openai-prefixed tool_use_id");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake openai received a request");
    assert!(
        openai_body_has_foreign_id(&body, foreign_id),
        "openai body must carry {foreign_id} in BOTH assistant.tool_calls[].id and tool-role.tool_call_id; body was {body:#?}"
    );
}

// ===========================================================================
// Live-vendor twins (#[ignore]; run via `cargo test … -- --ignored`).
// One per receiver, sender-side coverage stays in the mocked matrix above.
// Each test reads its API key from env and skips with a clear message
// when the key is absent so `--ignored` does not fail loudly for vendors
// the operator has no credentials for.
// ===========================================================================

fn live_request_for(model: &str) -> savvagent_protocol::CompleteRequest {
    build_request(model, history_with_foreign_id("anthropic"))
}

/// Look up a live-vendor API key. Returns `Some(key)` if the primary or
/// (optional) fallback env var is set; returns `None` only when
/// `SAVVAGENT_ALLOW_MISSING_LIVE_KEYS=1` explicitly opts into the silent
/// skip. Panics otherwise — without that opt-out, a CI run that
/// invokes `cargo test … -- --ignored` without exporting the right
/// secrets would silently report PASS, masking a misconfiguration. The
/// opt-out exists so a developer running `cargo test -- --ignored`
/// locally with only one vendor's key still gets through the others
/// without editing the test file.
fn require_live_key(primary: &str, fallback: Option<&str>) -> Option<String> {
    if let Ok(key) = std::env::var(primary) {
        return Some(key);
    }
    if let Some(fb) = fallback {
        if let Ok(key) = std::env::var(fb) {
            return Some(key);
        }
    }
    if std::env::var("SAVVAGENT_ALLOW_MISSING_LIVE_KEYS").is_ok() {
        eprintln!("{primary} not set; SAVVAGENT_ALLOW_MISSING_LIVE_KEYS active — skipping");
        return None;
    }
    let fallback_note = fallback.map(|f| format!(" (or {f})")).unwrap_or_default();
    panic!(
        "{primary}{fallback_note} not set, and SAVVAGENT_ALLOW_MISSING_LIVE_KEYS \
         is unset. Export the key, or set SAVVAGENT_ALLOW_MISSING_LIVE_KEYS=1 \
         to allow silent skip (local-dev convenience)."
    );
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY (or SAVVAGENT_ALLOW_MISSING_LIVE_KEYS=1 to skip silently); run with cargo test … -- --ignored"]
async fn anthropic_to_anthropic_live() {
    let Some(key) = require_live_key("ANTHROPIC_API_KEY", None) else {
        return;
    };
    let provider = provider_anthropic::AnthropicProvider::builder()
        .api_key(key)
        .build()
        .expect("anthropic provider build ok");
    let req = live_request_for("claude-haiku-4-5");
    provider
        .complete(req, None)
        .await
        .expect("live anthropic accepts anthropic-prefixed tool_use_id");
}

#[tokio::test]
#[ignore = "requires GEMINI_API_KEY (or GOOGLE_API_KEY, or SAVVAGENT_ALLOW_MISSING_LIVE_KEYS=1 to skip silently); run with cargo test … -- --ignored"]
async fn anthropic_to_gemini_live() {
    let Some(key) = require_live_key("GEMINI_API_KEY", Some("GOOGLE_API_KEY")) else {
        return;
    };
    let provider = provider_gemini::GeminiProvider::builder()
        .api_key(key)
        .build()
        .expect("gemini provider build ok");
    let req = live_request_for("gemini-2.0-flash");
    provider
        .complete(req, None)
        .await
        .expect("live gemini accepts anthropic-prefixed tool_use_id");
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY (or SAVVAGENT_ALLOW_MISSING_LIVE_KEYS=1 to skip silently); run with cargo test … -- --ignored"]
async fn anthropic_to_openai_live() {
    let Some(key) = require_live_key("OPENAI_API_KEY", None) else {
        return;
    };
    let provider = provider_openai::OpenAiProvider::builder()
        .api_key(key)
        .build()
        .expect("openai provider build ok");
    let req = live_request_for("gpt-4o-mini");
    provider
        .complete(req, None)
        .await
        .expect("live openai accepts anthropic-prefixed tool_use_id");
}
