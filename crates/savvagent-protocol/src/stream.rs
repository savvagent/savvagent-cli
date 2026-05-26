//! Streaming events delivered via MCP progress notifications.
//!
//! When [`CompleteRequest.stream`](crate::CompleteRequest::stream) is `true`,
//! the provider server emits a sequence of [`StreamEvent`]s before returning
//! the final [`CompleteResponse`](crate::CompleteResponse). Each event is
//! attached to an MCP `notifications/progress` payload under the
//! [`STREAM_EVENT_KIND`](crate::STREAM_EVENT_KIND) discriminator.
//!
//! The event vocabulary mirrors Anthropic's streaming spec because it is the
//! most expressive of the major provider streaming formats. OpenAI, Gemini,
//! and Ollama provider servers translate vendor deltas into this shape.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::content::ContentBlock;
use crate::response::{StopReason, Usage};

/// A single streaming event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Sent once at the start of a streamed response. Carries the assigned
    /// message id and initial usage estimate.
    MessageStart {
        /// Provider-assigned message id.
        id: String,
        /// Resolved model name.
        model: String,
        /// Initial usage (typically only `input_tokens` populated).
        usage: Usage,
    },

    /// A new content block has begun. Subsequent
    /// [`StreamEvent::ContentBlockDelta`] events with the same `index` apply
    /// deltas to it until [`StreamEvent::ContentBlockStop`].
    ContentBlockStart {
        /// Zero-based block index within the message.
        index: u32,
        /// The block in its initial (often empty) form.
        block: ContentBlock,
    },

    /// Incremental update to an in-flight content block.
    ContentBlockDelta {
        /// Index of the block being updated.
        index: u32,
        /// The delta to apply.
        delta: BlockDelta,
    },

    /// The content block at `index` is complete.
    ContentBlockStop {
        /// Index of the block that just finished.
        index: u32,
    },

    /// Top-level message metadata update — typically the final stop reason
    /// and any usage refinement.
    MessageDelta {
        /// Final stop reason if known at this point.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<StopReason>,
        /// Matched stop sequence, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_sequence: Option<String>,
        /// Token-count updates (additive over what was already announced).
        usage_delta: UsageDelta,
    },

    /// Stream is fully drained; the host should expect the final tool result
    /// to land next.
    MessageStop,

    /// Non-fatal heartbeat. Hosts may safely ignore.
    Ping,

    /// Recoverable warning attached to the stream (e.g., rate-limit retry).
    /// Fatal errors are returned as MCP tool errors instead.
    Warning {
        /// Human-readable message.
        message: String,
    },
}

/// Delta applied to a content block during streaming.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockDelta {
    /// Append text to a [`ContentBlock::Text`].
    TextDelta {
        /// Text fragment.
        text: String,
    },
    /// Append a JSON fragment to the partially-built `input` of a
    /// [`ContentBlock::ToolUse`]. Hosts must concatenate fragments and parse
    /// once at [`StreamEvent::ContentBlockStop`].
    InputJsonDelta {
        /// Raw JSON fragment (not necessarily valid JSON on its own).
        partial_json: String,
    },
    /// Append text to a [`ContentBlock::Thinking`].
    ThinkingDelta {
        /// Thinking text fragment.
        text: String,
    },
    /// Final signature for a thinking block. Always sent before
    /// [`StreamEvent::ContentBlockStop`] for thinking blocks that carry one.
    SignatureDelta {
        /// Provider-opaque signature.
        signature: String,
    },
    /// Append a fragment of HTML source to a `ContentBlock::Html` block
    /// during streaming. Hosts concatenate `source` fragments across
    /// deltas until `ContentBlockStop`, then hand the assembled source
    /// to the registered renderer.
    HtmlSourceDelta {
        /// HTML source fragment.
        source: String,
    },
}

/// Token-count delta on [`StreamEvent::MessageDelta`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UsageDelta {
    /// Additional output tokens generated since the last update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
    /// Additional cached input tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_source_delta_round_trip() {
        let delta = BlockDelta::HtmlSourceDelta {
            source: "<!docty".into(),
        };
        let v = serde_json::to_value(&delta).unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "type": "html_source_delta", "source": "<!docty" }),
        );
        let back: BlockDelta = serde_json::from_value(v).unwrap();
        assert_eq!(back, delta);
    }
}
