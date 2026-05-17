//! Test support for `cross_vendor_history.rs` and
//! `cross_provider_history.rs`.
//!
//! Builds synthetic SPP histories whose `tool_use_id`s carry a foreign
//! provider prefix (e.g. `"anthropic:toolu_xyz"`), and spins per-vendor
//! axum fake servers that capture the body of every received request so
//! tests can assert the foreign id flowed through the translator unchanged.
//!
//! The pattern mirrors `crates/provider-anthropic/tests/integration.rs`
//! and its siblings.
//!
//! `dead_code` is suppressed here because each integration-test binary
//! that includes this module via `mod support;` consumes only a subset of
//! the helpers — `cross_vendor_history` exercises every vendor, while
//! `cross_provider_history` only touches the Anthropic-side helpers.
//! Without the allow, the latter binary would fail under workspace
//! `-D warnings`.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use savvagent_protocol::{CompleteRequest, ContentBlock, Message, Role};
use serde_json::{Value, json};

/// Build an SPP `Vec<Message>` that contains an assistant `ToolUse` block
/// and a matching user `ToolResult` block, with the `tool_use_id` prefixed
/// by `sender_provider` (e.g. `"anthropic:toolu_abc"`). The history is
/// followed by a fresh user message so the resulting `CompleteRequest`
/// asks the receiving provider to produce its next turn given that
/// foreign-id-bearing context.
pub fn history_with_foreign_id(sender_provider: &str) -> Vec<Message> {
    let foreign_id = format!("{sender_provider}:toolu_abc_123");
    vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "list the cwd".into(),
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: foreign_id.clone(),
                name: "list_dir".into(),
                input: json!({ "path": "." }),
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: foreign_id,
                content: vec![ContentBlock::Text {
                    text: "Cargo.toml\nsrc\n".into(),
                }],
                is_error: false,
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "what did you find?".into(),
            }],
        },
    ]
}

/// Build a minimal `CompleteRequest` carrying `history` against the given
/// model, with one synthetic tool (`list_dir`) declared so the receiver's
/// translator has a tool surface to attach.
pub fn build_request(model: &str, history: Vec<Message>) -> CompleteRequest {
    use savvagent_protocol::ToolDef;
    CompleteRequest {
        model: model.into(),
        messages: history,
        system: None,
        tools: vec![ToolDef {
            name: "list_dir".into(),
            description: "List directory entries.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        }],
        temperature: None,
        top_p: None,
        max_tokens: 256,
        stop_sequences: Vec::new(),
        stream: false,
        thinking: None,
        metadata: None,
    }
}

/// Shared state for the axum fake-vendor handlers: the most recent request
/// body received by the fake (so the test can assert the foreign id flowed
/// through to the wire) plus the canned response to return.
#[derive(Clone)]
pub struct FakeState {
    last_body: Arc<Mutex<Option<Value>>>,
    response: Value,
}

impl FakeState {
    pub fn new(response: Value) -> Self {
        Self {
            last_body: Arc::new(Mutex::new(None)),
            response,
        }
    }

    /// Read the captured body of the most recent request the fake received.
    /// Returns `None` if no request has been served yet.
    ///
    /// Async-signatured for forward-compat (an `async fn` cannot be
    /// downgraded to a sync one without an API break) even though the
    /// implementation no longer awaits — `std::sync::Mutex` is fine here
    /// because the critical section is a single field clone.
    pub async fn captured_body(&self) -> Option<Value> {
        self.last_body.lock().unwrap().clone()
    }
}

async fn capture_and_respond(state: State<FakeState>, Json(body): Json<Value>) -> Response {
    *state.0.last_body.lock().unwrap() = Some(body);
    (StatusCode::OK, Json(state.0.response.clone())).into_response()
}

/// Spin a fake Anthropic backend on `127.0.0.1:0`. Returns the base URL
/// the caller passes to `provider_for_tests`. The handler captures the
/// request body into `state.last_body` (shared via the `Arc<Mutex<_>>`
/// inside `FakeState`) and replies with `state.response`. Callers keep
/// their own `state` binding to read the captured body after the call:
///
/// ```ignore
/// let state = FakeState::new(anthropic_success_response());
/// let base = spawn_fake_anthropic(&state).await;
/// // ... drive the provider against `base` ...
/// let body = state.captured_body().await.expect("...");
/// ```
pub async fn spawn_fake_anthropic(state: &FakeState) -> String {
    let app = Router::new()
        .route("/v1/messages", post(capture_and_respond))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("fake vendor server died: {e}");
        }
    });
    format!("http://{addr}")
}

/// Spin a fake Gemini backend on `127.0.0.1:0`. Gemini's request path is
/// `/v1beta/models/{model}:{action}` — both `generateContent` and
/// `streamGenerateContent` collapse into one route-table entry whose
/// captured segment is the whole `<model>:<action>` tail. Same parameter
/// name as `crates/provider-gemini/tests/integration.rs` for symmetry.
pub async fn spawn_fake_gemini(state: &FakeState) -> String {
    let app = Router::new()
        .route(
            "/v1beta/models/{model_with_action}",
            post(capture_and_respond),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("fake vendor server died: {e}");
        }
    });
    format!("http://{addr}")
}

