//! `web_fetch`: retrieve a URL and return readable text.
//!
//! Two safety layers apply to every fetch, mirroring the Layer-1 path
//! containment convention used by `tool-fs` / `tool-bash` / `tool-grep`,
//! adapted to network destinations instead of filesystem paths:
//!
//! 1. **Scheme allow-list.** Only `http` and `https` are accepted.
//! 2. **SSRF guard.** The URL's host is resolved and every resolved IP is
//!    checked against loopback / private / link-local / unspecified /
//!    multicast ranges before the request is sent. This stops the tool
//!    from being used to reach `169.254.169.254` (cloud metadata
//!    endpoints), `localhost`, or other hosts on the machine's private
//!    network — even indirectly via DNS rebinding, since the check runs
//!    against the resolved socket address `reqwest` will actually connect
//!    to, not just the literal hostname string.
//!
//! HTML responses are converted to plain, wrapped text via `html2text` so
//! the model gets readable content instead of markup noise. Non-HTML
//! text responses (JSON, plain text, markdown, etc.) pass through as-is.
//! Output is capped at [`DEFAULT_MAX_CHARS`] characters by default.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use futures::StreamExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::WebToolError;

/// Default cap on returned characters. Overridable per call via
/// [`FetchInput::max_chars`].
pub const DEFAULT_MAX_CHARS: usize = 50_000;

/// Hard cap on response body bytes read from the wire, applied
/// regardless of `max_chars`. Protects against unbounded/streamed
/// responses (memory-exhaustion DoS) from a malicious or compromised
/// server — `max_chars` only truncates *after* the body is read, so it
/// cannot bound memory usage on its own.
const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Wall-clock timeout for the HTTP request, including redirects.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// At most this many redirects are followed before giving up. Each hop is
/// re-validated by the SSRF guard (see [`safe_client`]).
const MAX_REDIRECTS: usize = 10;

/// Input for the `web_fetch` tool.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FetchInput {
    /// The URL to fetch. Must be `http://` or `https://`.
    pub url: String,
    /// Cap on returned characters. Defaults to [`DEFAULT_MAX_CHARS`].
    #[serde(default)]
    pub max_chars: Option<usize>,
    /// Return the raw response body instead of HTML-to-text conversion.
    /// Useful for JSON APIs or when the caller wants the original markup.
    #[serde(default)]
    pub raw: bool,
}

/// Output of the `web_fetch` tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct FetchOutput {
    /// The final URL after following redirects.
    pub url: String,
    /// HTTP status code.
    pub status: u16,
    /// `content-type` response header, if present.
    pub content_type: Option<String>,
    /// Extracted (or raw) body content, capped at `max_chars`.
    pub content: String,
    /// `true` if `content` was truncated to fit the cap.
    pub truncated: bool,
}

/// Fetch `input.url`, applying the scheme allow-list and SSRF guard,
/// returning readable text (or raw body, per `input.raw`).
pub async fn run(input: FetchInput) -> Result<FetchOutput, WebToolError> {
    let url = Url::parse(&input.url)
        .map_err(|e| WebToolError::InvalidArgument(format!("invalid url: {e}")))?;
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(WebToolError::InvalidArgument(format!(
                "unsupported scheme {other:?}: only http/https are allowed"
            )));
        }
    }

    let client = build_client()?;
    let resp = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| WebToolError::Network(format!("request to {url} failed: {e}")))?;

    let status = resp.status().as_u16();
    let final_url = resp.url().to_string();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    if let Some(len) = resp.content_length()
        && len > MAX_BODY_BYTES as u64
    {
        return Err(WebToolError::InvalidArgument(format!(
            "response too large: content-length {len} bytes exceeds the \
             {MAX_BODY_BYTES}-byte limit"
        )));
    }

    let body = read_body_capped(resp).await?;

    let is_html = content_type
        .as_deref()
        .is_some_and(|ct| ct.contains("text/html") || ct.contains("application/xhtml"));

    let text = if input.raw || !is_html {
        body
    } else {
        html2text::from_read(body.as_bytes(), 100).unwrap_or(body)
    };

    let max_chars = input.max_chars.unwrap_or(DEFAULT_MAX_CHARS).max(1);
    let (content, truncated) = truncate_chars(&text, max_chars);

    Ok(FetchOutput {
        url: final_url,
        status,
        content_type,
        content,
        truncated,
    })
}

/// Reads `resp`'s body as a stream, aborting with an error the moment
/// [`MAX_BODY_BYTES`] is exceeded rather than buffering the whole
/// response first. This bounds memory usage even when the server lies
/// about (or omits) `Content-Length`.
async fn read_body_capped(resp: reqwest::Response) -> Result<String, WebToolError> {
    let mut buf = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|e| WebToolError::Network(format!("reading response body failed: {e}")))?;
        if buf.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(WebToolError::InvalidArgument(format!(
                "response exceeded the {MAX_BODY_BYTES}-byte limit while streaming"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn truncate_chars(s: &str, max_chars: usize) -> (String, bool) {
    if s.chars().count() <= max_chars {
        return (s.to_string(), false);
    }
    (s.chars().take(max_chars).collect(), true)
}

/// Build a `reqwest::Client` that re-validates every connection attempt
/// (including each redirect hop) against the SSRF guard, via a resolver
/// override that rejects blocked addresses before `reqwest` ever opens a
/// socket.
fn build_client() -> Result<reqwest::Client, WebToolError> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            match attempt.url().scheme() {
                "http" | "https" => attempt.follow(),
                _ => attempt.error("redirect to unsupported scheme"),
            }
        }))
        .dns_resolver(std::sync::Arc::new(GuardedResolver))
        .build()
        .map_err(|e| WebToolError::Network(format!("building HTTP client failed: {e}")))
}

