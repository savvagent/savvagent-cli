//! `web_search`: query a configured search backend and return structured
//! results.
//!
//! Unlike `web_fetch`, this tool has no keyless default: every viable
//! keyless scraping target (DuckDuckGo HTML/Lite, etc.) either violates
//! its terms of service for automated use or actively challenges non-browser
//! clients, so we don't ship a scraper that would silently break or get
//! the user's IP flagged. Instead two backends are supported, selected by
//! whichever is configured (Brave is tried first if both are set):
//!
//! - **Brave Search API** (`SAVVAGENT_BRAVE_API_KEY`, falls back to the
//!   unprefixed `BRAVE_API_KEY`) — hosted, requires a free-tier API key
//!   from <https://brave.com/search/api/>.
//! - **SearXNG** (`SAVVAGENT_SEARXNG_URL`) — points at a self-hosted or
//!   third-party SearXNG instance's base URL (e.g. `http://localhost:8080`)
//!   with its JSON output format enabled; no API key required.
//!
//! If neither is configured, `run` returns a [`WebToolError::NotConfigured`]
//! with setup instructions for both options rather than failing silently.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::WebToolError;

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Default cap on results returned. Overridable per call via
/// [`SearchInput::max_results`].
pub const DEFAULT_MAX_RESULTS: u32 = 10;

/// Input for the `web_search` tool.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SearchInput {
    /// The search query string.
    pub query: String,
    /// Cap on results returned. Defaults to [`DEFAULT_MAX_RESULTS`].
    #[serde(default)]
    pub max_results: Option<u32>,
}

/// A single search result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResult {
    /// Result title.
    pub title: String,
    /// Result URL.
    pub url: String,
    /// Short snippet/description, if the backend provided one.
    #[serde(default)]
    pub snippet: String,
}

/// Output of the `web_search` tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct SearchOutput {
    /// Which backend served this query (`"brave"` or `"searxng"`).
    pub backend: String,
    /// Matching results, in the backend's ranking order.
    pub results: Vec<SearchResult>,
}

/// Run a search against whichever backend is configured, preferring Brave
/// when both are set.
pub async fn run(input: SearchInput) -> Result<SearchOutput, WebToolError> {
    if input.query.trim().is_empty() {
        return Err(WebToolError::InvalidArgument(
            "query must not be empty".into(),
        ));
    }
    let max_results = input
        .max_results
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(1, 50);

    if let Ok(key) =
        std::env::var("SAVVAGENT_BRAVE_API_KEY").or_else(|_| std::env::var("BRAVE_API_KEY"))
    {
        return brave_search(&input.query, max_results, &key).await;
    }
    if let Ok(base_url) = std::env::var("SAVVAGENT_SEARXNG_URL") {
        return searxng_search(&input.query, max_results, &base_url).await;
    }
    Err(WebToolError::NotConfigured(
        "web_search has no backend configured. Set SAVVAGENT_BRAVE_API_KEY \
         (get a free-tier key at https://brave.com/search/api/) or \
         SAVVAGENT_SEARXNG_URL (base URL of a SearXNG instance with JSON \
         output enabled, e.g. http://localhost:8080)."
            .into(),
    ))
}

