//! Anthropic `/v1/models` listing — GET the live catalog and translate into
//! SPP [`ListModelsResponse`].
//!
//! Mirrors `provider-gemini/src/models.rs`: hit the catalog endpoint, decode
//! a small private struct that names only the fields we care about, then map
//! into the SPP envelope.

use savvagent_protocol::{ErrorKind, ListModelsResponse, ModelInfo, ProviderError};
use serde::Deserialize;

use crate::{API_VERSION, AnthropicProvider, map_reqwest_error, status_to_error_kind};

/// Truncate `body` to at most `max_bytes`, snapping back to the previous
/// UTF-8 char boundary so `&body[..n]` never panics on multi-byte sequences.
fn truncate_at_char_boundary(body: &str, max_bytes: usize) -> &str {
    if body.len() <= max_bytes {
        return body;
    }
    let mut end = max_bytes;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    &body[..end]
}

/// The model id we report as `default_model_id` when it appears in the
/// catalog. Keep in sync with `crates/savvagent/src/providers.rs`'s
/// Anthropic `default_model`.
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5";

#[derive(Debug, Deserialize)]
struct ModelsList {
    #[serde(default)]
    data: Vec<RawModel>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
}

/// Query Anthropic's `/v1/models` endpoint and return a [`ListModelsResponse`]
/// whose `default_model_id` is [`DEFAULT_MODEL`] when present in the catalog,
/// otherwise the first model id.
pub async fn list_models(
    provider: &AnthropicProvider,
) -> Result<ListModelsResponse, ProviderError> {
    // `limit=1000` is the API's hard max — one request returns everything.
    let url = format!("{}/v1/models?limit=1000", provider.base_url);
    let resp = provider
        .http
        .get(&url)
        .header("x-api-key", &provider.api_key)
        .header("anthropic-version", API_VERSION)
        .send()
        .await
        .map_err(map_reqwest_error)?;

    if !resp.status().is_success() {
        let status = resp.status();
        let kind = status_to_error_kind(status.as_u16());
        let body = resp.text().await.unwrap_or_default();
        let snippet = truncate_at_char_boundary(&body, 512);
        let message = if snippet.is_empty() {
            format!("Anthropic /v1/models returned HTTP {status}")
        } else if snippet.len() == body.len() {
            format!("Anthropic /v1/models returned HTTP {status}: {snippet}")
        } else {
            format!("Anthropic /v1/models returned HTTP {status}: {snippet}…")
        };
        return Err(ProviderError {
            kind,
            message,
            retry_after_ms: None,
            provider_code: None,
        });
    }

    let raw: ModelsList = resp.json().await.map_err(|e| ProviderError {
        kind: ErrorKind::Internal,
        message: format!("failed to parse Anthropic /v1/models: {e}"),
        retry_after_ms: None,
        provider_code: None,
    })?;

    let models: Vec<ModelInfo> = raw
        .data
        .into_iter()
        .map(|m| ModelInfo {
            id: m.id,
            display_name: m.display_name,
            context_window: None,
        })
        .collect();

    let default_model_id = if models.iter().any(|m| m.id == DEFAULT_MODEL) {
        Some(DEFAULT_MODEL.to_string())
    } else {
        models.first().map(|m| m.id.clone())
    };

    Ok(ListModelsResponse {
        models,
        default_model_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AnthropicProvider;
    use axum::{Json, Router, routing::get};
    use serde_json::json;

    async fn spawn_mock(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn provider_for(base: String) -> AnthropicProvider {
        AnthropicProvider::builder()
            .api_key("test-key")
            .base_url(base)
            .build()
            .expect("test provider must build")
    }

    #[tokio::test]
    async fn list_models_decodes_data_array() {
        let app = Router::new().route(
            "/v1/models",
            get(|| async {
                Json(json!({
                    "data": [
                        { "id": "claude-opus-4-5-20251001", "display_name": "Claude Opus 4.5" },
                        { "id": "claude-haiku-4-5", "display_name": "Claude Haiku 4.5" }
                    ],
                    "has_more": false
                }))
            }),
        );
        let base = spawn_mock(app).await;
        let provider = provider_for(base);
        let resp = list_models(&provider).await.expect("should succeed");

        let ids: Vec<&str> = resp.models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"claude-opus-4-5-20251001"), "{ids:?}");
        assert!(ids.contains(&"claude-haiku-4-5"), "{ids:?}");
    }

    #[tokio::test]
    async fn list_models_default_is_haiku_when_present() {
        let app = Router::new().route(
            "/v1/models",
            get(|| async {
                Json(json!({
                    "data": [
                        { "id": "claude-opus-4-5-20251001", "display_name": "Claude Opus 4.5" },
                        { "id": "claude-haiku-4-5", "display_name": "Claude Haiku 4.5" }
                    ]
                }))
            }),
        );
        let base = spawn_mock(app).await;
        let provider = provider_for(base);
        let resp = list_models(&provider).await.expect("should succeed");
        assert_eq!(resp.default_model_id, Some(DEFAULT_MODEL.to_string()));
    }

    #[tokio::test]
    async fn list_models_default_falls_back_to_first_when_canonical_missing() {
        let app = Router::new().route(
            "/v1/models",
            get(|| async {
                Json(json!({
                    "data": [
                        { "id": "claude-opus-4-5", "display_name": "Claude Opus 4.5" }
                    ]
                }))
            }),
        );
        let base = spawn_mock(app).await;
        let provider = provider_for(base);
        let resp = list_models(&provider).await.expect("should succeed");
        assert_eq!(resp.default_model_id, Some("claude-opus-4-5".to_string()));
    }

    #[tokio::test]
    async fn list_models_401_maps_to_authentication() {
        let app = Router::new().route(
            "/v1/models",
            get(|| async {
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    r#"{"error":{"message":"invalid api key","type":"authentication_error"}}"#,
                )
            }),
        );
        let base = spawn_mock(app).await;
        let provider = provider_for(base);
        let err = list_models(&provider)
            .await
            .expect_err("401 must surface as ProviderError");
        assert!(
            matches!(err.kind, ErrorKind::Authentication),
            "kind: {:?}",
            err
        );
        assert!(err.message.contains("HTTP 401"), "msg: {}", err.message);
        assert!(
            err.message.contains("invalid api key"),
            "msg: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn list_models_5xx_maps_to_overloaded() {
        let app = Router::new().route(
            "/v1/models",
            get(|| async {
                (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "service unavailable",
                )
            }),
        );
        let base = spawn_mock(app).await;
        let provider = provider_for(base);
        let err = list_models(&provider).await.expect_err("503 must error");
        assert!(matches!(err.kind, ErrorKind::Overloaded), "kind: {:?}", err);
    }

    #[test]
    fn truncate_at_char_boundary_handles_multibyte() {
        // A 3-byte UTF-8 codepoint straddling the requested boundary must
        // snap back instead of panicking on `&body[..n]`.
        let body = format!("{}\u{1F4A9}tail", "a".repeat(510)); // 510 + 4 + 4 = 518 bytes
        let snipped = truncate_at_char_boundary(&body, 512);
        // Boundary snaps to byte 510 (the start of the emoji).
        assert_eq!(snipped.len(), 510);
        assert!(snipped.is_char_boundary(snipped.len()));
    }

    #[test]
    fn truncate_at_char_boundary_passthrough_for_short_body() {
        let body = "short";
        assert_eq!(truncate_at_char_boundary(body, 512), "short");
    }
}
