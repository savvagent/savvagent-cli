# Multi-provider pool — Phase 2 implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the cross-vendor `tool_use_id` compatibility CI gate (the "Phase 2 gate" from the spec). Every `(sender_provider, receiver_provider)` pair across the three shipping vendors (Anthropic, Gemini, OpenAI) is exercised through a matrix test that synthesizes history with foreign-prefixed `tool_use_id`s and asserts each vendor's translator round-trips them without breaking the receiver's semantic contract. **What "round-trips" means is per-vendor** (this is the substance of the cross-vendor compatibility question, not an implementation detail to wave away): for **Anthropic** and **OpenAI**, which carry `tool_use_id` / `tool_call_id` on the wire, the foreign id must appear verbatim in the canonical field of the outgoing request body. For **Gemini**, which routes function results by `name` rather than id (Gemini's API has no id field), the translator's existing `id_to_name` lookup must resolve the foreign id to the correct function name on the wire — the test asserts the corresponding `functionResponse` part carries the right name, which is the contract that matters since Gemini cannot leak an unrecognized id to the wire even in principle. The gate runs in PR CI against axum-backed mock vendor servers, with `#[ignore]`-marked twins for nightly/manual runs against live APIs. **Build the matrix first, ship no fallback adapter unless a pair fails** (per design decision deferred to a Phase 2.5 patch if real-vendor testing surfaces a regression).

