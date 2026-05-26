//! Ollama HTTP API as a Savvagent SPP [`ProviderHandler`].
//!
//! Crate layout:
//!
//! - [`api`] — typed subset of Ollama's `/api/chat` request/response shapes.
//! - [`translate`] — pure functions converting between SPP and [`api`] types.
//! - [`stream`] — Ollama NDJSON stream → SPP [`StreamEvent`] adapter.
//!
//! # Example
//!
//! ```no_run
//! use provider_local::OllamaProvider;
//!
//! let provider = OllamaProvider::builder()
//!     .base_url("http://localhost:11434")
//!     .build()
//!     .expect("failed to build Ollama provider");
//! ```
//!
//! The provider is keyless — no API key is required. The `OLLAMA_HOST` env
//! var overrides the base URL at runtime (Ollama's own convention).

#![allow(clippy::collapsible_if)] // pre-existing debt; many sites under rustc 1.95 new lint
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod api;
pub mod stream;
pub mod translate;

use std::time::Duration;

use async_trait::async_trait;
use savvagent_mcp::{ProviderHandler, StreamEmitter};
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ErrorKind, ListModelsResponse, ModelInfo, ProviderError,
    StreamEvent,
};

/// Default Ollama base URL. Override via [`OllamaProviderBuilder::base_url`]
/// or the `OLLAMA_HOST` environment variable.
pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// Default model used when none is specified in the request.
pub const DEFAULT_MODEL: &str = "llama3.2";

/// SPP provider backed by Ollama's `/api/chat` endpoint.
pub struct OllamaProvider {
    http: reqwest::Client,
    base_url: String,
}

impl OllamaProvider {
    /// Lightweight health probe. Issues a short-timeout `GET /api/tags`
    /// against the configured base URL and returns a typed
    /// [`ProviderError`] when Ollama is unreachable.
    ///
    /// Intended for the connect path so the user gets a useful "is `ollama
    /// serve` running?" message instead of a successful "Connected" notice
    /// followed by the first turn timing out.
    pub async fn ready(&self) -> Result<(), ProviderError> {
        let url = format!("{}/api/tags", self.base_url);
        let resp = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map_err(|e| ProviderError {
                kind: ErrorKind::Network,
                message: format!(
                    "Ollama not reachable at {} — is `ollama serve` running? ({e})",
                    self.base_url
                ),
                retry_after_ms: None,
                provider_code: None,
            })?;
        if !resp.status().is_success() {
            return Err(ProviderError {
                kind: ErrorKind::Network,
                message: format!(
                    "Ollama health probe at {} returned HTTP {} — is `ollama serve` running?",
                    self.base_url,
                    resp.status()
                ),
                retry_after_ms: None,
                provider_code: None,
            });
        }
        Ok(())
    }

    /// Configured Ollama base URL (no trailing slash).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// Builder for [`OllamaProvider`]. Use [`OllamaProvider::builder`].
pub struct OllamaProviderBuilder {
    base_url: String,
    timeout: Duration,
}

impl OllamaProvider {
    /// Start configuring an [`OllamaProvider`].
    pub fn builder() -> OllamaProviderBuilder {
        OllamaProviderBuilder {
            base_url: DEFAULT_BASE_URL.to_string(),
            timeout: Duration::from_secs(300),
        }
    }
}

impl OllamaProviderBuilder {
    /// Override the Ollama base URL (no trailing slash). If unset,
    /// [`build`](Self::build) reads `OLLAMA_HOST` from the environment,
    /// falling back to `http://localhost:11434`.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Override the HTTP request timeout. Default is 300 s (local inference
    /// can be slow on first load).
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    /// Build the provider. Reads `OLLAMA_HOST` from the environment when no
    /// explicit base URL was set via [`base_url`](Self::base_url).
    pub fn build(mut self) -> Result<OllamaProvider, BuildError> {
        // Honor OLLAMA_HOST if the caller did not set an explicit URL. Ollama's
        // own tooling uses this env var.
        if self.base_url == DEFAULT_BASE_URL {
            if let Ok(host) = std::env::var("OLLAMA_HOST") {
                if !host.is_empty() {
                    self.base_url = host;
                }
            }
        }
        let http = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| BuildError::HttpClient(e.to_string()))?;
        Ok(OllamaProvider {
            http,
            base_url: self.base_url,
        })
    }
}

