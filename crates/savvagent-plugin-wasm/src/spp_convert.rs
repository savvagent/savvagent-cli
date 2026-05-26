//! Mechanical `From`/`Into` between `savvagent_protocol` SPP types and
//! the WIT bindings generated for the `plugin-provider` world.
//!
//! Round-trip discipline: every byte in a `savvagent_protocol` value must
//! survive a conversion to its WIT mirror and back. The convention here
//! is to provide both `From<spp::X> for wit::X` and `From<wit::X> for
//! spp::X` for every protocol type; that gives the adapters in Tasks 4–6
//! a single canonical conversion surface.
//!
//! Three types in `wit` are deliberately *not* converted:
//!
//! - [`wit::ProviderManifest`], [`wit::CountTokensRequest`],
//!   [`wit::CountTokensResponse`] — these exist only as part of the
//!   `plugin-provider` world's contract (returned by `init` / `count-tokens`
//!   exports) and have no counterpart in `savvagent_protocol`. The provider
//!   adapter in Task 6 reads them directly from the bindgen output.
//!
//! - [`wit::ListModelsResponse`] is converted *to* `spp::ListModelsResponse`
//!   only — the host does not pass it the other direction.
//!
//! Recursive `tool-result` content is round-tripped via a serialized JSON
//! array because WIT does not permit recursive type definitions. See the
//! comment on `ContentBlock::ToolResult` for details.

use crate::provider_world::savvagent::plugin::spp as wit;
use savvagent_protocol as spp;

// ---- Role -----------------------------------------------------------
impl From<spp::Role> for wit::Role {
    fn from(r: spp::Role) -> Self {
        match r {
            spp::Role::User => Self::User,
            spp::Role::Assistant => Self::Assistant,
        }
    }
}
impl From<wit::Role> for spp::Role {
    fn from(r: wit::Role) -> Self {
        match r {
            wit::Role::User => Self::User,
            wit::Role::Assistant => Self::Assistant,
        }
    }
}

// ---- MediaType ------------------------------------------------------
impl From<spp::MediaType> for wit::MediaType {
    fn from(m: spp::MediaType) -> Self {
        match m {
            spp::MediaType::Jpeg => Self::Jpeg,
            spp::MediaType::Png => Self::Png,
            spp::MediaType::Gif => Self::Gif,
            spp::MediaType::Webp => Self::Webp,
        }
    }
}
impl From<wit::MediaType> for spp::MediaType {
    fn from(m: wit::MediaType) -> Self {
        match m {
            wit::MediaType::Jpeg => Self::Jpeg,
            wit::MediaType::Png => Self::Png,
            wit::MediaType::Gif => Self::Gif,
            wit::MediaType::Webp => Self::Webp,
        }
    }
}

// ---- ImageSource ----------------------------------------------------
impl From<spp::ImageSource> for wit::ImageSource {
    fn from(s: spp::ImageSource) -> Self {
        match s {
            spp::ImageSource::Base64 { media_type, data } => Self::Base64(wit::ImageBase64 {
                media_type: media_type.into(),
                data,
            }),
            spp::ImageSource::Url { url } => Self::Url(url),
        }
    }
}
impl From<wit::ImageSource> for spp::ImageSource {
    fn from(s: wit::ImageSource) -> Self {
        match s {
            wit::ImageSource::Base64(b) => Self::Base64 {
                media_type: b.media_type.into(),
                data: b.data,
            },
            wit::ImageSource::Url(url) => Self::Url { url },
        }
    }
}

