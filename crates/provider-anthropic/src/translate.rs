//! Pure translation between SPP and Anthropic Messages API types.

use savvagent_protocol::{self as spp};

use crate::api;

/// Translate an SPP request into an Anthropic `/v1/messages` request body.
pub fn request_to_anthropic(req: &spp::CompleteRequest, stream: bool) -> api::MessageRequest {
    api::MessageRequest {
        model: req.model.clone(),
        messages: req.messages.iter().map(message_to_anthropic).collect(),
        system: req.system.clone(),
        max_tokens: req.max_tokens,
        temperature: req.temperature,
        top_p: req.top_p,
        stop_sequences: req.stop_sequences.clone(),
        tools: req.tools.iter().map(tool_to_anthropic).collect(),
        thinking: req.thinking.as_ref().map(|t| api::Thinking {
            kind: api::ThinkingKind::Enabled,
            budget_tokens: t.budget_tokens,
        }),
        metadata: req.metadata.clone(),
        stream,
    }
}

/// Translate an Anthropic response back into an SPP [`spp::CompleteResponse`].
pub fn response_from_anthropic(r: api::MessageResponse) -> spp::CompleteResponse {
    spp::CompleteResponse {
        id: r.id,
        model: r.model,
        content: r.content.into_iter().map(block_from_anthropic).collect(),
        stop_reason: r
            .stop_reason
            .map(stop_reason_from_anthropic)
            .unwrap_or(spp::StopReason::Other),
        stop_sequence: r.stop_sequence,
        usage: usage_from_anthropic(r.usage),
    }
}

fn message_to_anthropic(m: &spp::Message) -> api::Message {
    api::Message {
        role: match m.role {
            spp::Role::User => api::Role::User,
            spp::Role::Assistant => api::Role::Assistant,
        },
        content: m.content.iter().map(block_to_anthropic).collect(),
    }
}

fn block_to_anthropic(b: &spp::ContentBlock) -> api::ContentBlock {
    match b {
        spp::ContentBlock::Text { text } => api::ContentBlock::Text { text: text.clone() },
        spp::ContentBlock::ToolUse { id, name, input } => api::ContentBlock::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        },
        spp::ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => api::ContentBlock::ToolResult {
            tool_use_id: tool_use_id.clone(),
            content: content.iter().map(block_to_anthropic).collect(),
            is_error: *is_error,
        },
        spp::ContentBlock::Image { source } => api::ContentBlock::Image {
            source: image_source_to_anthropic(source),
        },
        spp::ContentBlock::Thinking { text, signature } => api::ContentBlock::Thinking {
            thinking: text.clone(),
            signature: signature.clone(),
        },
        // Html blocks are a host-side rendering hint; on the wire to
        // Anthropic we echo the source as plain text so the model can
        // reference its own prior output.
        spp::ContentBlock::Html { source, .. } => api::ContentBlock::Text {
            text: source.clone(),
        },
    }
}

fn block_from_anthropic(b: api::ContentBlock) -> spp::ContentBlock {
    match b {
        api::ContentBlock::Text { text } => spp::ContentBlock::Text { text },
        api::ContentBlock::ToolUse { id, name, input } => {
            spp::ContentBlock::ToolUse { id, name, input }
        }
        api::ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => spp::ContentBlock::ToolResult {
            tool_use_id,
            content: content.into_iter().map(block_from_anthropic).collect(),
            is_error,
        },
        api::ContentBlock::Image { source } => spp::ContentBlock::Image {
            source: image_source_from_anthropic(source),
        },
        api::ContentBlock::Thinking {
            thinking,
            signature,
        } => spp::ContentBlock::Thinking {
            text: thinking,
            signature,
        },
    }
}

fn image_source_to_anthropic(s: &spp::ImageSource) -> api::ImageSource {
    match s {
        spp::ImageSource::Base64 { media_type, data } => api::ImageSource::Base64 {
            media_type: match media_type {
                spp::MediaType::Jpeg => "image/jpeg".into(),
                spp::MediaType::Png => "image/png".into(),
                spp::MediaType::Gif => "image/gif".into(),
                spp::MediaType::Webp => "image/webp".into(),
            },
            data: data.clone(),
        },
        spp::ImageSource::Url { url } => api::ImageSource::Url { url: url.clone() },
    }
}