#[derive(Deserialize)]
struct BraveResponse {
    #[serde(default)]
    web: Option<BraveWeb>,
}
#[derive(Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveResult>,
}
#[derive(Deserialize)]
struct BraveResult {
    title: String,
    url: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct SearxResponse {
    #[serde(default)]
    results: Vec<SearxResult>,
}
#[derive(Deserialize)]
struct SearxResult {
    title: String,
    url: String,
    #[serde(default)]
    content: String,
}

async fn brave_search(
    query: &str,
    max_results: u32,
    api_key: &str,
) -> Result<SearchOutput, WebToolError> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| WebToolError::Network(format!("building HTTP client failed: {e}")))?;

    let resp = client
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("Accept", "application/json")
        .header("X-Subscription-Token", api_key)
        .query(&[("q", query), ("count", &max_results.to_string())])
        .send()
        .await
        .map_err(|e| WebToolError::Network(format!("brave search request failed: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(WebToolError::Network(format!(
            "brave search returned {status}: {body}"
        )));
    }

    let parsed: BraveResponse = resp
        .json()
        .await
        .map_err(|e| WebToolError::Network(format!("parsing brave search response failed: {e}")))?;

    let results = parsed
        .web
        .map(|w| w.results)
        .unwrap_or_default()
        .into_iter()
        .take(max_results as usize)
        .map(|r| SearchResult {
            title: r.title,
            url: r.url,
            snippet: r.description,
        })
        .collect();

    Ok(SearchOutput {
        backend: "brave".into(),
        results,
    })
}

async fn searxng_search(
    query: &str,
    max_results: u32,
    base_url: &str,
) -> Result<SearchOutput, WebToolError> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| WebToolError::Network(format!("building HTTP client failed: {e}")))?;

    let search_url = format!("{}/search", base_url.trim_end_matches('/'));
    let resp = client
        .get(&search_url)
        .query(&[("q", query), ("format", "json")])
        .send()
        .await
        .map_err(|e| {
            WebToolError::Network(format!("searxng request to {search_url} failed: {e}"))
        })?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(WebToolError::Network(format!(
            "searxng returned {status}: {body}"
        )));
    }

    let parsed: SearxResponse = resp
        .json()
        .await
        .map_err(|e| WebToolError::Network(format!("parsing searxng response failed: {e}")))?;

    let results = parsed
        .results
        .into_iter()
        .take(max_results as usize)
        .map(|r| SearchResult {
            title: r.title,
            url: r.url,
            snippet: r.content,
        })
        .collect();

    Ok(SearchOutput {
        backend: "searxng".into(),
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json as AxumJson, Router, routing::get};
    use serde_json::json;
    use serial_test::serial;

    #[test]
    fn brave_response_deserializes_expected_shape() {
        let raw = json!({
            "web": {
                "results": [
                    {"title": "Rust", "url": "https://rust-lang.org", "description": "A language"},
                    {"title": "No desc", "url": "https://example.com"}
                ]
            }
        });
        let parsed: BraveResponse = serde_json::from_value(raw).unwrap();
        let results = parsed.web.unwrap().results;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust");
        assert_eq!(results[1].description, "");
    }

    #[test]
    fn brave_response_tolerates_missing_web_key() {
        let parsed: BraveResponse = serde_json::from_value(json!({})).unwrap();
        assert!(parsed.web.is_none());
    }

    #[tokio::test]
    #[serial]
    async fn searxng_search_parses_mock_server_response() {
        let app = Router::new().route(
            "/search",
            get(|| async {
                AxumJson(json!({
                    "results": [
                        {"title": "Result A", "url": "https://a.example", "content": "snippet A"},
                    ]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // SAFETY: guarded by ENV_LOCK above; no other test in this crate
        // touches SAVVAGENT_SEARXNG_URL without also holding it.
        unsafe {
            std::env::remove_var("SAVVAGENT_BRAVE_API_KEY");
            std::env::remove_var("BRAVE_API_KEY");
            std::env::set_var("SAVVAGENT_SEARXNG_URL", format!("http://{addr}"));
        }

        let out = run(SearchInput {
            query: "rust".into(),
            max_results: None,
        })
        .await
        .unwrap();

        unsafe {
            std::env::remove_var("SAVVAGENT_SEARXNG_URL");
        }

        assert_eq!(out.backend, "searxng");
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].title, "Result A");
        assert_eq!(out.results[0].snippet, "snippet A");
    }

    #[tokio::test]
    async fn rejects_empty_query() {
        let err = run(SearchInput {
            query: "   ".into(),
            max_results: None,
        })
        .await
        .unwrap_err();
        assert!(matches!(err, WebToolError::InvalidArgument(_)));
    }
}