// ---- ContentBlock ---------------------------------------------------
//
// `tool_result.content` is itself a `Vec<ContentBlock>` and would create
// a WIT type cycle, so we encode the inner content as a JSON string and
// round-trip it through `serde_json` here. JSON round-trip is byte-equal
// because every variant in `ContentBlock` derives `PartialEq` and serde's
// `serde_json::Value` ordering is stable.
impl From<spp::ContentBlock> for wit::ContentBlock {
    fn from(b: spp::ContentBlock) -> Self {
        match b {
            spp::ContentBlock::Text { text } => Self::Text(wit::TextBlock { text }),
            spp::ContentBlock::ToolUse { id, name, input } => Self::ToolUse(wit::ToolUseBlock {
                id,
                name,
                input_json: serde_json::to_string(&input)
                    .expect("serde_json::Value always serializes"),
            }),
            spp::ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Self::ToolResult(wit::ToolResultBlock {
                tool_use_id,
                content_json: serde_json::to_string(&content)
                    .expect("Vec<ContentBlock> always serializes"),
                is_error,
            }),
            spp::ContentBlock::Image { source } => Self::Image(wit::ImageBlock {
                source: source.into(),
            }),
            spp::ContentBlock::Thinking { text, signature } => {
                Self::Thinking(wit::ThinkingBlock { text, signature })
            }
            // The `Html` content block was added in the inline-canvas
            // initiative (Phase 1, v0.17.0). The v0.18.0 WIT contract
            // pre-dates it; rather than break the contract this late,
            // we degrade by replacing the HTML payload with a text
            // block that fences the source for the plugin to inspect.
            // A v0.19.0 WIT bump can introduce a typed Html variant.
            spp::ContentBlock::Html { source, state: _ } => {
                tracing::debug!(
                    target: "savvagent_plugin_wasm::spp_convert",
                    chars = source.len(),
                    "ContentBlock::Html crossing wasm boundary; degraded to Text",
                );
                Self::Text(wit::TextBlock { text: source })
            }
        }
    }
}
impl From<wit::ContentBlock> for spp::ContentBlock {
    fn from(b: wit::ContentBlock) -> Self {
        match b {
            wit::ContentBlock::Text(t) => Self::Text { text: t.text },
            wit::ContentBlock::ToolUse(t) => Self::ToolUse {
                id: t.id,
                name: t.name,
                input: parse_wasm_json("tool_use.input", &t.input_json),
            },
            wit::ContentBlock::ToolResult(t) => Self::ToolResult {
                tool_use_id: t.tool_use_id,
                content: serde_json::from_str(&t.content_json).unwrap_or_else(|err| {
                    tracing::warn!(
                        target: "savvagent_plugin_wasm::spp_convert",
                        field = "tool_result.content",
                        ?err,
                        "wasm guest emitted malformed JSON; falling back to empty content"
                    );
                    Vec::new()
                }),
                is_error: t.is_error,
            },
            wit::ContentBlock::Image(i) => Self::Image {
                source: i.source.into(),
            },
            wit::ContentBlock::Thinking(t) => Self::Thinking {
                text: t.text,
                signature: t.signature,
            },
        }
    }
}

/// Parse a JSON payload that crossed the WIT boundary from a wasm plugin.
///
/// On parse failure we **log loudly and fall back to `Value::Null`**. The
/// fallback preserves caller liveness (tool dispatch proceeds with empty
/// input rather than panicking the host on a buggy guest), but it does
/// dispatch the downstream tool with no input — which is almost always
/// wrong. The host's error path is the place to react; v0.18.1 will
/// restructure these conversions into `TryFrom` so the failure
/// propagates as a `ProviderError` instead of being absorbed.
fn parse_wasm_json(field: &'static str, raw: &str) -> serde_json::Value {
    match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(
                target: "savvagent_plugin_wasm::spp_convert",
                field,
                ?err,
                "wasm guest emitted malformed JSON; falling back to Value::Null"
            );
            serde_json::Value::Null
        }
    }
}

// ---- Message --------------------------------------------------------
impl From<spp::Message> for wit::Message {
    fn from(m: spp::Message) -> Self {
        Self {
            role: m.role.into(),
            content: m.content.into_iter().map(Into::into).collect(),
        }
    }
}
impl From<wit::Message> for spp::Message {
    fn from(m: wit::Message) -> Self {
        Self {
            role: m.role.into(),
            content: m.content.into_iter().map(Into::into).collect(),
        }
    }
}

// ---- ToolDef --------------------------------------------------------
impl From<spp::ToolDef> for wit::ToolDef {
    fn from(t: spp::ToolDef) -> Self {
        Self {
            name: t.name,
            description: t.description,
            input_schema_json: serde_json::to_string(&t.input_schema)
                .expect("ToolDef.input_schema always serializes"),
        }
    }
}
impl From<wit::ToolDef> for spp::ToolDef {
    fn from(t: wit::ToolDef) -> Self {
        Self {
            name: t.name,
            description: t.description,
            input_schema: parse_wasm_json("tool_def.input_schema", &t.input_schema_json),
        }
    }
}

// ---- ThinkingConfig -------------------------------------------------
//
// `spp::request::ThinkingConfig` is re-exported via the canonical
// `savvagent_protocol::request::ThinkingConfig` path. We can't `use
// spp::ThinkingConfig` because it isn't re-exported at the crate root, so
// reference it through the module path.
impl From<spp::request::ThinkingConfig> for wit::ThinkingConfig {
    fn from(t: spp::request::ThinkingConfig) -> Self {
        Self {
            budget_tokens: t.budget_tokens,
        }
    }
}
impl From<wit::ThinkingConfig> for spp::request::ThinkingConfig {
    fn from(t: wit::ThinkingConfig) -> Self {
        Self {
            budget_tokens: t.budget_tokens,
        }
    }
}

