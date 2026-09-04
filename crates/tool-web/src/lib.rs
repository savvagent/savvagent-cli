//! Web access tools as a Model Context Protocol stdio server.
//!
//! Exposes two tools an agent host can call over MCP:
//!
//! - [`web_fetch`](WebTools::web_fetch) — retrieve a URL and return
//!   readable text (HTML is converted to plain text; other content types
//!   pass through as-is).
//! - [`web_search`](WebTools::web_search) — query a configured search
//!   backend (Brave Search API or a self-hosted SearXNG instance) and
//!   return structured `{title, url, snippet}` results.
//!
//! Both tools carry their own safety layer in place of `tool-fs`'s path
//! containment: `web_fetch` guards against SSRF by resolving hostnames
//! and rejecting loopback/private/link-local targets before connecting
//! (see [`fetch::is_blocked_addr`]); `web_search` never scrapes — it only
//! ever calls an explicitly configured backend, and fails with setup
//! instructions rather than silently no-op'ing when none is configured.

#![cfg_attr(not(test), forbid(unsafe_code))]
#![warn(missing_docs)]

pub use fetch::{DEFAULT_MAX_CHARS, FetchInput, FetchOutput};
pub use search::{DEFAULT_MAX_RESULTS, SearchInput, SearchOutput, SearchResult};

mod fetch;
mod search;

use rmcp::{
    ErrorData, ServerHandler,
    handler::server::{
        router::tool::ToolRouter,
        wrapper::{Json, Parameters},
    },
    model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};

/// Errors a tool handler can surface to the caller.
#[derive(Debug, thiserror::Error)]
pub enum WebToolError {
    /// Caller passed an invalid argument.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// The requested backend isn't configured (`web_search` only).
    #[error("not configured: {0}")]
    NotConfigured(String),
    /// The HTTP request itself failed (DNS, TLS, timeout, non-2xx, etc.).
    #[error("network error: {0}")]
    Network(String),
}

impl From<WebToolError> for ErrorData {
    fn from(err: WebToolError) -> Self {
        match err {
            WebToolError::InvalidArgument(_) => ErrorData::invalid_params(err.to_string(), None),
            WebToolError::NotConfigured(_) => ErrorData::invalid_request(err.to_string(), None),
            WebToolError::Network(_) => ErrorData::internal_error(err.to_string(), None),
        }
    }
}

/// MCP server exposing the `web_fetch` and `web_search` tools.
#[derive(Debug, Clone, Default)]
pub struct WebTools {
    #[allow(dead_code)] // Read by the `#[tool_handler]` macro expansion.
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl WebTools {
    /// Construct a new server instance.
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    /// Fetch a URL and return readable text.
    #[tool(
        name = "web_fetch",
        description = "Fetch a URL over http/https and return its content as readable text (HTML is converted to plain text). Rejects requests to loopback, private, and link-local addresses. Args: {url, max_chars?, raw?}."
    )]
    pub async fn web_fetch(
        &self,
        Parameters(input): Parameters<fetch::FetchInput>,
    ) -> Result<Json<fetch::FetchOutput>, ErrorData> {
        Ok(Json(fetch::run(input).await?))
    }

    /// Query a configured search backend.
    #[tool(
        name = "web_search",
        description = "Search the web via a configured backend (Brave Search API or SearXNG) and return {title, url, snippet} results. Requires SAVVAGENT_BRAVE_API_KEY or SAVVAGENT_SEARXNG_URL to be set. Args: {query, max_results?}."
    )]
    pub async fn web_search(
        &self,
        Parameters(input): Parameters<search::SearchInput>,
    ) -> Result<Json<search::SearchOutput>, ErrorData> {
        Ok(Json(search::run(input).await?))
    }
}

#[tool_handler]
impl ServerHandler for WebTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::default())
            .with_server_info(
                Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
                    .with_description(
                        "Savvagent web-access tool server (web_fetch, web_search). \
                         web_fetch enforces an SSRF guard on every request; \
                         web_search requires an explicitly configured backend.",
                    ),
            )
            .with_instructions(
                "Web access tool server for Savvagent. web_fetch retrieves a URL as \
                 readable text and refuses loopback/private/link-local targets. \
                 web_search requires SAVVAGENT_BRAVE_API_KEY or SAVVAGENT_SEARXNG_URL \
                 to be set and returns setup instructions otherwise.",
            )
    }
}

/// Serve [`WebTools`] over a stdio MCP transport. Shared between the
/// `savvagent-tool-web` binary and the bundled shim in the `savvagent`
/// crate's release archive.
pub async fn run() -> anyhow::Result<()> {
    use rmcp::{ServiceExt, transport::stdio};

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    tracing::info!(
        "savvagent-tool-web {} starting on stdio",
        env!("CARGO_PKG_VERSION")
    );

    let tools = WebTools::new();
    let service = tools.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod mcp_tests {
    use super::*;
    use rmcp::handler::server::wrapper::Parameters;

    /// `Result::unwrap_err` requires `T: Debug`, which `Json<_>` doesn't
    /// implement. This sidesteps that bound for these error-path tests.
    fn expect_err<T>(result: Result<T, ErrorData>) -> ErrorData {
        match result {
            Ok(_) => panic!("expected Err, got Ok"),
            Err(e) => e,
        }
    }

    #[tokio::test]
    async fn mcp_web_fetch_rejects_scheme() {
        let tools = WebTools::new();
        let err = expect_err(
            tools
                .web_fetch(Parameters(fetch::FetchInput {
                    url: "ftp://example.com/file".into(),
                    max_chars: None,
                    raw: false,
                }))
                .await,
        );
        assert!(err.message.contains("scheme"));
    }

    #[tokio::test]
    async fn mcp_web_fetch_rejects_loopback_target() {
        // The SSRF guard treats 127.0.0.1 as a blocked literal address,
        // independent of DNS — so even a test server the caller controls
        // can't be reached via web_fetch. This is the intended
        // conservative behavior, not a test-environment limitation.
        let tools = WebTools::new();
        let err = expect_err(
            tools
                .web_fetch(Parameters(fetch::FetchInput {
                    url: "http://127.0.0.1:1/".into(),
                    max_chars: None,
                    raw: false,
                }))
                .await,
        );
        assert!(err.message.to_lowercase().contains("network"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn mcp_web_search_without_backend_configured_errors() {
        // SAFETY: guarded by `#[serial]`, shared with search.rs's tests that
        // also mutate these same env vars.
        unsafe {
            std::env::remove_var("SAVVAGENT_BRAVE_API_KEY");
            std::env::remove_var("BRAVE_API_KEY");
            std::env::remove_var("SAVVAGENT_SEARXNG_URL");
        }
        let tools = WebTools::new();
        let err = expect_err(
            tools
                .web_search(Parameters(search::SearchInput {
                    query: "rust".into(),
                    max_results: None,
                }))
                .await,
        );
        assert!(err.message.contains("not configured"));
    }
}
