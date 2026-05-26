//! `plugin-hello-provider` — a trivial echo provider.
//!
//! Exports the `plugin-provider` world. The provider id is `"hello-echo"`
//! and it offers one model, `"echo-1"`. `complete` scans the request's
//! `messages` for the last user message, takes the first `Text` block, and
//! returns its contents verbatim.
//!
//! ## Build
//!
//! ```bash
//! cargo component build --release --target wasm32-unknown-unknown
//! ```
//!
//! ## What this example demonstrates
//!
//! - The four required `Guest` methods of the `plugin-provider` world:
//!   `manifest`, `list_models`, `complete`, `count_tokens`.
//! - Emitting a streaming `TextDelta` via the `progress` capability so
//!   the host's progress channel fires at least once. Real providers will
//!   chunk the response token-by-token.
//! - Returning a `CompleteResponse` with `stop-reason = end-turn` and a
//!   populated `usage` record.
//! - A `count_tokens` implementation that returns a deterministic
//!   character-count estimate (no real tokenizer needed for an example).
//!
//! ## What this example does NOT demonstrate
//!
//! - HTTP calls (see the manifest: no `[security]` block).
//! - Keyring access. Real third-party providers read their API key from
//!   the keyring under their own service/account; see the in-tree fixtures
//!   in `crates/savvagent-plugin-wasm/tests/fixtures-src/` for examples.

#[allow(warnings)]
mod bindings;

use bindings::Guest;
use bindings::savvagent::plugin::progress_capability;
use bindings::savvagent::plugin::spp as s;
use bindings::savvagent::plugin::types as t;

struct Component;

impl Guest for Component {
    fn manifest() -> Result<s::ProviderManifest, t::PluginError> {
        Ok(s::ProviderManifest {
            provider_id: "hello-echo".to_string(),
            models: vec![s::ModelInfo {
                id: "echo-1".to_string(),
                display_name: Some("Hello Echo".to_string()),
                context_window: Some(8192),
            }],
            default_model: Some("echo-1".to_string()),
        })
    }

    fn list_models() -> Result<s::ListModelsResponse, s::ProviderError> {
        Ok(s::ListModelsResponse {
            models: vec![s::ModelInfo {
                id: "echo-1".to_string(),
                display_name: Some("Hello Echo".to_string()),
                context_window: Some(8192),
            }],
            default_model_id: Some("echo-1".to_string()),
        })
    }

    /// Echo back the last user message's first text block. If there is no
    /// user message we return an empty completion rather than erroring —
    /// it makes the example forgiving when wired up by hand for the first
    /// time.
    fn complete(req: s::CompleteRequest) -> Result<s::CompleteResponse, s::ProviderError> {
        let echo = last_user_text(&req.messages).unwrap_or_default();

        // Fire a single streaming TextDelta so callers waiting on the
        // progress channel actually receive something. Real providers will
        // chunk the response and emit one delta per token.
        progress_capability::emit_stream_event(&s::StreamEvent::ContentBlockDelta(
            s::ContentBlockDeltaEvt {
                index: 0,
                delta: s::BlockDelta::TextDelta(echo.clone()),
            },
        ));

        let output_tokens = approx_tokens(&echo);
        let input_tokens = total_user_tokens(&req.messages);

        Ok(s::CompleteResponse {
            id: format!("hello-echo-{}", echo.len()),
            model: req.model,
            content: vec![s::ContentBlock::Text(s::TextBlock { text: echo })],
            stop_reason: s::StopReason::EndTurn,
            stop_sequence: None,
            usage: s::Usage {
                input_tokens,
                output_tokens,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        })
    }

    /// A 4-chars-per-token approximation is more than good enough for an
    /// example. Real providers will call into the model's tokenizer.
    fn count_tokens(
        req: s::CountTokensRequest,
    ) -> Result<s::CountTokensResponse, s::ProviderError> {
        Ok(s::CountTokensResponse {
            input_tokens: total_user_tokens(&req.messages),
        })
    }
}

/// Pull the most recent user message's first text block out of the request.
fn last_user_text(messages: &[s::Message]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, s::Role::User))
        .and_then(|m| {
            m.content.iter().find_map(|b| match b {
                s::ContentBlock::Text(tb) => Some(tb.text.clone()),
                _ => None,
            })
        })
}

/// Sum of `approx_tokens` over all user-message text blocks. Used for both
/// `usage.input_tokens` and `count_tokens`.
fn total_user_tokens(messages: &[s::Message]) -> u32 {
    messages
        .iter()
        .filter(|m| matches!(m.role, s::Role::User))
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            s::ContentBlock::Text(tb) => Some(approx_tokens(&tb.text)),
            _ => None,
        })
        .sum()
}

/// Character-count / 4, rounded up, clamped to `u32::MAX`.
fn approx_tokens(text: &str) -> u32 {
    let chars = text.chars().count();
    chars.div_ceil(4).min(u32::MAX as usize) as u32
}

bindings::export!(Component with_types_in bindings);