/// [`OllamaProviderBuilder::build`] failures.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// The reqwest client failed to construct.
    #[error("failed to build HTTP client: {0}")]
    HttpClient(String),
}

#[async_trait]
impl ProviderHandler for OllamaProvider {
    async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
        let url = format!("{}/api/tags", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_status_error("Ollama /api/tags", status, body));
        }
        #[derive(serde::Deserialize)]
        struct Tags {
            models: Vec<Tag>,
        }
        #[derive(serde::Deserialize)]
        struct Tag {
            name: String,
        }
        let tags: Tags = resp.json().await.map_err(|e| ProviderError {
            kind: ErrorKind::Internal,
            message: format!("failed to parse /api/tags: {e}"),
            retry_after_ms: None,
            provider_code: None,
        })?;
        let models: Vec<ModelInfo> = tags
            .models
            .into_iter()
            .map(|t| ModelInfo {
                id: t.name.clone(),
                display_name: Some(t.name),
                context_window: None,
            })
            .collect();
        // Advertise DEFAULT_MODEL as the default only when it appears in tags;
        // a user that hasn't pulled `llama3.2` shouldn't see it as default.
        let default_model_id = models
            .iter()
            .any(|m| m.id == DEFAULT_MODEL)
            .then(|| DEFAULT_MODEL.to_string());
        Ok(ListModelsResponse {
            models,
            default_model_id,
        })
    }

    async fn complete(
        &self,
        req: CompleteRequest,
        emit: Option<&dyn StreamEmitter>,
    ) -> Result<CompleteResponse, ProviderError> {
        let want_stream = req.stream && emit.is_some();
        let body = translate::request_to_ollama(&req, want_stream);
        let url = format!("{}/api/chat", self.base_url);

        tracing::trace!(%url, want_stream, model = %req.model, "POSTing to Ollama");
        let resp = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        tracing::trace!(status = %resp.status(), "Ollama responded");

        if !resp.status().is_success() {
            return Err(parse_error_response(resp).await);
        }

        if want_stream {
            tracing::trace!("entering NDJSON consumer");
            let out = stream::consume_ndjson(resp, emit.unwrap()).await;
            tracing::trace!(ok = out.is_ok(), "NDJSON consumer returned");
            out
        } else {
            let raw: api::ChatResponse = resp.json().await.map_err(|e| ProviderError {
                kind: ErrorKind::Internal,
                message: format!("failed to parse response body: {e}"),
                retry_after_ms: None,
                provider_code: None,
            })?;
            Ok(translate::response_from_ollama(raw))
        }
    }
}

fn map_reqwest_error(e: reqwest::Error) -> ProviderError {
    let kind = if e.is_timeout() || e.is_connect() {
        ErrorKind::Network
    } else {
        ErrorKind::Internal
    };
    ProviderError {
        kind,
        message: e.to_string(),
        retry_after_ms: None,
        provider_code: None,
    }
}

/// Build a `Network`-kind [`ProviderError`] that surfaces the response body
/// alongside the HTTP status, truncated at 512 bytes.
fn http_status_error(label: &str, status: reqwest::StatusCode, body: String) -> ProviderError {
    let truncated = if body.len() > 512 {
        format!("{}…", &body[..512])
    } else {
        body
    };
    let message = if truncated.is_empty() {
        format!("{label} returned HTTP {status}")
    } else {
        format!("{label} returned HTTP {status}: {truncated}")
    };
    ProviderError {
        kind: ErrorKind::Network,
        message,
        retry_after_ms: None,
        provider_code: None,
    }
}

async fn parse_error_response(resp: reqwest::Response) -> ProviderError {
    let status = resp.status();
    let kind = match status.as_u16() {
        400 => ErrorKind::InvalidRequest,
        401 => ErrorKind::Authentication,
        403 => ErrorKind::PermissionDenied,
        404 => ErrorKind::ModelNotFound,
        429 => ErrorKind::RateLimited,
        500 | 502 | 503 | 504 => ErrorKind::Overloaded,
        _ => ErrorKind::Internal,
    };
    let body = resp.text().await.unwrap_or_default();
    // Ollama error shape: { "error": "message" }
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(String::from))
        .unwrap_or(body);
    ProviderError {
        kind,
        message,
        retry_after_ms: None,
        provider_code: None,
    }
}

