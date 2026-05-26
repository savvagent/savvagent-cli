//! Anthropic Messages API as a Savvagent SPP [`ProviderHandler`].
//!
//! Crate layout:
//!
//! - [`api`] — typed mirror of the relevant subset of the Anthropic
//!   `/v1/messages` request/response shapes.
//! - [`translate`] — pure functions converting between SPP and
//!   [`api`] types.
//! - [`stream`] — Anthropic SSE → SPP [`StreamEvent`](savvagent_protocol::StreamEvent)
//!   adapter.
//! - [`AnthropicProvider`] — [`ProviderHandler`] impl that wires the pieces
//!   together over an HTTP client.
//!
//! The eventual binary wraps an `AnthropicProvider` in an `rmcp` Streamable
//! HTTP server (see `src/main.rs`).

#![allow(clippy::collapsible_if)] // pre-existing debt; many sites under rustc 1.95 new lint
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod api;
pub mod mcp;
pub mod models;
pub mod stream;
pub mod translate;

pub use mcp::AnthropicMcpServer;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use savvagent_mcp::{ProviderHandler, StreamEmitter};
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ErrorKind, ListModelsResponse, ProviderError, StreamEvent,
};

/// Default Anthropic API base URL. Override via [`AnthropicProviderBuilder::base_url`]
/// to point at a proxy or local mock.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// API version header expected by Anthropic.
pub const API_VERSION: &str = "2023-06-01";

/// SPP provider backed by Anthropic's Messages API.
pub struct AnthropicProvider {
    pub(crate) http: reqwest::Client,
    pub(crate) api_key: String,
    pub(crate) base_url: String,
}

/// Builder for [`AnthropicProvider`]. Use [`AnthropicProvider::builder`].
pub struct AnthropicProviderBuilder {
    api_key: Option<String>,
    base_url: String,
    timeout: Duration,
}

impl AnthropicProvider {
    /// Start configuring an [`AnthropicProvider`].
    pub fn builder() -> AnthropicProviderBuilder {
        AnthropicProviderBuilder {
            api_key: None,
            base_url: DEFAULT_BASE_URL.to_string(),
            timeout: Duration::from_secs(120),
        }
    }
}

impl AnthropicProviderBuilder {
    /// Set the API key. If unset, [`build`](Self::build) reads `ANTHROPIC_API_KEY`
    /// from the environment.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Override the API base URL (no trailing slash). Defaults to
    /// [`DEFAULT_BASE_URL`].
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Override the HTTP request timeout. Default is 120 s.
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    /// Build the provider, validating that an API key is available.
    pub fn build(self) -> Result<AnthropicProvider, BuildError> {
        let api_key = self
            .api_key
            .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
            .filter(|k| !k.is_empty())
            .ok_or(BuildError::MissingApiKey)?;
        let http = reqwest::Client::builder()
            .timeout(self.timeout)
            .https_only(self.base_url.starts_with("https://"))
            .build()
            .map_err(|e| BuildError::HttpClient(e.to_string()))?;
        Ok(AnthropicProvider {
            http,
            api_key,
            base_url: self.base_url,
        })
    }
}

/// [`AnthropicProviderBuilder::build`] failures.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// Neither the builder nor `ANTHROPIC_API_KEY` provided an API key.
    #[error("ANTHROPIC_API_KEY is not set and no api_key was provided")]
    MissingApiKey,
    /// The reqwest client failed to construct.
    #[error("failed to build HTTP client: {0}")]
    HttpClient(String),
}

