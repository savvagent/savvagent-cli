//! `http-capability` host import for provider plugins.
//!
//! Implements the WIT `http-capability` interface declared in
//! `plugin-provider.wit`: exactly one entry point, `fetch`, plus the
//! manifest-driven `allowed-hosts` filter that runs on every call.
//!
//! ## Capability model
//!
//! The `allowed-hosts` list comes from the plugin's `plugin.toml`
//! `[security]` table at manifest-load time. It is a **host-only
//! allow-list**: each entry must equal the URL's `host_str` exactly
//! (after lowercase normalization). The port is excluded from the
//! comparison — an entry `api.example.com` matches every port on that
//! host. No wildcards (`*.example.com`), no IDN normalization beyond
//! what `url::Url::host_str` already does. This is intentional for
//! v0.18.0; we'd rather miss a few edge cases than ship a misconfigured
//! wildcard.
//!
//! Method allow-list is a fixed set in v1: GET, POST, PUT, DELETE, HEAD,
//! PATCH. Any other method (including CONNECT and TRACE) returns
//! `DeniedMethod`.
//!
//! Per-request timeout is capped at 300_000 ms (5 minutes). Body size is
//! capped at 32 MiB. Both limits are enforced after the response is
//! received, before it crosses back into wasm.
//!
//! `fetch-stream` (the SSE/chunked-body variant) is **not implemented**
//! in v0.18.0. Its host wiring requires resource-table management that
//! we'll add in a follow-up; the current stub returns
//! `HttpError::Transport("fetch-stream not yet supported")` so a
//! provider plugin that tries to use it gets a controlled error rather
//! than a wasm trap.
//!
//! ## Concurrency
//!
//! `HttpState` holds a single `reqwest::Client`. `reqwest::Client`'s
//! connection pool is `Sync` and cheap to share, so we hand it out
//! through `Arc<HttpState>` to every per-call `Store`.

use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;

use crate::provider_world::savvagent::plugin::http_capability as wit;

/// Maximum per-request timeout the host will honor. The plugin can pass
/// any value via `HttpRequest.timeout-ms`; values above this ceiling are
/// silently clamped.
const MAX_TIMEOUT_MS: u64 = 300_000;

/// Maximum response body the host will return to wasm. Anything larger
/// surfaces as `HttpError::BodyTooLarge`.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// Per-store HTTP state. Holds the reqwest client and the
/// manifest-derived allow-list. Cloning is cheap (internal `Arc`s).
#[derive(Clone)]
pub struct HttpState {
    /// Shared HTTP client. Rustls-only TLS, no system-root trust store
    /// (matches the host's existing `reqwest` configuration). Connection
    /// pool is `Sync`.
    pub client: Client,
    /// Literal-match allow-list. Empty list = deny every request.
    pub allowed_hosts: Arc<Vec<String>>,
}

impl HttpState {
    /// Build a fresh HTTP state for one plugin's per-call Store.
    ///
    /// `allowed_hosts` is the list from the manifest's `[security]`
    /// table — an empty Vec is a valid input (the plugin can construct
    /// HTTP requests but every one will be denied at the allowlist
    /// check). The `reqwest::Client` is built with a generous 300s
    /// top-level timeout; per-request `timeout-ms` overrides this where
    /// the plugin asks for a smaller value.
    pub fn new(allowed_hosts: Vec<String>) -> Self {
        // The `expect` here is sound: `Client::build()` only fails when
        // TLS init or DNS resolver init fails, both of which would also
        // crash every other reqwest user in the process. If we hit it,
        // we hit it loudly.
        let client = Client::builder()
            .timeout(Duration::from_millis(MAX_TIMEOUT_MS))
            .use_rustls_tls()
            .build()
            .expect("reqwest::Client::build with rustls TLS");
        // Normalize allow-list entries to lowercase. `Url::host_str()`
        // returns a lowercase host per RFC 3986, so without this an
        // operator who writes "API.Example.com" in plugin.toml would
        // silently get DeniedHost for every request.
        let allowed_hosts: Vec<String> = allowed_hosts
            .into_iter()
            .map(|h| h.to_lowercase())
            .collect();
        Self {
            client,
            allowed_hosts: Arc::new(allowed_hosts),
        }
    }

