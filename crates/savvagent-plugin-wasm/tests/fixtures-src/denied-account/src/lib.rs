//! `fixture-denied-account` — Task 7 fault fixture.
//!
//! Implements `plugin-provider`. `complete()` calls
//! `keyring_capability::get("not-listed")`. The integration test stages
//! a `plugin.toml` whose `[security] keyring-accounts` is `["allowed"]`,
//! so the host's `KeyringState::get` returns
//! `KeyringError::Denied("not-listed")` without ever touching the real
//! OS keyring backend.
//!
//! The guest converts the `KeyringError` into a `ProviderError` whose
//! `message` carries the denied account name, so the integration test
//! can match on "denied" / "not-listed" / "keyring".
//!
//! `list-models` and `count-tokens` return canned data; the fault test
//! exercises only `complete`.
//!
//! Rebuilt via `just build-fixture-denied-account` from the repo root.

#[allow(warnings)]
mod bindings;

use bindings::Guest;
use bindings::savvagent::plugin::keyring_capability as keyring;
use bindings::savvagent::plugin::spp as s;
use bindings::savvagent::plugin::types as t;

struct Component;

impl Guest for Component {
    fn manifest() -> Result<s::ProviderManifest, t::PluginError> {
        Ok(s::ProviderManifest {
            provider_id: "fixture-denied-account".to_string(),
            models: vec![s::ModelInfo {
                id: "denied-account-model-1".to_string(),
                display_name: Some("Denied Account Model".to_string()),
                context_window: Some(4096),
            }],
            default_model: Some("denied-account-model-1".to_string()),
        })
    }

    fn complete(_req: s::CompleteRequest) -> Result<s::CompleteResponse, s::ProviderError> {
        // Probe an account name the manifest does NOT declare. The host
        // rejects before touching any keyring backend; the returned
        // `KeyringError::Denied(...)` is converted into a
        // `ProviderError` that the integration test can match on.
        let err = keyring::get("not-listed")
            .err()
            .map(|e| match e {
                keyring::KeyringError::Denied(a) => format!("keyring Denied({a})"),
                keyring::KeyringError::NotFound => "keyring NotFound".to_string(),
                keyring::KeyringError::Backend(b) => format!("keyring Backend({b})"),
            })
            .unwrap_or_else(|| "expected Denied but keyring::get succeeded".to_string());

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
                id: "denied-account-model-1".to_string(),
                display_name: Some("Denied Account Model".to_string()),
                context_window: Some(4096),
            }],
            default_model_id: Some("denied-account-model-1".to_string()),
        })
    }

    fn count_tokens(
        _req: s::CountTokensRequest,
    ) -> Result<s::CountTokensResponse, s::ProviderError> {
        Ok(s::CountTokensResponse { input_tokens: 0 })
    }
}

bindings::export!(Component with_types_in bindings);