// ---- CompleteRequest ------------------------------------------------
//
// `metadata` is `Option<serde_json::Value>`. We encode it as
// `Option<String>` in WIT, where `None` → `None` and `Some(value)` →
// `Some(serialized)`. Round-trip is byte-equal because `serde_json` is
// canonicalizing the Value the same way both directions.
impl From<spp::CompleteRequest> for wit::CompleteRequest {
    fn from(r: spp::CompleteRequest) -> Self {
        Self {
            model: r.model,
            messages: r.messages.into_iter().map(Into::into).collect(),
            system: r.system,
            tools: r.tools.into_iter().map(Into::into).collect(),
            temperature: r.temperature,
            top_p: r.top_p,
            max_tokens: r.max_tokens,
            stop_sequences: r.stop_sequences,
            stream: r.stream,
            thinking: r.thinking.map(Into::into),
            metadata_json: r
                .metadata
                .as_ref()
                .map(|v| serde_json::to_string(v).expect("serde_json::Value always serializes")),
        }
    }
}
impl From<wit::CompleteRequest> for spp::CompleteRequest {
    fn from(r: wit::CompleteRequest) -> Self {
        Self {
            model: r.model,
            messages: r.messages.into_iter().map(Into::into).collect(),
            system: r.system,
            tools: r.tools.into_iter().map(Into::into).collect(),
            temperature: r.temperature,
            top_p: r.top_p,
            max_tokens: r.max_tokens,
            stop_sequences: r.stop_sequences,
            stream: r.stream,
            thinking: r.thinking.map(Into::into),
            metadata: r
                .metadata_json
                .as_deref()
                .map(|s| parse_wasm_json("complete_request.metadata", s)),
        }
    }
}

// ---- StopReason -----------------------------------------------------
impl From<spp::StopReason> for wit::StopReason {
    fn from(s: spp::StopReason) -> Self {
        match s {
            spp::StopReason::EndTurn => Self::EndTurn,
            spp::StopReason::ToolUse => Self::ToolUse,
            spp::StopReason::MaxTokens => Self::MaxTokens,
            spp::StopReason::StopSequence => Self::StopSequence,
            spp::StopReason::Refusal => Self::Refusal,
            spp::StopReason::Other => Self::Other,
        }
    }
}
impl From<wit::StopReason> for spp::StopReason {
    fn from(s: wit::StopReason) -> Self {
        match s {
            wit::StopReason::EndTurn => Self::EndTurn,
            wit::StopReason::ToolUse => Self::ToolUse,
            wit::StopReason::MaxTokens => Self::MaxTokens,
            wit::StopReason::StopSequence => Self::StopSequence,
            wit::StopReason::Refusal => Self::Refusal,
            wit::StopReason::Other => Self::Other,
        }
    }
}

// ---- Usage ----------------------------------------------------------
impl From<spp::Usage> for wit::Usage {
    fn from(u: spp::Usage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
        }
    }
}
impl From<wit::Usage> for spp::Usage {
    fn from(u: wit::Usage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
        }
    }
}

// ---- CompleteResponse -----------------------------------------------
impl From<spp::CompleteResponse> for wit::CompleteResponse {
    fn from(r: spp::CompleteResponse) -> Self {
        Self {
            id: r.id,
            model: r.model,
            content: r.content.into_iter().map(Into::into).collect(),
            stop_reason: r.stop_reason.into(),
            stop_sequence: r.stop_sequence,
            usage: r.usage.into(),
        }
    }
}
impl From<wit::CompleteResponse> for spp::CompleteResponse {
    fn from(r: wit::CompleteResponse) -> Self {
        Self {
            id: r.id,
            model: r.model,
            content: r.content.into_iter().map(Into::into).collect(),
            stop_reason: r.stop_reason.into(),
            stop_sequence: r.stop_sequence,
            usage: r.usage.into(),
        }
    }
}

// ---- UsageDelta -----------------------------------------------------
impl From<spp::UsageDelta> for wit::UsageDelta {
    fn from(u: spp::UsageDelta) -> Self {
        Self {
            output_tokens: u.output_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
        }
    }
}
impl From<wit::UsageDelta> for spp::UsageDelta {
    fn from(u: wit::UsageDelta) -> Self {
        Self {
            output_tokens: u.output_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
        }
    }
}

// ---- BlockDelta -----------------------------------------------------
impl From<spp::BlockDelta> for wit::BlockDelta {
    fn from(d: spp::BlockDelta) -> Self {
        match d {
            spp::BlockDelta::TextDelta { text } => Self::TextDelta(text),
            spp::BlockDelta::InputJsonDelta { partial_json } => Self::InputJsonDelta(partial_json),
            spp::BlockDelta::ThinkingDelta { text } => Self::ThinkingDelta(text),
            spp::BlockDelta::SignatureDelta { signature } => Self::SignatureDelta(signature),
            // `HtmlSourceDelta` paired with the `ContentBlock::Html`
            // degradation above — degrade to TextDelta so v0.18.0 WASM
            // plugins see assembled-as-text output. Typed Html block
            // support waits on a v0.19.0 WIT bump.
            spp::BlockDelta::HtmlSourceDelta { source } => Self::TextDelta(source),
        }
    }
}
impl From<wit::BlockDelta> for spp::BlockDelta {
    fn from(d: wit::BlockDelta) -> Self {
        match d {
            wit::BlockDelta::TextDelta(text) => Self::TextDelta { text },
            wit::BlockDelta::InputJsonDelta(partial_json) => Self::InputJsonDelta { partial_json },
            wit::BlockDelta::ThinkingDelta(text) => Self::ThinkingDelta { text },
            wit::BlockDelta::SignatureDelta(signature) => Self::SignatureDelta { signature },
        }
    }
}