/// Useful for tests: an [`OllamaProvider`] aimed at a local mock server.
#[doc(hidden)]
pub fn provider_for_tests(base_url: impl Into<String>) -> OllamaProvider {
    OllamaProvider::builder()
        .base_url(base_url)
        .build()
        .expect("test provider should build")
}

#[doc(hidden)]
pub fn _events_phantom(_: StreamEvent) {}

#[cfg(test)]
mod ready_tests {
    use super::*;
    use axum::{Router, http::StatusCode, response::IntoResponse, routing::get};

    #[tokio::test]
    async fn ready_succeeds_when_api_tags_returns_ok() {
        let app = Router::new().route(
            "/api/tags",
            get(|| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    "{\"models\":[]}",
                )
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let provider = provider_for_tests(format!("http://{addr}"));
        provider.ready().await.expect("ready should succeed");
    }

    #[tokio::test]
    async fn ready_fails_when_ollama_is_unreachable() {
        // Bind a port, drop the listener, then point the provider at it.
        // No server is listening — the connection attempt must fail fast
        // and the error must call out the URL and `ollama serve`.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let provider = provider_for_tests(format!("http://{addr}"));
        let err = provider.ready().await.expect_err("ready must fail");
        assert!(matches!(err.kind, ErrorKind::Network), "kind: {:?}", err);
        assert!(
            err.message.contains("Ollama not reachable") && err.message.contains("ollama serve"),
            "expected actionable message, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn ready_fails_on_non_2xx_status() {
        let app = Router::new().route(
            "/api/tags",
            get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response() }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let provider = provider_for_tests(format!("http://{addr}"));
        let err = provider.ready().await.expect_err("ready must fail on 5xx");
        assert!(matches!(err.kind, ErrorKind::Network), "kind: {:?}", err);
    }
}

#[cfg(test)]
mod list_models_tests {
    use super::*;
    use axum::{Json, Router, routing::get};
    use serde_json::json;

    #[tokio::test]
    async fn list_models_parses_api_tags() {
        let app = Router::new().route(
            "/api/tags",
            get(|| async {
                Json(json!({
                    "models": [
                        {"name": "llama3.2", "model": "llama3.2", "size": 0},
                        {"name": "qwen2.5-coder:7b", "model": "qwen2.5-coder:7b", "size": 0}
                    ]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let provider = provider_for_tests(format!("http://{addr}"));
        let resp = provider.list_models().await.unwrap();
        let ids: Vec<_> = resp.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["llama3.2", "qwen2.5-coder:7b"]);
        // `llama3.2` matches DEFAULT_MODEL so it's advertised as the default.
        assert_eq!(resp.default_model_id, Some(DEFAULT_MODEL.to_string()));
    }

    #[tokio::test]
    async fn list_models_default_model_id_none_when_not_pulled() {
        let app = Router::new().route(
            "/api/tags",
            get(|| async {
                Json(json!({
                    "models": [
                        {"name": "qwen2.5-coder:7b", "model": "qwen2.5-coder:7b", "size": 0}
                    ]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let provider = provider_for_tests(format!("http://{addr}"));
        let resp = provider.list_models().await.unwrap();
        assert!(!resp.models.iter().any(|m| m.id == DEFAULT_MODEL));
        assert_eq!(resp.default_model_id, None);
    }

    #[tokio::test]
    async fn list_models_propagates_http_failure() {
        let app = Router::new().route(
            "/api/tags",
            get(|| async {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    r#"{"error":"ollama_overloaded"}"#,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let provider = provider_for_tests(format!("http://{addr}"));
        let err = provider.list_models().await.expect_err("must fail on 5xx");
        assert!(matches!(err.kind, ErrorKind::Network), "kind: {:?}", err);
        assert!(err.message.contains("HTTP 500"), "msg: {}", err.message);
        assert!(
            err.message.contains("ollama_overloaded"),
            "msg: {}",
            err.message
        );
    }
}