**Architecture:**
- New `crates/savvagent-host/tests/cross_vendor_history.rs` integration test holds nine `#[tokio::test]` functions — one per `(sender, receiver)` pair — so `cargo test … --no-fail-fast` produces per-pair pass/fail lines in CI output.
- Shared helpers live in `crates/savvagent-host/tests/support/mod.rs`: synthetic-history builder (`history_with_foreign_id`), axum-based fake-vendor spawners with body-capture state (`spawn_fake_anthropic`, `spawn_fake_gemini`, `spawn_fake_openai`), and success-response fixture builders. We **reuse the axum fake-server pattern already established in `crates/provider-*/tests/integration.rs`** rather than introducing a new HTTP mocking framework — same outcome (hand-written JSON fixtures, offline PR CI), zero new workspace deps.
- The host crate gains dev-dependencies on `provider-anthropic`, `provider-gemini`, `provider-openai` (clean direction — these crates don't depend on `savvagent-host`, so no cycle).
- Each test points the provider's `provider_for_tests(base_url)` factory at the fake server, calls `ProviderHandler::complete` directly (the layer where the SPP→vendor translator runs), then asserts (a) the call returned `Ok`, and (b) the captured outgoing request body satisfies a **vendor-specific structural assertion** (see "Goal" — Anthropic/OpenAI look for the foreign id in the canonical id field via per-vendor inspectors in `support/mod.rs`; Gemini looks for a `functionResponse` part with the resolved tool name).
- A new `cross-vendor-gate` job in `.github/workflows/ci.yml` runs only this test with `--no-fail-fast` so PRs that break the matrix show every failing pair.
- Live-vendor variants are `#[ignore]`-marked; manually invoked via `cargo test … -- --ignored` with the appropriate `*_API_KEY` env vars set. No nightly workflow ships this phase — that's a follow-up tracked in release notes.
- **No host-side ID namespacing, no fallback hash adapters, no `cross_vendor_history_ok` capability flag**: those are explicitly deferred per design Q&A. Phase 2 ships a CI signal; Phase 3 consumes it.

**Tech Stack:** Rust 2024, Tokio, `axum` (already in workspace deps), `serde_json`, `async-trait`. No new workspace dependencies.

**Spec:** `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`. This plan covers **Phase 2 only** — the "Phase 2 gate" section of the spec plus its testing-strategy bullet. Phases 3-6 each get their own plan.

---

## File structure (Phase 2)

**New files:**
- `crates/savvagent-host/tests/cross_vendor_history.rs` — nine pair tests + their `#[ignore]`-marked live twins.
- `crates/savvagent-host/tests/support/mod.rs` — fake-vendor spawners, body-capture state, synthetic-history helpers, success-fixture builders.

**Modified files:**
- `crates/savvagent-host/Cargo.toml` — add `provider-anthropic`, `provider-gemini`, `provider-openai`, and `axum` (with the `json` feature) to `[dev-dependencies]`. `serde_json` and `futures` are intentionally **not** re-declared because they are already in the host crate's `[dependencies]` and Cargo makes runtime deps available to tests automatically; duplicating them as dev-deps would add visual noise without effect.
- `.github/workflows/ci.yml` — add `cross-vendor-gate` job after `test`.
- `Cargo.toml` (workspace) — bump `[workspace.package].version` to `0.16.0` and every literal in `[workspace.dependencies]` to `0.16.0`.
- `CHANGELOG.md` — add `## 0.16.0 - 2026-05-16` entry.
- `README.md` — short note in the "Architecture" or "Testing" section pointing at the new gate.

---

## Task 1: Add dev-dependencies to savvagent-host

**Files:**
- Modify: `crates/savvagent-host/Cargo.toml`

We need the three real provider crates available to the integration test, plus axum for the fake servers. The host crate depends on none of these at runtime; this is a dev-only layering choice.

- [ ] **Step 1: Inspect current `[dev-dependencies]` section**

Run: `cat crates/savvagent-host/Cargo.toml`
Expected: section currently contains `tempfile`, `tokio` (with macros + rt-multi-thread), `tracing-subscriber`.

- [ ] **Step 2: Add the new dev-deps**

Edit `crates/savvagent-host/Cargo.toml`, replace the existing `[dev-dependencies]` block with:

```toml
[dev-dependencies]
tempfile = "3"
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
tracing-subscriber = { workspace = true }

# Cross-vendor compatibility gate (tests/cross_vendor_history.rs).
# Only deps that are NOT already in [dependencies] go here: Cargo makes
# runtime deps available to tests automatically, so re-declaring
# `serde_json` or `futures` (both in the host's [dependencies]) would be
# redundant. `axum` is dev-only and needs the `json` feature for its
# `Json<_>` extractor used by the fake-vendor handlers in Task 2.
provider-anthropic.workspace = true
provider-gemini.workspace = true
provider-openai.workspace = true
axum = { workspace = true, features = ["json"] }
```

- [ ] **Step 3: Verify cargo accepts the new deps**

Run: `cargo check -p savvagent-host --tests`
Expected: clean build (no test files reference these yet; we're only confirming the dep graph resolves).

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent-host/Cargo.toml
git commit -m "build(host): add provider crates + axum as dev-deps for cross-vendor gate"
```

---

## Task 2: Synthetic-history helper + fake-vendor spawners

**Files:**
- Create: `crates/savvagent-host/tests/support/mod.rs`

Self-contained test-support module. The same helpers serve all nine pair tests. The axum fake-server pattern is lifted (with adaptations for body capture) from `crates/provider-anthropic/tests/integration.rs`.

- [ ] **Step 1: Create the support module with synthetic-history helper**

Create `crates/savvagent-host/tests/support/mod.rs`:

```rust
//! Test support for `cross_vendor_history.rs`.
//!
//! Builds synthetic SPP histories whose `tool_use_id`s carry a foreign
//! provider prefix (e.g. `"anthropic:toolu_xyz"`), and spins per-vendor
//! axum fake servers that capture the body of every received request so
//! tests can assert the foreign id flowed through the translator unchanged.
//!
//! The pattern mirrors `crates/provider-anthropic/tests/integration.rs`
//! and its siblings.

// TODO(phase-2): drop this allow once Tasks 3-7 land their `mod support;`
// declarations — those will reference every public item below.
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
    pub last_body: Arc<Mutex<Option<Value>>>,
    pub response: Value,
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

async fn capture_and_respond(
    state: State<FakeState>,
    Json(body): Json<Value>,
) -> Response {
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
        let _ = axum::serve(listener, app).await;
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
        let _ = axum::serve(listener, app).await;
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
        let _ = axum::serve(listener, app).await;
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
            // happy under the workspace's `-D warnings` CI flag. Formatted per
            // rustfmt's preferred shape (guard inline, arm body brace on the
            // same line as the `=>`) so `cargo fmt --check` stays clean.
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
```

- [ ] **Step 2: Confirm the support module compiles in isolation**

Run: `cargo check -p savvagent-host --tests`
Expected: clean build. The `#![allow(dead_code)]` at the top of the file suppresses warnings because no test file references the helpers yet.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent-host/tests/support/mod.rs
git commit -m "test(host): add cross-vendor test support module (fake vendor servers + synthetic history)"
```

---

## Task 3: First pair — (anthropic → anthropic) control test

**Files:**
- Create: `crates/savvagent-host/tests/cross_vendor_history.rs`

This is the "same vendor sees its own id" control case. It establishes the test scaffold every subsequent pair will follow: build history with a foreign-prefixed id (here `"anthropic:..."`), spin the fake vendor, call `ProviderHandler::complete` via `provider_for_tests`, assert `Ok`, assert the foreign id appears in the captured outgoing body. Test failure here means the test scaffold itself is broken.

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent-host/tests/cross_vendor_history.rs`:

```rust
//! Cross-vendor `tool_use_id` compatibility matrix.
//!
//! For every `(sender_provider, receiver_provider)` pair across the three
//! shipping vendors (Anthropic, Gemini, OpenAI), build a synthetic SPP
//! history whose `tool_use_id` is prefixed with `sender_provider` (the
//! shape Phase 3 will introduce via host-side namespacing) and submit a
//! `CompleteRequest` carrying it to `receiver_provider`. The test asserts
//! (a) the call returns `Ok` and (b) the foreign id appears verbatim in
//! the outgoing request body the receiver's translator built.
//!
//! Tests use the axum fake-vendor servers in `support::mod.rs`; live-vendor
//! twins are marked `#[ignore]` so PR CI does not need credentials.

mod support;

use savvagent_mcp::ProviderHandler;
use support::{
    FakeState, anthropic_body_has_foreign_id, anthropic_success_response, build_request,
    gemini_body_has_resolved_function_name, gemini_success_response, history_with_foreign_id,
    openai_body_has_foreign_id, openai_success_response, spawn_fake_anthropic,
    spawn_fake_gemini, spawn_fake_openai,
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
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p savvagent-host --test cross_vendor_history anthropic_to_anthropic_control -- --nocapture`
Expected: `test anthropic_to_anthropic_control ... ok`. If it fails, the scaffold has a bug — fix before moving on.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent-host/tests/cross_vendor_history.rs
git commit -m "test(host): cross-vendor gate — anthropic→anthropic control pair"
```

---

## Task 4: (anthropic → gemini) and (anthropic → openai) pairs

**Files:**
- Modify: `crates/savvagent-host/tests/cross_vendor_history.rs`

These two are the first real cross-vendor cases. Gemini's translator routes function results by name (it builds an `id_to_name` lookup from prior assistant turns — see `crates/provider-gemini/src/translate.rs:142-158`), so the foreign id never reaches the wire body in a literal `tool_use_id` field; instead we assert the call succeeds (Gemini's translator does not reject the request). OpenAI preserves `tool_use_id` verbatim as `tool_call_id`, so we assert the foreign string appears in the body.

- [ ] **Step 1: Append the two tests**

Append to `crates/savvagent-host/tests/cross_vendor_history.rs`:

```rust
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

    // Gemini's API has no id field; the round-trip contract is that the
    // translator resolves the foreign tool_use_id back to the matching
    // ToolUse.name (`list_dir`) via the per-request id_to_name lookup.
    // A regression dropping that lookup would surface `"unknown_tool"`
    // on the wire instead, and this assertion would fail.
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
```

- [ ] **Step 2: Run both tests**

Run: `cargo test -p savvagent-host --test cross_vendor_history -- --nocapture --test-threads=1 anthropic_to`
Expected: `test anthropic_to_anthropic_control ... ok`, `test anthropic_to_gemini ... ok`, `test anthropic_to_openai ... ok`. Three tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent-host/tests/cross_vendor_history.rs
git commit -m "test(host): cross-vendor gate — anthropic→{gemini,openai} pairs"
```

---

## Task 5: (gemini → *) sender pairs

**Files:**
- Modify: `crates/savvagent-host/tests/cross_vendor_history.rs`

Three tests: `gemini → anthropic`, `gemini → gemini` (control), `gemini → openai`. Same template; just swap the sender prefix and the receiver provider.

- [ ] **Step 1: Append the three tests**

Append to `crates/savvagent-host/tests/cross_vendor_history.rs`:

```rust
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
```

- [ ] **Step 2: Run all six tests so far**

Run: `cargo test -p savvagent-host --test cross_vendor_history -- --nocapture`
Expected: six tests pass (`anthropic_to_*` and `gemini_to_*`).

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent-host/tests/cross_vendor_history.rs
git commit -m "test(host): cross-vendor gate — gemini→{anthropic,gemini,openai} pairs"
```

---

## Task 6: (openai → *) sender pairs

**Files:**
- Modify: `crates/savvagent-host/tests/cross_vendor_history.rs`

Final three: `openai → anthropic`, `openai → gemini`, `openai → openai` (control). Matrix is now complete (9 pairs).

- [ ] **Step 1: Append the three tests**

Append to `crates/savvagent-host/tests/cross_vendor_history.rs`:

```rust
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
```

- [ ] **Step 2: Run the full matrix**

Run: `cargo test -p savvagent-host --test cross_vendor_history -- --nocapture`
Expected: nine tests pass. Per-pair `... ok` line in the output for each.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent-host/tests/cross_vendor_history.rs
git commit -m "test(host): cross-vendor gate — openai→{anthropic,gemini,openai} pairs (matrix complete)"
```

---

## Task 7: `#[ignore]`-marked live-vendor template

**Files:**
- Modify: `crates/savvagent-host/tests/cross_vendor_history.rs`

Add **one** live-vendor variant per receiver (three tests total — one against the real Anthropic, one against real Gemini, one against real OpenAI; sender is anthropic-prefixed for all three since the matrix already showed sender prefix doesn't matter). These are `#[ignore]` so PR CI skips them. They run via `cargo test --test cross_vendor_history -- --ignored` when the operator sets the relevant `*_API_KEY` env var. Marker tests, not exhaustive — full live matrix is a follow-up tracked in the release notes.

- [ ] **Step 1: Append the live-vendor module**

Append to `crates/savvagent-host/tests/cross_vendor_history.rs`:

```rust
// ===========================================================================
// Live-vendor twins (#[ignore]; run via `cargo test … -- --ignored`).
// One per receiver, sender-side coverage stays in the mocked matrix above.
// Each test reads its API key from env and skips with a clear message
// when the key is absent so `--ignored` does not fail loudly for vendors
// the operator has no credentials for.
// ===========================================================================

fn live_request_for(model: &str, sender: &str) -> savvagent_protocol::CompleteRequest {
    build_request(model, history_with_foreign_id(sender))
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY; run with cargo test … -- --ignored"]
async fn anthropic_to_anthropic_live() {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        eprintln!("ANTHROPIC_API_KEY not set; skipping live anthropic gate test");
        return;
    };
    let provider = provider_anthropic::AnthropicProvider::builder()
        .api_key(key)
        .build()
        .expect("anthropic provider build ok");
    let req = live_request_for("claude-haiku-4-5", "anthropic");
    provider
        .complete(req, None)
        .await
        .expect("live anthropic accepts anthropic-prefixed tool_use_id");
}

#[tokio::test]
#[ignore = "requires GEMINI_API_KEY (or GOOGLE_API_KEY); run with cargo test … -- --ignored"]
async fn anthropic_to_gemini_live() {
    let Ok(key) = std::env::var("GEMINI_API_KEY").or_else(|_| std::env::var("GOOGLE_API_KEY")) else {
        eprintln!("GEMINI_API_KEY not set; skipping live gemini gate test");
        return;
    };
    let provider = provider_gemini::GeminiProvider::builder()
        .api_key(key)
        .build()
        .expect("gemini provider build ok");
    let req = live_request_for("gemini-2.0-flash", "anthropic");
    provider
        .complete(req, None)
        .await
        .expect("live gemini accepts anthropic-prefixed tool_use_id");
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY; run with cargo test … -- --ignored"]
async fn anthropic_to_openai_live() {
    let Ok(key) = std::env::var("OPENAI_API_KEY") else {
        eprintln!("OPENAI_API_KEY not set; skipping live openai gate test");
        return;
    };
    let provider = provider_openai::OpenAiProvider::builder()
        .api_key(key)
        .build()
        .expect("openai provider build ok");
    let req = live_request_for("gpt-4o-mini", "anthropic");
    provider
        .complete(req, None)
        .await
        .expect("live openai accepts anthropic-prefixed tool_use_id");
}
```

- [ ] **Step 2: Confirm the live tests compile but are skipped by default**

Run: `cargo test -p savvagent-host --test cross_vendor_history`
Expected: `9 passed; 0 failed; 3 ignored` (the nine mocked + three live-`#[ignore]`).

- [ ] **Step 3: Smoke-test the live-test plumbing locally if a key is available**

Skip this step if the implementer has no API keys handy — the live tests are documented as `--ignored` for that reason. With at least one key available:

Run: `ANTHROPIC_API_KEY=sk-… cargo test -p savvagent-host --test cross_vendor_history anthropic_to_anthropic_live -- --ignored --nocapture`
Expected: live test passes (real Anthropic accepts a foreign-prefixed `tool_use_id`).

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent-host/tests/cross_vendor_history.rs
git commit -m "test(host): cross-vendor gate — #[ignore]-marked live-vendor variants"
```

---

## Task 8: `cross-vendor-gate` GitHub Actions job

**Files:**
- Modify: `.github/workflows/ci.yml`

A dedicated job so PR diffs that break the matrix surface every failing pair, not just the first. Runs `cargo test … --no-fail-fast` on Linux only (matrix coverage of vendor translators isn't OS-specific; the existing `test` job already runs the full workspace across three OSes). Job depends on nothing else so it can run in parallel with `test`.

- [ ] **Step 1: Read the current CI file to confirm the exact `dist-plan` job location**

Run: `cat .github/workflows/ci.yml`
Expected: see `lint`, `test`, `dist-plan` jobs. The new job appends after `test`.

- [ ] **Step 2: Add the new job**

Edit `.github/workflows/ci.yml`. After the `test:` job block and before the `dist-plan:` job block, insert:

```yaml
  # Cross-vendor `tool_use_id` compatibility gate. Each (sender, receiver)
  # pair across the three shipping vendors gets its own #[tokio::test] in
  # `crates/savvagent-host/tests/cross_vendor_history.rs`; `--no-fail-fast`
  # ensures every failing pair appears in the job log instead of stopping
  # at the first. See docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md
  # ("Phase 2 gate") for the contract this job enforces.
  #
  # Live-vendor twins are #[ignore]-marked and run manually via
  # `cargo test -p savvagent-host --test cross_vendor_history -- --ignored`
  # with the appropriate *_API_KEY env vars set; this PR-CI job covers only
  # the offline mocked matrix.
  cross-vendor-gate:
    name: cross-vendor compatibility gate
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install rust toolchain
        uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Install Linux system deps
        run: sudo apt-get update && sudo apt-get install -y libdbus-1-dev pkg-config
      - name: cross-vendor matrix
        # `--no-fail-fast` is a cargo flag (not a libtest flag), so it sits
        # before any `--` separator. With a single test target it's also the
        # natural libtest behavior anyway (all tests in a binary run to
        # completion regardless of intermediate failures); the explicit flag
        # is kept for symmetry with the main `test` job.
        run: cargo test -p savvagent-host --test cross_vendor_history --no-fail-fast
```

- [ ] **Step 3: Validate the workflow YAML parses**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml'))" && echo ok`
Expected: `ok`. If you don't have Python handy, use any YAML linter; the goal is to catch indentation errors before pushing.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add cross-vendor-gate job for tool_use_id compatibility matrix"
```

---

## Task 9: Version bump to 0.16.0 + CHANGELOG + README mention

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.package].version` and every literal in `[workspace.dependencies]`).
- Modify: `CHANGELOG.md`
- Modify: `README.md` (short section in "Testing" or "CI").

Per `[[feedback_semver]]`: pre-1.0, MINOR bump for new capabilities. Phase 2 ships a new CI gate that gates every future release of Phase 3+; that's a MINOR bump. Per `[[feedback_release_notes]]` and `[[feedback_release_docs]]`: every release ships with release notes + README/PRD update.

- [ ] **Step 1: Bump the workspace version**

Edit `Cargo.toml`:

```toml
[workspace.package]
version = "0.16.0"
```

- [ ] **Step 2: Bump every literal under `[workspace.dependencies]`**

Edit `Cargo.toml`. Every `version = "0.15.0"` literal in the `[workspace.dependencies]` block becomes `version = "0.16.0"`. There are twelve entries (savvagent-plugin, savvagent-protocol, savvagent-mcp, savvagent-host, provider-anthropic, provider-gemini, provider-local, provider-openai, tool-bash, tool-fs, tool-grep, and the savvagent binary if listed). Use:

Run: `grep -n 'version = "0.15.0"' Cargo.toml`
Expected: list of every line that needs editing. Update each via Edit tool with `replace_all`.

- [ ] **Step 3: Verify the bump is consistent**

Run: `grep -c 'version = "0.16.0"' Cargo.toml && grep -c 'version = "0.15.0"' Cargo.toml`
Expected: non-zero `"0.16.0"` count, zero `"0.15.0"` count.

- [ ] **Step 4: Add the CHANGELOG entry**

Edit `CHANGELOG.md`. Insert at the top (after any header, before the previous release):

```markdown
## 0.16.0 - 2026-05-16

### CI

- **Cross-vendor `tool_use_id` compatibility gate.** New
  `crates/savvagent-host/tests/cross_vendor_history.rs` integration test
  validates that every `(sender_provider, receiver_provider)` pair across
  the three shipping vendors (Anthropic, Gemini, OpenAI) accepts SPP
  history whose `tool_use_id` is prefixed with the originating provider
  (e.g. `"anthropic:toolu_xyz"`). Nine pair tests run in PR CI against
  axum-backed mock vendor servers via the dedicated `cross-vendor-gate`
  job with `--no-fail-fast`, so any regression surfaces every failing
  pair. `#[ignore]`-marked live-vendor twins are runnable manually via
  `cargo test -p savvagent-host --test cross_vendor_history -- --ignored`
  with `ANTHROPIC_API_KEY` / `GEMINI_API_KEY` / `OPENAI_API_KEY` set.

### Internal

- Phase 2 of the multi-provider-pool roadmap (see
  `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`).
  No user-visible runtime behavior changes; this release establishes the
  release-gate Phase 3 (cross-provider routing with `@provider:model`
  overrides) depends on. A live-vendor nightly workflow is intentionally
  deferred to a follow-up.
```

- [ ] **Step 5: Add a short README mention**

Edit `README.md`. Find an existing testing/CI section (if absent, add one near the bottom). Insert:

```markdown
### Cross-vendor compatibility gate

`crates/savvagent-host/tests/cross_vendor_history.rs` exercises every
sender/receiver pair across the shipping providers to ensure foreign
`tool_use_id`s round-trip through each vendor's translator. PR CI runs
the offline (mocked) matrix as a dedicated `cross-vendor-gate` job. Live
variants are `#[ignore]`-marked; run them manually with the appropriate
`*_API_KEY` env vars:

```bash
ANTHROPIC_API_KEY=sk-… GEMINI_API_KEY=AIza… OPENAI_API_KEY=sk-… \
    cargo test -p savvagent-host --test cross_vendor_history -- --ignored
```
```

- [ ] **Step 6: Verify the full build + workspace test still passes**

Run: `cargo build --workspace --all-targets`
Expected: clean build with the new version literals.

Run: `cargo test --workspace --no-fail-fast`
Expected: all existing tests still pass plus the nine new pair tests, three ignored. No regressions.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml CHANGELOG.md README.md
git commit -m "release(0.16.0): cross-vendor tool_use_id compatibility gate"
```

---

## Task 10: Final verification + open PR

**Files:** none — verification + git operations only.

- [ ] **Step 1: Match CI's stable toolchain locally**

Per `[[feedback_match_ci_toolchain_locally]]`:

Run: `rustup run stable cargo fmt --all -- --check`
Expected: no output (clean).

Run: `rustup run stable cargo clippy --workspace --all-targets -- -D warnings`
Expected: no output (clean). Per `[[feedback_dead_code_in_binary_crate]]`, watch for `dead_code` warnings on test-only helpers; the `#![allow(dead_code)]` at the top of `support/mod.rs` should cover them.

- [ ] **Step 2: Re-run the gate test stand-alone to confirm `--no-fail-fast` behavior matches CI**

Run: `cargo test -p savvagent-host --test cross_vendor_history --no-fail-fast`
Expected: `9 passed; 0 failed; 3 ignored`.

- [ ] **Step 3: Push the branch + open the PR**

Use the `git-expert` agent per the user-level instruction. Branch name: `phase-2-cross-vendor-gate`. PR body should reference the spec, list the nine pair tests, call out that no host code changes ship in Phase 2, and link the design doc's "Phase 2 gate" section. Do **not** include any Claude self-attribution.

- [ ] **Step 4: Confirm CI is green for the pushed SHA**

Per `[[feedback_verify_ci_after_push]]`:

Run: `gh pr checks` (or `gh run list --branch phase-2-cross-vendor-gate --limit 5`)
Expected: all jobs green, especially the new `cross-vendor-gate` job.

- [ ] **Step 5: Post a status comment on the multi-provider tracking issue (if one exists)**

Per `[[feedback_keep_issue_updated]]`. If there is no tracking issue for the multi-provider roadmap, skip this step.

---

## Spec coverage check

Mapping each Phase 2 requirement in the spec to a task above. **Two spec items (fallback adapters, routing fallthrough) are explicitly deferred** based on the design Q&A captured in the Architecture section — those aren't gaps in the plan, they're deliberate scope cuts that the reviewer should accept or push back on directly.

| Spec requirement | Plan task |
|---|---|
| Per-vendor compatibility test (`crates/savvagent-host/tests/cross_vendor_history.rs`) | Tasks 2-6 |
| Synthetic history with foreign-prefix `tool_use_id` + matching `ToolResult` | Task 2 (helper) + each pair test |
| Submit `CompleteRequest` to each receiver, assert success **plus** receiver-specific structural assertion on the outgoing wire body | Each pair test (Anthropic/OpenAI: `*_body_has_foreign_id` inspector; Gemini: `gemini_body_has_resolved_function_name` inspector — see Goal for why these differ) |
| Vendor-specific fallback strategy for failing pairs | **Deferred per design Q&A** ("Build matrix first, react if it fails"). Plan ships zero adapter code; if a pair fails the matrix in a follow-up live-vendor run, a Phase 2.5 patch lands the relevant adapter. The plan calls this out twice (Goal, Architecture) so reviewers can object before implementation. |
| Default-fallthrough behavior excluding failing vendor from cross-provider routing | **Deferred to Phase 3 per design Q&A.** No routing code ships until Phase 3; Phase 2 produces only the CI signal Phase 3 will read. |
| `release-gate` test target | Task 8 (`cross-vendor-gate` CI job) |
| PR CI uses vendor mocks/replays (spec wording: "vendor mocks/replays in PR CI") | Task 2 + Task 8. Plan uses **hand-authored fixtures via axum fake servers** under the "mocks" half of the spec's "mocks/replays" — explicitly chosen over VCR-style recorded cassettes in the design Q&A ("wiremock + hand-written fixtures" selection). If a reviewer requires recorded real-payload captures specifically, that's a Phase 2.5 follow-up that re-runs the live `--ignored` tests with a capturing middleware. |
| Nightly CI runs the same tests against live vendor APIs gated behind credentials | **Out of scope for this PR.** `#[ignore]`-marked live twins land in Task 7 as the manual entry point; a nightly workflow that runs them on schedule is a follow-up tracked in release notes. |
| Phase 2 ships **no code other than tests + per-vendor fallback adapters** (per spec phasing line: "Phase 2 of the phasing plan is dedicated to standing up the CI compatibility matrix") | Plan ships only the matrix; fallback adapters are deferred per the row above. Net runtime code change: zero. |