/// Resolves hostnames via the system resolver, then filters out any
/// resolved address that falls in a blocked range. If every candidate
/// address is blocked, resolution fails closed (empty result), which
/// `reqwest` surfaces as a connection error rather than a silent bypass.
#[derive(Debug)]
struct GuardedResolver;

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            // `lookup_host` requires a `host:port` pair; the port is
            // discarded by callers that only care about the address.
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .filter(|addr| !is_blocked_addr(addr.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host}: no non-blocked addresses resolved").into());
            }
            let iter: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

/// `true` if `ip` must not be reachable from `web_fetch` — loopback,
/// private, link-local, unspecified, multicast, or documentation ranges,
/// on either address family. This is the SSRF guard's core predicate;
/// callers should fail closed (treat unresolvable/ambiguous as blocked)
/// rather than call this speculatively.
pub fn is_blocked_addr(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => is_blocked_v6(v6),
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        // 100.64.0.0/10 — carrier-grade NAT, commonly used for internal
        // cloud infra (e.g. AWS/GCP metadata proxies sit adjacent to it).
        || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    // IPv4-mapped addresses (`::ffff:a.b.c.d`) must be checked against the
    // v4 rules too, or an attacker can bypass the guard by using the
    // mapped form of a private address.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_blocked_v4(v4);
    }
    // Other IPv6 forms that embed an IPv4 address (NAT64 well-known
    // prefix, 6to4) must also be unwrapped and checked, or a DNS
    // response can smuggle a blocked v4 address past the plain v6
    // predicates below.
    if let Some(v4) = embedded_ipv4(ip) {
        return is_blocked_v4(v4);
    }
    let segments = ip.segments();
    // fe80::/10 — link-local.
    let link_local = (segments[0] & 0xffc0) == 0xfe80;
    // fc00::/7 — unique local addresses (the IPv6 analog of RFC 1918).
    let unique_local = (segments[0] & 0xfe00) == 0xfc00;
    link_local || unique_local
}

/// Extracts an embedded IPv4 address from IPv6 transition mechanisms
/// other than the standard `::ffff:a.b.c.d` mapped form (already handled
/// by [`Ipv6Addr::to_ipv4_mapped`]):
///
/// - NAT64 / DNS64 well-known prefix `64:ff9b::/96` (RFC 6052)
/// - 6to4 `2002::/16` (RFC 3056), where the next 32 bits are the IPv4
///   address
fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 {
        return Some(segments_to_ipv4(s[6], s[7]));
    }
    if s[0] == 0x2002 {
        return Some(segments_to_ipv4(s[1], s[2]));
    }
    None
}

fn segments_to_ipv4(hi: u16, lo: u16) -> Ipv4Addr {
    Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_loopback_and_private_v4() {
        assert!(is_blocked_addr("127.0.0.1".parse().unwrap()));
        assert!(is_blocked_addr("10.0.0.5".parse().unwrap()));
        assert!(is_blocked_addr("192.168.1.1".parse().unwrap()));
        assert!(is_blocked_addr("169.254.169.254".parse().unwrap()));
        assert!(is_blocked_addr("100.100.100.1".parse().unwrap()));
    }

    #[test]
    fn blocks_loopback_and_unique_local_v6() {
        assert!(is_blocked_addr("::1".parse().unwrap()));
        assert!(is_blocked_addr("fc00::1".parse().unwrap()));
        assert!(is_blocked_addr("fe80::1".parse().unwrap()));
        // IPv4-mapped private address must also be blocked.
        assert!(is_blocked_addr("::ffff:10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn blocks_nat64_and_6to4_embedded_v4() {
        // 64:ff9b::7f00:1 embeds 127.0.0.1 via the NAT64 well-known prefix.
        assert!(is_blocked_addr("64:ff9b::7f00:1".parse().unwrap()));
        // 64:ff9b::a9fe:a9fe embeds 169.254.169.254 (cloud metadata IP).
        assert!(is_blocked_addr("64:ff9b::a9fe:a9fe".parse().unwrap()));
        // 2002:7f00:1:: is the 6to4 form of 127.0.0.1.
        assert!(is_blocked_addr("2002:7f00:1::".parse().unwrap()));
        // Public addresses embedded the same way must still pass.
        assert!(!is_blocked_addr("64:ff9b::0101:0101".parse().unwrap()));
        assert!(!is_blocked_addr("2002:0101:0101::".parse().unwrap()));
    }

    #[test]
    fn allows_public_addrs() {
        assert!(!is_blocked_addr("1.1.1.1".parse().unwrap()));
        assert!(!is_blocked_addr("8.8.8.8".parse().unwrap()));
        assert!(!is_blocked_addr(
            "2606:4700:4700::1111".parse().unwrap() // Cloudflare DNS v6
        ));
    }

    #[test]
    fn truncates_by_char_count_not_byte_count() {
        let (out, truncated) = truncate_chars("héllo wörld", 5);
        assert_eq!(out.chars().count(), 5);
        assert!(truncated);
    }
}