// ---- StreamEvent ----------------------------------------------------
impl From<spp::StreamEvent> for wit::StreamEvent {
    fn from(e: spp::StreamEvent) -> Self {
        match e {
            spp::StreamEvent::MessageStart { id, model, usage } => {
                Self::MessageStart(wit::MessageStartEvt {
                    id,
                    model,
                    usage: usage.into(),
                })
            }
            spp::StreamEvent::ContentBlockStart { index, block } => {
                Self::ContentBlockStart(wit::ContentBlockStartEvt {
                    index,
                    block: block.into(),
                })
            }
            spp::StreamEvent::ContentBlockDelta { index, delta } => {
                Self::ContentBlockDelta(wit::ContentBlockDeltaEvt {
                    index,
                    delta: delta.into(),
                })
            }
            spp::StreamEvent::ContentBlockStop { index } => Self::ContentBlockStop(index),
            spp::StreamEvent::MessageDelta {
                stop_reason,
                stop_sequence,
                usage_delta,
            } => Self::MessageDelta(wit::MessageDeltaEvt {
                stop_reason: stop_reason.map(Into::into),
                stop_sequence,
                usage_delta: usage_delta.into(),
            }),
            spp::StreamEvent::MessageStop => Self::MessageStop,
            spp::StreamEvent::Ping => Self::Ping,
            spp::StreamEvent::Warning { message } => Self::Warning(message),
        }
    }
}
impl From<wit::StreamEvent> for spp::StreamEvent {
    fn from(e: wit::StreamEvent) -> Self {
        match e {
            wit::StreamEvent::MessageStart(m) => Self::MessageStart {
                id: m.id,
                model: m.model,
                usage: m.usage.into(),
            },
            wit::StreamEvent::ContentBlockStart(e) => Self::ContentBlockStart {
                index: e.index,
                block: e.block.into(),
            },
            wit::StreamEvent::ContentBlockDelta(e) => Self::ContentBlockDelta {
                index: e.index,
                delta: e.delta.into(),
            },
            wit::StreamEvent::ContentBlockStop(index) => Self::ContentBlockStop { index },
            wit::StreamEvent::MessageDelta(d) => Self::MessageDelta {
                stop_reason: d.stop_reason.map(Into::into),
                stop_sequence: d.stop_sequence,
                usage_delta: d.usage_delta.into(),
            },
            wit::StreamEvent::MessageStop => Self::MessageStop,
            wit::StreamEvent::Ping => Self::Ping,
            wit::StreamEvent::Warning(message) => Self::Warning { message },
        }
    }
}

// ---- ModelInfo + ListModelsResponse ---------------------------------
impl From<spp::ModelInfo> for wit::ModelInfo {
    fn from(m: spp::ModelInfo) -> Self {
        Self {
            id: m.id,
            display_name: m.display_name,
            context_window: m.context_window,
        }
    }
}
impl From<wit::ModelInfo> for spp::ModelInfo {
    fn from(m: wit::ModelInfo) -> Self {
        Self {
            id: m.id,
            display_name: m.display_name,
            context_window: m.context_window,
        }
    }
}

impl From<spp::ListModelsResponse> for wit::ListModelsResponse {
    fn from(r: spp::ListModelsResponse) -> Self {
        Self {
            models: r.models.into_iter().map(Into::into).collect(),
            default_model_id: r.default_model_id,
        }
    }
}
impl From<wit::ListModelsResponse> for spp::ListModelsResponse {
    fn from(r: wit::ListModelsResponse) -> Self {
        Self {
            models: r.models.into_iter().map(Into::into).collect(),
            default_model_id: r.default_model_id,
        }
    }
}