    /// Execute one buffered HTTP request on behalf of a wasm plugin.
    ///
    /// Every checkpoint that can reject a request returns a
    /// `wit::HttpError` variant the wasm guest can match on directly:
    /// `DeniedHost`, `DeniedMethod`, `Transport`, `Timeout`, or
    /// `BodyTooLarge`. The host **never** panics out of this path — wasm
    /// guests rely on `result<_, http-error>` to surface failures.
    pub async fn fetch(&self, req: wit::HttpRequest) -> Result<wit::HttpResponse, wit::HttpError> {
        // 1. Parse URL. Malformed URLs are a transport-class problem
        //    from the plugin's perspective.
        let url =
            url::Url::parse(&req.url).map_err(|e| wit::HttpError::Transport(e.to_string()))?;

        // 2. Allow-list check. We compare against `Url::host_str` so the
        //    plugin can't sneak past with a userinfo-embedded host or a
        //    URL-encoded variant.
        let host = url.host_str().unwrap_or("").to_string();
        if !self.allowed_hosts.iter().any(|h| h == &host) {
            return Err(wit::HttpError::DeniedHost(host));
        }

        // 3. Method allow-list. Anything outside the fixed set rejects
        //    before we touch the network. `parse` would otherwise accept
        //    any RFC 7230 token, including CONNECT/TRACE.
        let method_upper = req.method.to_ascii_uppercase();
        match method_upper.as_str() {
            "GET" | "POST" | "PUT" | "DELETE" | "HEAD" | "PATCH" => {}
            _ => return Err(wit::HttpError::DeniedMethod(req.method.clone())),
        }
        let method: reqwest::Method = method_upper
            .parse()
            .map_err(|_| wit::HttpError::DeniedMethod(req.method.clone()))?;

        // 4. Build the request. Headers are forwarded as-is; reqwest
        //    rejects invalid header names/values at send time, which we
        //    surface as Transport.
        let mut rb = self.client.request(method, url);
        for (k, v) in req.headers {
            rb = rb.header(k, v);
        }
        if let Some(body) = req.body {
            rb = rb.body(body);
        }
        if let Some(ms) = req.timeout_ms {
            // Clamp to the ceiling. `as u64` is safe — u32 always fits.
            rb = rb.timeout(Duration::from_millis(u64::from(ms).min(MAX_TIMEOUT_MS)));
        }

        // 5. Send + decode. `is_timeout` distinguishes the connect/read
        //    timeout from a generic transport failure; reqwest's other
        //    error classes (TLS, DNS, body) all collapse into Transport
        //    in our taxonomy.
        let resp = rb.send().await.map_err(|e| {
            if e.is_timeout() {
                wit::HttpError::Timeout
            } else {
                wit::HttpError::Transport(e.to_string())
            }
        })?;

        let status = resp.status().as_u16();
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();

        let body = resp
            .bytes()
            .await
            .map_err(|e| wit::HttpError::Transport(e.to_string()))?
            .to_vec();

        // 6. Size cap. We *could* enforce this with `bytes_stream` and
        //    abort mid-body, but for v0.18.0 the post-receive check is
        //    sufficient: a malicious server-of-record is already
        //    constrained by the allow-list, so this is mostly a
        //    defense-in-depth check against runaway responses.
        if body.len() > MAX_BODY_BYTES {
            return Err(wit::HttpError::BodyTooLarge(body.len() as u64));
        }

        Ok(wit::HttpResponse {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allowed_hosts_denies_every_request() {
        let state = HttpState::new(Vec::new());
        // Synchronous denial path — no network involved.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(async {
            state
                .fetch(wit::HttpRequest {
                    method: "GET".into(),
                    url: "https://example.com/x".into(),
                    headers: vec![],
                    body: None,
                    timeout_ms: None,
                })
                .await
                .unwrap_err()
        });
        assert!(matches!(err, wit::HttpError::DeniedHost(h) if h == "example.com"));
    }

    #[test]
    fn allow_list_is_host_only_port_excluded() {
        // The allow-list is intentionally host-only — an entry
        // `api.example.com` matches every port on that host. This
        // mirrors the documented behavior at the top of the module.
        // Verifying with a non-standard port (9090) that would otherwise
        // be served by a different daemon than the standard 443.
        let state = HttpState::new(vec!["api.example.com".into()]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(async {
            state
                .fetch(wit::HttpRequest {
                    method: "GET".into(),
                    // Point at a non-routable port so the test never
                    // actually opens a network connection; we want the
                    // allow-list check to PASS so the request proceeds
                    // to the network stage, where reqwest will fail
                    // with Transport (connection refused / timeout).
                    url: "http://api.example.com:9090/secret".into(),
                    headers: vec![],
                    body: None,
                    timeout_ms: Some(50),
                })
                .await
                .unwrap_err()
        });
        // The denial would be DeniedHost; any non-DeniedHost error
        // means the allow-list passed and we got further. Treat
        // Transport (connection refused) and Timeout as proof the
        // port-stripping check works.
        assert!(
            !matches!(err, wit::HttpError::DeniedHost(_)),
            "expected allow-list to pass on host-only match, got {err:?}"
        );
    }

    #[test]
    fn malformed_url_returns_transport_error() {
        let state = HttpState::new(vec!["example.com".into()]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(async {
            state
                .fetch(wit::HttpRequest {
                    method: "GET".into(),
                    url: "not a url".into(),
                    headers: vec![],
                    body: None,
                    timeout_ms: None,
                })
                .await
                .unwrap_err()
        });
        assert!(matches!(err, wit::HttpError::Transport(_)));
    }

    #[test]
    fn disallowed_method_rejects_before_network() {
        let state = HttpState::new(vec!["example.com".into()]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // CONNECT/TRACE/OPTIONS are explicitly outside the allow-list.
        for method in ["CONNECT", "TRACE", "OPTIONS"] {
            let err = rt.block_on(async {
                state
                    .fetch(wit::HttpRequest {
                        method: method.into(),
                        url: "https://example.com/x".into(),
                        headers: vec![],
                        body: None,
                        timeout_ms: None,
                    })
                    .await
                    .unwrap_err()
            });
            assert!(
                matches!(&err, wit::HttpError::DeniedMethod(m) if m == method),
                "expected DeniedMethod for {method}, got {err:?}",
            );
        }
    }

    #[test]
    fn unlisted_host_denied_with_canonical_host_string() {
        // Allow-list contains `example.com`; request to `evil.example`
        // must be denied. We assert the *canonical* `host_str` is
        // surfaced in the error (it's lowercase, no scheme, no path).
        let state = HttpState::new(vec!["example.com".into()]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(async {
            state
                .fetch(wit::HttpRequest {
                    method: "GET".into(),
                    url: "https://Evil.Example/x".into(),
                    headers: vec![],
                    body: None,
                    timeout_ms: None,
                })
                .await
                .unwrap_err()
        });
        match err {
            wit::HttpError::DeniedHost(h) => assert_eq!(h, "evil.example"),
            other => panic!("expected DeniedHost, got {other:?}"),
        }
    }
}
