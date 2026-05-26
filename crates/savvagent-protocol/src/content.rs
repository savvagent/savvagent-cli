//! Content blocks shared between requests, responses, and stream events.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A single block of content within a [`Message`](crate::Message) or
/// [`CompleteResponse`](crate::CompleteResponse).
///
/// Modeled on Anthropic's content-block vocabulary because it is the most
/// expressive of the major provider APIs. Other providers translate to/from
/// this canonical form inside their MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text.
    Text {
        /// The text content.
        text: String,
    },

    /// A tool invocation requested by the assistant. Hosts execute the tool
    /// and return the result as a [`ContentBlock::ToolResult`] in a follow-up
    /// `user` message.
    ToolUse {
        /// Provider-assigned identifier, echoed back in `tool_result.tool_use_id`.
        id: String,
        /// Tool name (matches an MCP tool exposed to the provider).
        name: String,
        /// JSON arguments for the tool call. Always a JSON object at the top
        /// level, even if empty.
        input: serde_json::Value,
    },

    /// The result of executing a previously-requested tool call.
    ToolResult {
        /// Identifier from the originating [`ContentBlock::ToolUse`].
        tool_use_id: String,
        /// Result content. Most often a single text block, but may include
        /// images or further structured content.
        content: Vec<ContentBlock>,
        /// Whether the tool call failed. Defaults to `false`.
        #[serde(default, skip_serializing_if = "is_false")]
        is_error: bool,
    },

    /// An image attached to a user message.
    Image {
        /// Where the bytes come from.
        source: ImageSource,
    },

    /// Extended thinking output from the model, when supported.
    ///
    /// Hosts should preserve thinking blocks verbatim (including any opaque
    /// `signature`) when echoing prior assistant turns back to the provider,
    /// or models will reject the request.
    Thinking {
        /// The thinking text.
        text: String,
        /// Provider-opaque signature used to verify the block on echo.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// HTML source the host should render inline in the conversation
    /// transcript via a registered `ContentRenderer` plugin. Used for
    /// structured documents (plans, specs, status updates) where
    /// rendered HTML is more legible than markdown.
    ///
    /// The source is a complete HTML document; the renderer parses it
    /// fresh. Hosts that do not have a renderer registered render the
    /// source as a code block.
    Html {
        /// Complete HTML document source.
        source: String,
        /// Opaque, base64-encoded interactive-state blob produced by a
        /// `ContentRenderer::snapshot_state` (e.g. expanded `<details>`,
        /// form values, focus). `None` when the canvas has no persisted
        /// interactive state. Added in Phase 2; older transcripts (no
        /// `state`) deserialize via the serde default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<String>,
    },
}

/// Source of image bytes in an [`ContentBlock::Image`] block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    /// Inlined base64-encoded bytes.
    Base64 {
        /// MIME media type.
        media_type: MediaType,
        /// Base64-encoded image bytes (no `data:` URI prefix).
        data: String,
    },
    /// URL the provider should fetch on the host's behalf. Not all providers
    /// support this — translators may inline the bytes when needed.
    Url {
        /// Absolute URL.
        url: String,
    },
}

/// Image MIME types accepted across providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    /// `image/jpeg`
    #[serde(rename = "image/jpeg")]
    Jpeg,
    /// `image/png`
    #[serde(rename = "image/png")]
    Png,
    /// `image/gif`
    #[serde(rename = "image/gif")]
    Gif,
    /// `image/webp`
    #[serde(rename = "image/webp")]
    Webp,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trip() {
        let block = ContentBlock::Text { text: "hi".into() };
        let v = serde_json::to_value(&block).unwrap();
        assert_eq!(v, serde_json::json!({ "type": "text", "text": "hi" }));
        let back: ContentBlock = serde_json::from_value(v).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn tool_use_round_trip() {
        let block = ContentBlock::ToolUse {
            id: "call_123".into(),
            name: "read_file".into(),
            input: serde_json::json!({ "path": "/tmp/x" }),
        };
        let v = serde_json::to_value(&block).unwrap();
        assert_eq!(v["type"], "tool_use");
        assert_eq!(v["id"], "call_123");
        let back: ContentBlock = serde_json::from_value(v).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn tool_result_default_is_error_omitted() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "call_123".into(),
            content: vec![ContentBlock::Text { text: "ok".into() }],
            is_error: false,
        };
        let v = serde_json::to_value(&block).unwrap();
        assert!(
            v.get("is_error").is_none(),
            "is_error=false must be omitted"
        );
    }

    #[test]
    fn html_round_trip() {
        let block = ContentBlock::Html {
            source: "<!doctype html><body>hi</body>".into(),
            state: None,
        };
        let v = serde_json::to_value(&block).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "type": "html",
                "source": "<!doctype html><body>hi</body>",
            }),
        );
        let back: ContentBlock = serde_json::from_value(v).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn html_with_state_round_trips() {
        let block = ContentBlock::Html {
            source: "<p>x</p>".into(),
            state: Some("eyJ4Ijoxf0=".into()),
        };
        let v = serde_json::to_value(&block).unwrap();
        assert_eq!(v["state"], "eyJ4Ijoxf0=");
        let back: ContentBlock = serde_json::from_value(v).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn html_without_state_key_deserializes_to_none() {
        // Old transcripts have no `state` key.
        let v = serde_json::json!({ "type": "html", "source": "<p>x</p>" });
        let back: ContentBlock = serde_json::from_value(v).unwrap();
        assert!(matches!(back, ContentBlock::Html { state: None, .. }));
    }

    #[test]
    fn html_none_state_omitted_from_json() {
        // skip_serializing_if keeps the wire clean for stateless canvases.
        let block = ContentBlock::Html {
            source: "<p>x</p>".into(),
            state: None,
        };
        let v = serde_json::to_value(&block).unwrap();
        assert!(v.get("state").is_none(), "None state must be omitted: {v}");
    }
}