// ---- ErrorKind + ProviderError --------------------------------------
impl From<spp::ErrorKind> for wit::ErrorKind {
    fn from(k: spp::ErrorKind) -> Self {
        match k {
            spp::ErrorKind::InvalidRequest => Self::InvalidRequest,
            spp::ErrorKind::Authentication => Self::Authentication,
            spp::ErrorKind::PermissionDenied => Self::PermissionDenied,
            spp::ErrorKind::ModelNotFound => Self::ModelNotFound,
            spp::ErrorKind::ContextLengthExceeded => Self::ContextLengthExceeded,
            spp::ErrorKind::RateLimited => Self::RateLimited,
            spp::ErrorKind::Overloaded => Self::Overloaded,
            spp::ErrorKind::Refusal => Self::Refusal,
            spp::ErrorKind::Network => Self::Network,
            spp::ErrorKind::NotImplemented => Self::NotImplemented,
            spp::ErrorKind::Internal => Self::Internal,
        }
    }
}
impl From<wit::ErrorKind> for spp::ErrorKind {
    fn from(k: wit::ErrorKind) -> Self {
        match k {
            wit::ErrorKind::InvalidRequest => Self::InvalidRequest,
            wit::ErrorKind::Authentication => Self::Authentication,
            wit::ErrorKind::PermissionDenied => Self::PermissionDenied,
            wit::ErrorKind::ModelNotFound => Self::ModelNotFound,
            wit::ErrorKind::ContextLengthExceeded => Self::ContextLengthExceeded,
            wit::ErrorKind::RateLimited => Self::RateLimited,
            wit::ErrorKind::Overloaded => Self::Overloaded,
            wit::ErrorKind::Refusal => Self::Refusal,
            wit::ErrorKind::Network => Self::Network,
            wit::ErrorKind::NotImplemented => Self::NotImplemented,
            wit::ErrorKind::Internal => Self::Internal,
        }
    }
}

impl From<spp::ProviderError> for wit::ProviderError {
    fn from(e: spp::ProviderError) -> Self {
        Self {
            kind: e.kind.into(),
            message: e.message,
            retry_after_ms: e.retry_after_ms,
            provider_code: e.provider_code,
        }
    }
}
impl From<wit::ProviderError> for spp::ProviderError {
    fn from(e: wit::ProviderError) -> Self {
        Self {
            kind: e.kind.into(),
            message: e.message,
            retry_after_ms: e.retry_after_ms,
            provider_code: e.provider_code,
        }
    }
}

// =====================================================================
// Round-trip tests
// =====================================================================
//
// One unit test per protocol type / variant (≈25 tests), plus two
// proptest-driven sweeps over the highest-fanout values
// (`CompleteRequest` and `StreamEvent`) to catch field-permutation bugs
// the unit tests would miss.
#[cfg(test)]
mod roundtrip_tests {
    use super::*;
    use savvagent_protocol::request::ThinkingConfig;
    use savvagent_protocol::{
        BlockDelta, CompleteRequest, CompleteResponse, ContentBlock, ErrorKind, ImageSource,
        ListModelsResponse, MediaType, Message, ModelInfo, ProviderError, Role, StopReason,
        StreamEvent, ToolDef, Usage, UsageDelta,
    };

    fn rt_role(r: Role) {
        let w: wit::Role = r.into();
        let back: Role = w.into();
        assert_eq!(r, back);
    }

    #[test]
    fn role_user_roundtrip() {
        rt_role(Role::User);
    }
    #[test]
    fn role_assistant_roundtrip() {
        rt_role(Role::Assistant);
    }

    #[test]
    fn media_type_roundtrip() {
        for mt in [
            MediaType::Jpeg,
            MediaType::Png,
            MediaType::Gif,
            MediaType::Webp,
        ] {
            let w: wit::MediaType = mt.into();
            let back: MediaType = w.into();
            assert_eq!(mt, back);
        }
    }

    #[test]
    fn image_source_base64_roundtrip() {
        let src = ImageSource::Base64 {
            media_type: MediaType::Png,
            data: "aGVsbG8=".into(),
        };
        let w: wit::ImageSource = src.clone().into();
        let back: ImageSource = w.into();
        assert_eq!(src, back);
    }
    #[test]
    fn image_source_url_roundtrip() {
        let src = ImageSource::Url {
            url: "https://example.com/cat.png".into(),
        };
        let w: wit::ImageSource = src.clone().into();
        let back: ImageSource = w.into();
        assert_eq!(src, back);
    }

    #[test]
    fn content_block_text_roundtrip() {
        let b = ContentBlock::Text { text: "hi".into() };
        let w: wit::ContentBlock = b.clone().into();
        let back: ContentBlock = w.into();
        assert_eq!(b, back);
    }