fn image_source_from_anthropic(s: api::ImageSource) -> spp::ImageSource {
    match s {
        api::ImageSource::Base64 { media_type, data } => {
            let mt = match media_type.as_str() {
                "image/jpeg" => spp::MediaType::Jpeg,
                "image/gif" => spp::MediaType::Gif,
                "image/webp" => spp::MediaType::Webp,
                _ => spp::MediaType::Png,
            };
            spp::ImageSource::Base64 {
                media_type: mt,
                data,
            }
        }
        api::ImageSource::Url { url } => spp::ImageSource::Url { url },
    }
}

fn tool_to_anthropic(t: &spp::ToolDef) -> api::Tool {
    api::Tool {
        name: t.name.clone(),
        description: t.description.clone(),
        input_schema: t.input_schema.clone(),
    }
}

fn stop_reason_from_anthropic(r: api::StopReason) -> spp::StopReason {
    match r {
        api::StopReason::EndTurn => spp::StopReason::EndTurn,
        api::StopReason::ToolUse => spp::StopReason::ToolUse,
        api::StopReason::MaxTokens => spp::StopReason::MaxTokens,
        api::StopReason::StopSequence => spp::StopReason::StopSequence,
        api::StopReason::Refusal => spp::StopReason::Refusal,
        api::StopReason::Other => spp::StopReason::Other,
    }
}

pub(crate) fn stop_reason_from_str(s: &str) -> spp::StopReason {
    match s {
        "end_turn" => spp::StopReason::EndTurn,
        "tool_use" => spp::StopReason::ToolUse,
        "max_tokens" => spp::StopReason::MaxTokens,
        "stop_sequence" => spp::StopReason::StopSequence,
        "refusal" => spp::StopReason::Refusal,
        _ => spp::StopReason::Other,
    }
}

fn usage_from_anthropic(u: api::Usage) -> spp::Usage {
    spp::Usage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_creation_input_tokens: u.cache_creation_input_tokens,
        cache_read_input_tokens: u.cache_read_input_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_text_message() {
        let req = spp::CompleteRequest::text("claude-x", "hello", 64);
        let body = request_to_anthropic(&req, false);
        assert_eq!(body.model, "claude-x");
        assert_eq!(body.messages.len(), 1);
        assert!(matches!(
            body.messages[0].content[0],
            api::ContentBlock::Text { .. }
        ));
        assert!(!body.stream);
    }

    #[test]
    fn translates_tool_use_round_trip() {
        let req = spp::CompleteRequest {
            model: "x".into(),
            messages: vec![spp::Message {
                role: spp::Role::Assistant,
                content: vec![spp::ContentBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "ls".into(),
                    input: json!({"path": "/"}),
                }],
            }],
            system: None,
            tools: vec![],
            temperature: None,
            top_p: None,
            max_tokens: 16,
            stop_sequences: vec![],
            stream: false,
            thinking: None,
            metadata: None,
        };
        let body = request_to_anthropic(&req, false);
        let out = serde_json::to_value(&body).unwrap();
        assert_eq!(out["messages"][0]["content"][0]["type"], "tool_use");
        assert_eq!(out["messages"][0]["content"][0]["name"], "ls");
    }

    #[test]
    fn parses_minimal_response() {
        let raw = json!({
            "id": "msg_1",
            "model": "claude-x",
            "content": [{ "type": "text", "text": "hello back" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 5, "output_tokens": 2 }
        });
        let r: api::MessageResponse = serde_json::from_value(raw).unwrap();
        let resp = response_from_anthropic(r);
        assert_eq!(resp.id, "msg_1");
        assert_eq!(resp.stop_reason, spp::StopReason::EndTurn);
        assert!(matches!(resp.content[0], spp::ContentBlock::Text { .. }));
    }

    #[test]
    fn parses_response_with_cache_usage() {
        let raw = json!({
            "id": "msg_2",
            "model": "claude-x",
            "content": [],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 1,
                "output_tokens": 0,
                "cache_creation_input_tokens": 256,
                "cache_read_input_tokens": 1024
            }
        });
        let r: api::MessageResponse = serde_json::from_value(raw).unwrap();
        let resp = response_from_anthropic(r);
        assert_eq!(resp.usage.cache_read_input_tokens, Some(1024));
        assert_eq!(resp.usage.cache_creation_input_tokens, Some(256));
    }

    #[test]
    fn unknown_stop_reason_maps_to_other() {
        let raw = json!({
            "id": "msg_3",
            "model": "x",
            "content": [],
            "stop_reason": "future_reason_we_dont_know",
            "usage": { "input_tokens": 0, "output_tokens": 0 }
        });
        let r: api::MessageResponse = serde_json::from_value(raw).unwrap();
        let resp = response_from_anthropic(r);
        assert_eq!(resp.stop_reason, spp::StopReason::Other);
    }
}
