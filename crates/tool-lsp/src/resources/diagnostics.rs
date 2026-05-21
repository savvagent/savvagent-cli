//! Publishes `lsp://diagnostics/<absolute-path>` MCP resources and
//! serves `resources/read` for them.
//!
//! The URI scheme is a 1:1 reskin of the LSP `file://<path>` URI: we
//! strip the `file://` prefix and prepend `lsp://diagnostics/`. This
//! keeps the absolute path human-readable in the conversation and
//! round-trips cleanly back to a `file://` URI when serving reads.

use crate::convert::DiagnosticOut;
use crate::pool::LspPool;
use rmcp::ErrorData;
use rmcp::model::{ReadResourceResult, ResourceContents};

/// URI scheme + prefix every diagnostics resource lives under.
const URI_PREFIX: &str = "lsp://diagnostics/";

/// Translate an LSP file URI (e.g. `file:///abs/path/main.rs`) into the
/// `lsp://diagnostics/<absolute-path>` form fired in
/// `notifications/resources/updated`.
///
/// If `file_uri` does not start with `file://` it is appended verbatim
/// — callers should normally only pass LSP-shaped URIs, but we don't
/// want to panic if a server sends something unusual.
pub fn diagnostics_uri_for(file_uri: &str) -> String {
    let path = file_uri.strip_prefix("file://").unwrap_or(file_uri);
    format!("{URI_PREFIX}{path}")
}

/// Reverse of [`diagnostics_uri_for`]: pull the absolute file path back
/// out of a diagnostics URI so we can match it against cached sessions.
///
/// Returns `None` if `uri` is not a diagnostics URI.
pub fn file_path_from_diagnostics_uri(uri: &str) -> Option<String> {
    uri.strip_prefix(URI_PREFIX).map(str::to_string)
}

/// Serve `resources/read` for any URI under `lsp://diagnostics/*`.
///
/// Walks every cached session via [`LspPool::snapshot_sessions`] and
/// asks each one for its diagnostics view of the URI. The first session
/// that has a non-empty entry wins; absent any match we return an empty
/// JSON array (no diagnostics ≡ "no problems known"). The body is
/// always serialized as `application/json` containing a `Vec<DiagnosticOut>`.
pub async fn read(uri: &str, pool: &LspPool) -> Result<ReadResourceResult, ErrorData> {
    let path = file_path_from_diagnostics_uri(uri).ok_or_else(|| {
        ErrorData::invalid_params(format!("not an lsp:// diagnostics URI: {uri}"), None)
    })?;
    let file_uri = format!("file://{path}");
    let sessions = pool.snapshot_sessions().await;
    let mut diagnostics = Vec::new();
    for session in sessions {
        let mut hit = session.diagnostics_for(&file_uri).await;
        if !hit.is_empty() {
            diagnostics.append(&mut hit);
            break;
        }
    }
    let out: Vec<DiagnosticOut> = diagnostics.into_iter().map(Into::into).collect();
    let body = serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string());
    Ok(ReadResourceResult::new(vec![
        ResourceContents::TextResourceContents {
            uri: uri.to_string(),
            mime_type: Some("application/json".into()),
            text: body,
            meta: None,
        },
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_uri_for_strips_file_scheme() {
        assert_eq!(
            diagnostics_uri_for("file:///abs/path/main.rs"),
            "lsp://diagnostics//abs/path/main.rs"
        );
    }

    #[test]
    fn diagnostics_uri_for_passes_through_non_file_uris() {
        // Defensive: if something other than file:// arrives, keep the
        // original string so we don't silently drop characters.
        assert_eq!(
            diagnostics_uri_for("urn:foo:bar"),
            "lsp://diagnostics/urn:foo:bar"
        );
    }

    #[test]
    fn round_trip_through_file_path() {
        let file_uri = "file:///home/user/proj/src/lib.rs";
        let diag_uri = diagnostics_uri_for(file_uri);
        let path = file_path_from_diagnostics_uri(&diag_uri).expect("recovered path");
        assert_eq!(path, "/home/user/proj/src/lib.rs");
        // Closing the loop back to the LSP-shaped URI.
        assert_eq!(format!("file://{path}"), file_uri);
    }

    #[test]
    fn file_path_from_diagnostics_uri_rejects_other_schemes() {
        assert!(file_path_from_diagnostics_uri("test://other/uri").is_none());
        assert!(file_path_from_diagnostics_uri("file:///abs/path").is_none());
    }

    #[tokio::test]
    async fn read_rejects_non_diagnostics_uri() {
        let pool = LspPool::default();
        let err = read("test://other/payload", &pool).await.unwrap_err();
        assert!(
            err.message.contains("not an lsp:// diagnostics URI"),
            "unexpected error: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn read_returns_empty_array_when_no_sessions_match() {
        let pool = LspPool::default();
        let result = read("lsp://diagnostics//tmp/nope.rs", &pool).await.unwrap();
        assert_eq!(result.contents.len(), 1);
        match &result.contents[0] {
            ResourceContents::TextResourceContents {
                uri,
                mime_type,
                text,
                ..
            } => {
                assert_eq!(uri, "lsp://diagnostics//tmp/nope.rs");
                assert_eq!(mime_type.as_deref(), Some("application/json"));
                assert_eq!(text, "[]");
            }
            other => panic!("expected text resource, got {other:?}"),
        }
    }
}