#[async_trait]
impl ProviderHandler for AnthropicProvider {
    async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
        models::list_models(self).await
    }

    async fn complete(
        &self,
        req: CompleteRequest,
        emit: Option<&dyn StreamEmitter>,
    ) -> Result<CompleteResponse, ProviderError> {
        let want_stream = req.stream && emit.is_some();
        let body = translate::request_to_anthropic(&req, want_stream);

        let url = format!("{}/v1/messages", self.base_url);
        tracing::trace!(%url, want_stream, "POSTing to Anthropic");
        let resp = self
            .http
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        tracing::trace!(status = %resp.status(), "Anthropic responded");

        if !resp.status().is_success() {
            return Err(parse_error_response(resp).await);
        }

        if want_stream {
            tracing::trace!("entering SSE consumer");
            let out = stream::consume_sse(resp, emit.unwrap()).await;
            tracing::trace!(ok = out.is_ok(), "SSE consumer returned");
            out
        } else {
            let raw: api::MessageResponse = resp.json().await.map_err(|e| ProviderError {
                kind: ErrorKind::Internal,
                message: format!("failed to parse response body: {e}"),
                retry_after_ms: None,
                provider_code: None,
            })?;
            Ok(translate::response_from_anthropic(raw))
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

/// Map an HTTP status code from any Anthropic REST endpoint to the
/// closest [`ErrorKind`]. Shared between `/v1/messages` and `/v1/models`
/// so a 401 looks like an auth failure no matter which call surfaced it.
pub(crate) fn status_to_error_kind(status: u16) -> ErrorKind {
    match status {
        400 | 422 => ErrorKind::InvalidRequest,
        401 => ErrorKind::Authentication,
        403 => ErrorKind::PermissionDenied,
        404 => ErrorKind::ModelNotFound,
        413 => ErrorKind::ContextLengthExceeded,
        429 => ErrorKind::RateLimited,
        500 | 502 | 503 | 504 | 529 => ErrorKind::Overloaded,
        _ => ErrorKind::Internal,
    }
}

async fn parse_error_response(resp: reqwest::Response) -> ProviderError {
    let status = resp.status();
    let retry_after_ms = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(|s| s * 1000);

    let kind = status_to_error_kind(status.as_u16());

    let body = resp.text().await.unwrap_or_default();
    let (message, provider_code) = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
        let msg = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .map(String::from)
            .unwrap_or_else(|| body.clone());
        let code = v
            .get("error")
            .and_then(|e| e.get("type"))
            .and_then(|t| t.as_str())
            .map(String::from);
        (msg, code)
    } else {
        (body, None)
    };

    ProviderError {
        kind,
        message,
        retry_after_ms,
        provider_code,
    }
}

/// Default endpoint path the Streamable HTTP server is mounted at.
pub const DEFAULT_MCP_PATH: &str = "/mcp";

/// Build the `axum::Router` that serves [`AnthropicMcpServer`] over MCP
/// Streamable HTTP at [`DEFAULT_MCP_PATH`].
///
/// The provider is shared across all sessions; sessions are tracked by an
/// in-process [`LocalSessionManager`].
pub fn router(provider: Arc<AnthropicProvider>) -> axum::Router {
    let provider_for_factory = provider.clone();
    let service = StreamableHttpService::new(
        move || {
            Ok(AnthropicMcpServer::from_shared(
                provider_for_factory.clone(),
            ))
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    axum::Router::new().nest_service(DEFAULT_MCP_PATH, service)
}

/// Default bind address for the standalone `savvagent-anthropic` binary.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8787";

/// Run the standalone Anthropic MCP HTTP server. Reads `ANTHROPIC_API_KEY`,
/// `SAVVAGENT_ANTHROPIC_LISTEN`, and `ANTHROPIC_BASE_URL` from the environment
/// (a `.env` file walking up from the CWD is honored). Shared between this
/// crate's `savvagent-anthropic` binary and the bundled shim in the
/// `savvagent` crate.
pub async fn run() -> std::process::ExitCode {
    use std::env;
    use std::process::ExitCode;

    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let listen =
        env::var("SAVVAGENT_ANTHROPIC_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_string());
    let base_url = env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());

    let provider = match AnthropicProvider::builder().base_url(base_url).build() {
        Ok(p) => Arc::new(p),
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let app = router(provider);

    let listener = match tokio::net::TcpListener::bind(&listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error binding {listen}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let local = listener.local_addr().expect("local_addr");
    tracing::info!(
        "savvagent-anthropic {} listening on http://{local}{DEFAULT_MCP_PATH}",
        env!("CARGO_PKG_VERSION")
    );

    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("ctrl-c received, shutting down");
    };
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
    {
        eprintln!("server error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Useful for tests: an [`AnthropicProvider`] aimed at a local mock server.
#[doc(hidden)]
pub fn provider_for_tests(base_url: impl Into<String>) -> AnthropicProvider {
    AnthropicProvider::builder()
        .api_key("test-key")
        .base_url(base_url)
        .build()
        .expect("test provider should build")
}

#[doc(hidden)]
pub fn _events_phantom(_: StreamEvent) {}