/// Spin a fake OpenAI backend on `127.0.0.1:0`. See `spawn_fake_anthropic`
/// for the calling pattern.
pub async fn spawn_fake_openai(state: &FakeState) -> String {
    let app = Router::new()
        .route("/v1/chat/completions", post(capture_and_respond))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("fake vendor server died: {e}");
        }
    });
    format!("http://{addr}")
}

/// Canned success response from a fake Anthropic backend: a single
/// assistant text block, `end_turn`. Sufficient to drive a non-streaming
/// `complete` call to `Ok`.
pub fn anthropic_success_response() -> Value {
    json!({
        "id": "msg_test_0",
        "type": "message",
        "role": "assistant",
        "model": "claude-test",
        "content": [
            { "type": "text", "text": "ok" }
        ],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": { "input_tokens": 10, "output_tokens": 1 }
    })
}

/// Canned success response from a fake Gemini backend.
pub fn gemini_success_response() -> Value {
    json!({
        "responseId": "gem_test_0",
        "modelVersion": "gemini-test",
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{ "text": "ok" }]
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

/// Canned success response from a fake OpenAI backend.
pub fn openai_success_response() -> Value {
    json!({
        "id": "chatcmpl_test_0",
        "object": "chat.completion",
        "created": 0,
        "model": "gpt-test",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "ok",
                "tool_calls": []
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 1,
            "total_tokens": 11
        }
    })
}

// ---------------------------------------------------------------------------
// Per-vendor wire-body inspectors.
//
// These are NOT recursive "contains the string anywhere in JSON" walkers —
// each inspector navigates the vendor's canonical message-content structure
// and reports whether the foreign id (or, for Gemini, the resolved tool
// name) appears in the *semantically correct* location. A drift that moved
// the id into the wrong field (e.g. dropped it from `tool_use_id` and only
// left it in `metadata`) would fail these checks, where a permissive
// recursive walker would silently pass.
// ---------------------------------------------------------------------------

/// True iff the Anthropic request body contains a `tool_use` block whose
/// `id` is `foreign_id`, AND a `tool_result` block whose `tool_use_id` is
/// `foreign_id`. Both must match — the round-trip is what we're asserting.
pub fn anthropic_body_has_foreign_id(body: &Value, foreign_id: &str) -> bool {
    let mut saw_tool_use = false;
    let mut saw_tool_result = false;
    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else {
        return false;
    };
    for msg in messages {
        let Some(content) = msg.get("content").and_then(|c| c.as_array()) else {
            continue;
        };
        for block in content {
            let ty = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            // Match guards instead of nested `if let` keep `clippy::collapsible_match`
            // happy under the workspace's `-D warnings` CI flag.
            match ty {
                "tool_use" if block.get("id").and_then(|v| v.as_str()) == Some(foreign_id) => {
                    saw_tool_use = true;
                }
                "tool_result"
                    if block.get("tool_use_id").and_then(|v| v.as_str()) == Some(foreign_id) =>
                {
                    saw_tool_result = true;
                }
                _ => {}
            }
        }
    }
    saw_tool_use && saw_tool_result
}

/// True iff the OpenAI request body contains an assistant message with a
/// `tool_calls` entry whose `id` is `foreign_id`, AND a `tool`-role
/// message whose `tool_call_id` is `foreign_id`. OpenAI carries the id
/// in two places per round-trip; both must preserve the foreign value.
pub fn openai_body_has_foreign_id(body: &Value, foreign_id: &str) -> bool {
    let mut saw_tool_call = false;
    let mut saw_tool_call_id = false;
    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else {
        return false;
    };
    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role == "assistant" {
            if let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) {
                for call in calls {
                    if call.get("id").and_then(|v| v.as_str()) == Some(foreign_id) {
                        saw_tool_call = true;
                    }
                }
            }
        } else if role == "tool"
            && msg.get("tool_call_id").and_then(|v| v.as_str()) == Some(foreign_id)
        {
            saw_tool_call_id = true;
        }
    }
    saw_tool_call && saw_tool_call_id
}

/// True iff the Gemini request body contains a `functionResponse` part
/// whose `name` is `expected_name`. Gemini's API has no id field — the
/// receiver-side contract under test is that the translator correctly
/// resolved the foreign `tool_use_id` to the matching `ToolUse.name`
/// via the per-request `id_to_name` lookup (see
/// `crates/provider-gemini/src/translate.rs`). A regression that
/// dropped the lookup would surface the placeholder `"unknown_tool"`
/// instead of `expected_name`, and this check would fail.
pub fn gemini_body_has_resolved_function_name(body: &Value, expected_name: &str) -> bool {
    let Some(contents) = body.get("contents").and_then(|c| c.as_array()) else {
        return false;
    };
    for content in contents {
        let Some(parts) = content.get("parts").and_then(|p| p.as_array()) else {
            continue;
        };
        for part in parts {
            if let Some(fr) = part.get("functionResponse") {
                if fr.get("name").and_then(|n| n.as_str()) == Some(expected_name) {
                    return true;
                }
            }
        }
    }
    false
}
