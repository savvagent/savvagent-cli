//! `fixture-denied-host` — Task 7 fault fixture.
//!
//! Implements `plugin-provider`. `complete()` deliberately calls
//! `http_capability::fetch` against `https://evil.example/x`. The
//! integration test stages a `plugin.toml` whose
//! `[security] allowed-hosts` is `["api.example.com"]`, so the host's
//! `HttpState::fetch` returns `HttpError::DeniedHost("evil.example")`
//! before the request ever leaves the process.
//!
//! The guest converts the `HttpError` into a `ProviderError` whose
//! `message` carries the denied host string so the test can match on
//! "denied" / "evil" / "host" (case-insensitive).
//!
//! `list-models` and `count-tokens` are present (the world requires
//! them) but unused by the fault test; they return canned values.
//!
//! Rebuilt via `just build-fixture-denied-host` from the repo root.

#[allow(warnings)]
mod bindings;

use bindings::Guest;
use bindings::savvagent::plugin::http_capability as http;
use bindings::savvagent::plugin::spp as s;
use bindings::savvagent::plugin::types as t;

struct Component;

impl Guest for Component {
    fn manifest() -> Result<s::ProviderManifest, t::PluginError> {
        Ok(s::ProviderManifest {
            provider_id: "fixture-denied-host".to_string(),
            models: vec![s::ModelInfo {
                id: "denied-host-model-1".to_string(),
                display_name: Some("Denied Host Model".to_string()),
                context_window: Some(4096),
            }],
            default_model: Some("denied-host-model-1".to_string()),
        })
    }

    fn complete(_req: s::CompleteRequest) -> Result<s::CompleteResponse, s::ProviderError> {
        // Attempt a fetch that the host's allow-list will reject. The
        // resulting `HttpError::DeniedHost(...)` is converted into a
        // `ProviderError` so the integration test sees a stable
        // assert-on-substring path.
        let err = http::fetch(&http::HttpRequest {
            method: "GET".to_string(),
            url: "https://evil.example/x".to_string(),
            headers: vec![],
            body: None,
            timeout_ms: None,
        })
        .err()
        .map(|e| match e {
            http::HttpError::DeniedHost(h) => format!("DeniedHost({h})"),
            other => format!("unexpected HttpError variant: {other:?}"),
        })
        .unwrap_or_else(|| "expected DeniedHost but fetch succeeded".to_string());

        Err(s::ProviderError {
            kind: s::ErrorKind::PermissionDenied,
            message: err,
            retry_after_ms: None,
            provider_code: None,
        })
    }

    fn list_models() -> Result<s::ListModelsResponse, s::ProviderError> {
        Ok(s::ListModelsResponse {
            models: vec![s::ModelInfo {
                id: "denied-host-model-1".to_string(),
                display_name: Some("Denied Host Model".to_string()),
                context_window: Some(4096),
            }],
            default_model_id: Some("denied-host-model-1".to_string()),
        })
    }

    fn count_tokens(
        _req: s::CountTokensRequest,
    ) -> Result<s::CountTokensResponse, s::ProviderError> {
        Ok(s::CountTokensResponse { input_tokens: 0 })
    }
}

bindings::export!(Component with_types_in bindings);