    #[test]
    fn content_block_tool_use_roundtrip() {
        let b = ContentBlock::ToolUse {
            id: "call_1".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "/tmp/x", "n": 3}),
        };
        let w: wit::ContentBlock = b.clone().into();
        let back: ContentBlock = w.into();
        assert_eq!(b, back);
    }

    #[test]
    fn content_block_tool_result_roundtrip_with_nested_blocks() {
        // ToolResult.content is nested ContentBlocks; round-tripped through
        // JSON to break the WIT recursion ban.
        let b = ContentBlock::ToolResult {
            tool_use_id: "call_1".into(),
            content: vec![
                ContentBlock::Text { text: "ok".into() },
                ContentBlock::Text {
                    text: "more".into(),
                },
            ],
            is_error: false,
        };
        let w: wit::ContentBlock = b.clone().into();
        let back: ContentBlock = w.into();
        assert_eq!(b, back);
    }

    #[test]
    fn content_block_tool_result_roundtrip_is_error_true() {
        let b = ContentBlock::ToolResult {
            tool_use_id: "call_err".into(),
            content: vec![ContentBlock::Text {
                text: "boom".into(),
            }],
            is_error: true,
        };
        let w: wit::ContentBlock = b.clone().into();
        let back: ContentBlock = w.into();
        assert_eq!(b, back);
    }

    #[test]
    fn content_block_image_roundtrip() {
        let b = ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: MediaType::Webp,
                data: "Zm9v".into(),
            },
        };
        let w: wit::ContentBlock = b.clone().into();
        let back: ContentBlock = w.into();
        assert_eq!(b, back);
    }

    #[test]
    fn content_block_thinking_roundtrip() {
        let b = ContentBlock::Thinking {
            text: "let me think...".into(),
            signature: Some("sig-abc".into()),
        };
        let w: wit::ContentBlock = b.clone().into();
        let back: ContentBlock = w.into();
        assert_eq!(b, back);

        let b2 = ContentBlock::Thinking {
            text: "no signature here".into(),
            signature: None,
        };
        let w: wit::ContentBlock = b2.clone().into();
        let back: ContentBlock = w.into();
        assert_eq!(b2, back);
    }

    #[test]
    fn message_roundtrip() {
        let m = Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text { text: "hi".into() },
                ContentBlock::Text {
                    text: "again".into(),
                },
            ],
        };
        let w: wit::Message = m.clone().into();
        let back: Message = w.into();
        assert_eq!(m, back);
    }

    #[test]
    fn tool_def_roundtrip() {
        let t = ToolDef {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        };
        let w: wit::ToolDef = t.clone().into();
        let back: ToolDef = w.into();
        assert_eq!(t, back);
    }

    #[test]
    fn thinking_config_roundtrip() {
        let t = ThinkingConfig {
            budget_tokens: 8192,
        };
        let w: wit::ThinkingConfig = t.clone().into();
        let back: ThinkingConfig = w.into();
        assert_eq!(t, back);
    }

    #[test]
    fn complete_request_minimal_roundtrip() {
        let r = CompleteRequest::text("claude-sonnet-4-6", "hello", 1024);
        let w: wit::CompleteRequest = r.clone().into();
        let back: CompleteRequest = w.into();
        // CompleteRequest doesn't derive PartialEq, so compare via JSON.
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }

    #[test]
    fn complete_request_full_roundtrip() {
        let r = CompleteRequest {
            model: "gpt-4o".into(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "hi".into() }],
            }],
            system: Some("be terse".into()),
            tools: vec![ToolDef {
                name: "noop".into(),
                description: "do nothing".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            temperature: Some(0.7),
            top_p: Some(0.95),
            max_tokens: 2048,
            stop_sequences: vec!["\n\nUser:".into()],
            stream: true,
            thinking: Some(ThinkingConfig {
                budget_tokens: 4096,
            }),
            metadata: Some(serde_json::json!({"trace_id": "abc"})),
        };
        let w: wit::CompleteRequest = r.clone().into();
        let back: CompleteRequest = w.into();
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }

    fn rt_stop_reason(s: StopReason) {
        let w: wit::StopReason = s.into();
        let back: StopReason = w.into();
        assert_eq!(s, back);
    }
    #[test]
    fn stop_reason_all_variants_roundtrip() {
        for s in [
            StopReason::EndTurn,
            StopReason::ToolUse,
            StopReason::MaxTokens,
            StopReason::StopSequence,
            StopReason::Refusal,
            StopReason::Other,
        ] {
            rt_stop_reason(s);
        }
    }

    #[test]
    fn usage_roundtrip() {
        let u = Usage {
            input_tokens: 100,
            output_tokens: 250,
            cache_creation_input_tokens: Some(50),
            cache_read_input_tokens: Some(25),
        };
        let w: wit::Usage = u.clone().into();
        let back: Usage = w.into();
        assert_eq!(u, back);

        let u2 = Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let w: wit::Usage = u2.clone().into();
        let back: Usage = w.into();
        assert_eq!(u2, back);
    }

    #[test]
    fn complete_response_roundtrip() {
        let r = CompleteResponse {
            id: "msg_01".into(),
            model: "claude-sonnet-4-6".into(),
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        };
        let w: wit::CompleteResponse = r.clone().into();
        let back: CompleteResponse = w.into();
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }

    #[test]
    fn usage_delta_roundtrip() {
        let u = UsageDelta {
            output_tokens: Some(10),
            cache_read_input_tokens: Some(2),
        };
        let w: wit::UsageDelta = u.clone().into();
        let back: UsageDelta = w.into();
        assert_eq!(u, back);
    }

    fn rt_block_delta(d: BlockDelta) {
        let w: wit::BlockDelta = d.clone().into();
        let back: BlockDelta = w.into();
        assert_eq!(d, back);
    }
    #[test]
    fn block_delta_all_variants_roundtrip() {
        rt_block_delta(BlockDelta::TextDelta { text: "abc".into() });
        rt_block_delta(BlockDelta::InputJsonDelta {
            partial_json: "{\"a\":".into(),
        });
        rt_block_delta(BlockDelta::ThinkingDelta { text: "...".into() });
        rt_block_delta(BlockDelta::SignatureDelta {
            signature: "sig".into(),
        });
    }

    fn rt_stream_event(e: StreamEvent) {
        let w: wit::StreamEvent = e.clone().into();
        let back: StreamEvent = w.into();
        assert_eq!(e, back);
    }
    #[test]
    fn stream_event_message_start_roundtrip() {
        rt_stream_event(StreamEvent::MessageStart {
            id: "msg_1".into(),
            model: "m".into(),
            usage: Usage {
                input_tokens: 1,
                output_tokens: 0,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        });
    }
    #[test]
    fn stream_event_content_block_start_roundtrip() {
        rt_stream_event(StreamEvent::ContentBlockStart {
            index: 0,
            block: ContentBlock::Text { text: "".into() },
        });
    }
    #[test]
    fn stream_event_content_block_delta_roundtrip() {
        rt_stream_event(StreamEvent::ContentBlockDelta {
            index: 0,
            delta: BlockDelta::TextDelta { text: "hi".into() },
        });
    }
    #[test]
    fn stream_event_content_block_stop_roundtrip() {
        rt_stream_event(StreamEvent::ContentBlockStop { index: 0 });
    }
    #[test]
    fn stream_event_message_delta_roundtrip() {
        rt_stream_event(StreamEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            stop_sequence: Some("\n\n".into()),
            usage_delta: UsageDelta {
                output_tokens: Some(7),
                cache_read_input_tokens: None,
            },
        });
    }
    #[test]
    fn stream_event_message_stop_roundtrip() {
        rt_stream_event(StreamEvent::MessageStop);
    }
    #[test]
    fn stream_event_ping_roundtrip() {
        rt_stream_event(StreamEvent::Ping);
    }
    #[test]
    fn stream_event_warning_roundtrip() {
        rt_stream_event(StreamEvent::Warning {
            message: "slow down".into(),
        });
    }

    #[test]
    fn model_info_roundtrip() {
        let m = ModelInfo {
            id: "claude-haiku-4-5".into(),
            display_name: Some("Claude Haiku 4.5".into()),
            context_window: Some(200_000),
        };
        let w: wit::ModelInfo = m.clone().into();
        let back: ModelInfo = w.into();
        // ModelInfo doesn't derive PartialEq, compare via JSON.
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }

    #[test]
    fn list_models_response_roundtrip() {
        let r = ListModelsResponse {
            models: vec![ModelInfo {
                id: "x".into(),
                display_name: None,
                context_window: None,
            }],
            default_model_id: Some("x".into()),
        };
        let w: wit::ListModelsResponse = r.clone().into();
        let back: ListModelsResponse = w.into();
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }

    fn rt_error_kind(k: ErrorKind) {
        let w: wit::ErrorKind = k.into();
        let back: ErrorKind = w.into();
        assert_eq!(k, back);
    }
    #[test]
    fn error_kind_all_variants_roundtrip() {
        for k in [
            ErrorKind::InvalidRequest,
            ErrorKind::Authentication,
            ErrorKind::PermissionDenied,
            ErrorKind::ModelNotFound,
            ErrorKind::ContextLengthExceeded,
            ErrorKind::RateLimited,
            ErrorKind::Overloaded,
            ErrorKind::Refusal,
            ErrorKind::Network,
            ErrorKind::NotImplemented,
            ErrorKind::Internal,
        ] {
            rt_error_kind(k);
        }
    }

    #[test]
    fn provider_error_roundtrip() {
        let e = ProviderError {
            kind: ErrorKind::RateLimited,
            message: "slow down".into(),
            retry_after_ms: Some(2000),
            provider_code: Some("rate_limit_exceeded".into()),
        };
        let w: wit::ProviderError = e.clone().into();
        let back: ProviderError = w.into();
        // ProviderError doesn't derive PartialEq, compare via JSON.
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }

    // ---- Property tests ---------------------------------------------
    //
    // These two generators cover the high-fanout types (`CompleteRequest`
    // and `StreamEvent`). Each strategy mixes the simple shapes — strings,
    // primitives, the variant matrix — through a few hundred runs so a
    // typo in a field-name match arm would surface.

    use proptest::prelude::*;

    fn arb_role() -> impl Strategy<Value = Role> {
        prop_oneof![Just(Role::User), Just(Role::Assistant)]
    }

    fn arb_text_block() -> impl Strategy<Value = ContentBlock> {
        any::<String>().prop_map(|text| ContentBlock::Text { text })
    }
    fn arb_thinking_block() -> impl Strategy<Value = ContentBlock> {
        (any::<String>(), proptest::option::of(any::<String>()))
            .prop_map(|(text, signature)| ContentBlock::Thinking { text, signature })
    }
    fn arb_content_block() -> impl Strategy<Value = ContentBlock> {
        prop_oneof![arb_text_block(), arb_thinking_block(),]
    }

    fn arb_message() -> impl Strategy<Value = Message> {
        (arb_role(), prop::collection::vec(arb_content_block(), 0..3))
            .prop_map(|(role, content)| Message { role, content })
    }

    fn arb_complete_request() -> impl Strategy<Value = CompleteRequest> {
        (
            any::<String>(),
            prop::collection::vec(arb_message(), 0..4),
            proptest::option::of(any::<String>()),
            any::<u32>(),
            any::<bool>(),
        )
            .prop_map(
                |(model, messages, system, max_tokens, stream)| CompleteRequest {
                    model,
                    messages,
                    system,
                    tools: vec![],
                    temperature: None,
                    top_p: None,
                    max_tokens,
                    stop_sequences: vec![],
                    stream,
                    thinking: None,
                    metadata: None,
                },
            )
    }

    fn arb_usage() -> impl Strategy<Value = Usage> {
        (
            any::<u32>(),
            any::<u32>(),
            proptest::option::of(any::<u32>()),
            proptest::option::of(any::<u32>()),
        )
            .prop_map(|(input, output, cache_creation, cache_read)| Usage {
                input_tokens: input,
                output_tokens: output,
                cache_creation_input_tokens: cache_creation,
                cache_read_input_tokens: cache_read,
            })
    }

    fn arb_block_delta() -> impl Strategy<Value = BlockDelta> {
        prop_oneof![
            any::<String>().prop_map(|text| BlockDelta::TextDelta { text }),
            any::<String>().prop_map(|partial_json| BlockDelta::InputJsonDelta { partial_json }),
            any::<String>().prop_map(|text| BlockDelta::ThinkingDelta { text }),
            any::<String>().prop_map(|signature| BlockDelta::SignatureDelta { signature }),
        ]
    }

    fn arb_usage_delta() -> impl Strategy<Value = UsageDelta> {
        (
            proptest::option::of(any::<u32>()),
            proptest::option::of(any::<u32>()),
        )
            .prop_map(|(output_tokens, cache_read_input_tokens)| UsageDelta {
                output_tokens,
                cache_read_input_tokens,
            })
    }

    fn arb_stop_reason() -> impl Strategy<Value = StopReason> {
        prop_oneof![
            Just(StopReason::EndTurn),
            Just(StopReason::MaxTokens),
            Just(StopReason::StopSequence),
            Just(StopReason::ToolUse),
        ]
    }

    fn arb_stream_event() -> impl Strategy<Value = StreamEvent> {
        prop_oneof![
            (any::<String>(), any::<String>(), arb_usage())
                .prop_map(|(id, model, usage)| { StreamEvent::MessageStart { id, model, usage } }),
            (any::<u32>(), arb_text_block())
                .prop_map(|(index, block)| StreamEvent::ContentBlockStart { index, block }),
            (any::<u32>(), arb_block_delta())
                .prop_map(|(index, delta)| { StreamEvent::ContentBlockDelta { index, delta } }),
            any::<u32>().prop_map(|index| StreamEvent::ContentBlockStop { index }),
            (
                proptest::option::of(arb_stop_reason()),
                proptest::option::of(any::<String>()),
                arb_usage_delta(),
            )
                .prop_map(|(stop_reason, stop_sequence, usage_delta)| {
                    StreamEvent::MessageDelta {
                        stop_reason,
                        stop_sequence,
                        usage_delta,
                    }
                }),
            Just(StreamEvent::MessageStop),
            Just(StreamEvent::Ping),
            any::<String>().prop_map(|message| StreamEvent::Warning { message }),
        ]
    }

    proptest! {
        #[test]
        fn complete_request_roundtrip_property(r in arb_complete_request()) {
            let w: wit::CompleteRequest = r.clone().into();
            let back: CompleteRequest = w.into();
            prop_assert_eq!(
                serde_json::to_value(&r).unwrap(),
                serde_json::to_value(&back).unwrap()
            );
        }

        #[test]
        fn stream_event_roundtrip_property(e in arb_stream_event()) {
            let w: wit::StreamEvent = e.clone().into();
            let back: StreamEvent = w.into();
            prop_assert_eq!(e, back);
        }
    }
}
