//! `fixture-provider` — minimum-viable wasm component for the
//! `savvagent-plugin-wasm` provider-world adapter tests.
//!
//! Exports a `plugin-provider` world that:
//! - `manifest()` returns a `ProviderManifest` declaring one model
//!   (`"fixture-model-1"`) with `default-model = Some("fixture-model-1")`;
//! - `list-models()` returns the same one-model list (so the host's
//!   `ProviderClient::list_models` round-trip works);
//! - `complete(req)` emits one `ContentBlockDelta::TextDelta { text:
//!   "hi" }` via `progress::emit-stream-event`, then returns a canned
//!   `CompleteResponse` containing a `Text` content block with `"hi"`;
//! - `count-tokens(req)` returns `{ input-tokens: 7 }` (an arbitrary
//!   non-zero so the host test can assert it round-trips).
//!
//! The fixture does **not** exercise the `http-capability` or
//! `keyring-capability` imports — those are the focus of Task 7's
//! denied-host / denied-account fault fixtures, which fail-deliberately
//! and are easier to reason about as separate components.
//!
//! Rebuilt via `just build-fixtures` from the repo root.

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
            provider_id: "fixture".to_string(),
            models: vec![s::ModelInfo {
                id: "fixture-model-1".to_string(),
                display_name: Some("Fixture Model".to_string()),
                context_window: Some(4096),
            }],
            default_model: Some("fixture-model-1".to_string()),
        })
    }

    fn complete(req: s::CompleteRequest) -> Result<s::CompleteResponse, s::ProviderError> {
        // Emit one streaming TextDelta so the host's progress channel
        // actually fires. The host adapter test asserts at least one
        // event was received.
        progress_capability::emit_stream_event(&s::StreamEvent::ContentBlockDelta(
            s::ContentBlockDeltaEvt {
                index: 0,
                delta: s::BlockDelta::TextDelta("hi".to_string()),
            },
        ));

        // Return a canned response. We echo back the requested model so
        // the host can assert that the request actually crossed the WIT
        // boundary correctly.
        Ok(s::CompleteResponse {
            id: "fixture-msg-1".to_string(),
            model: req.model,
            content: vec![s::ContentBlock::Text(s::TextBlock {
                text: "hi".to_string(),
            })],
            stop_reason: s::StopReason::EndTurn,
            stop_sequence: None,
            usage: s::Usage {
                input_tokens: 3,
                output_tokens: 1,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        })
    }

    fn list_models() -> Result<s::ListModelsResponse, s::ProviderError> {
        Ok(s::ListModelsResponse {
            models: vec![s::ModelInfo {
                id: "fixture-model-1".to_string(),
                display_name: Some("Fixture Model".to_string()),
                context_window: Some(4096),
            }],
            default_model_id: Some("fixture-model-1".to_string()),
        })
    }

    fn count_tokens(_req: s::CountTokensRequest) -> Result<s::CountTokensResponse, s::ProviderError> {
        Ok(s::CountTokensResponse { input_tokens: 7 })
    }
}

bindings::export!(Component with_types_in bindings);
