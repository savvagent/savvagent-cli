//! Pure translation between SPP and OpenAI Chat Completions types.
//!
//! OpenAI differs from SPP in a few structural ways this module hides:
//!
//! 1. **System messages** are first-class role-bearing messages in OpenAI's
//!    schema, not a separate top-level field.
//! 2. **Tool-call identity.** SPP carries a `tool_use_id` linking each
//!    `ToolUse` to its matching `ToolResult`; OpenAI uses the same `id`
//!    field verbatim in `tool_calls` / `tool_call_id`. We preserve the SPP id
//!    directly — no synthesis needed.
//! 3. **Tool-result role.** SPP delivers tool results inside a `user` turn;
//!    OpenAI requires a dedicated `tool` role message with a `tool_call_id`.
//! 4. **Image messages.** SPP image blocks become `image_url` content parts
//!    with a `data:` URI. URL-sourced images are forwarded directly.

use savvagent_protocol::{self as spp};

use crate::api;

/// Translate an SPP request into an OpenAI Chat Completions body.
pub fn request_to_openai(req: &spp::CompleteRequest, stream: bool) -> api::ChatCompletionRequest {
    let mut messages = Vec::new();

    // System prompt becomes a dedicated system-role message at the front.
    if let Some(system) = &req.system {
        messages.push(api::RequestMessage::System {
            content: system.clone(),
        });
    }

    for m in &req.messages {
        flatten_message(m, &mut messages);
    }

    let tools: Vec<api::Tool> = req.tools.iter().map(tool_to_openai).collect();
    // Intentionally leave `tool_choice` unset. OpenAI defaults to `"auto"`
    // when `tools` is non-empty, and forcing `"auto"` here would preempt any
    // future SPP-level `tool_choice` controls.
    let tool_choice = None;

    api::ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        tools,
        tool_choice,
        temperature: req.temperature,
        top_p: req.top_p,
        max_tokens: req.max_tokens,
        stop: req.stop_sequences.clone(),
        stream,
        stream_options: if stream {
            Some(api::StreamOptions {
                include_usage: true,
            })
        } else {
            None
        },
    }
}

/// Translate a non-streaming OpenAI response into an SPP [`spp::CompleteResponse`].
pub fn response_from_openai(r: api::ChatCompletionResponse) -> spp::CompleteResponse {
    let usage = r.usage.map(usage_from_openai).unwrap_or_default();

    let choice = r.choices.into_iter().next();
    let (content, stop_reason) = match choice {
        Some(c) => {
            let mut blocks = Vec::new();
            if let Some(text) = c.message.content {
                if !text.is_empty() {
                    blocks.push(spp::ContentBlock::Text { text });
                }
            }
            for tc in c.message.tool_calls {
                let input = parse_tool_arguments(&tc.function.arguments);
                blocks.push(spp::ContentBlock::ToolUse {
                    id: tc.id,
                    name: tc.function.name,
                    input,
                });
            }
            let stop = stop_reason_from_str(c.finish_reason.as_deref());
            (blocks, stop)
        }
        None => (Vec::new(), spp::StopReason::Other),
    };

    spp::CompleteResponse {
        id: r.id,
        model: r.model,
        content,
        stop_reason,
        stop_sequence: None,
        usage,
    }
}

pub(crate) fn stop_reason_from_str(s: Option<&str>) -> spp::StopReason {
    match s {
        Some("stop") => spp::StopReason::EndTurn,
        Some("length") => spp::StopReason::MaxTokens,
        Some("tool_calls") | Some("function_call") => spp::StopReason::ToolUse,
        Some("content_filter") => spp::StopReason::Refusal,
        _ => spp::StopReason::Other,
    }
}

fn flatten_message(m: &spp::Message, out: &mut Vec<api::RequestMessage>) {
    match m.role {
        spp::Role::Assistant => {
            // Collect text and tool-use blocks separately.
            let mut text_parts = Vec::new();
            let mut tool_calls: Vec<api::RequestToolCall> = Vec::new();

            for b in &m.content {
                match b {
                    spp::ContentBlock::Text { text } => text_parts.push(text.clone()),
                    // Html source is echoed back to the model as plain text so
                    // it can reference its own prior canvas output. The host is
                    // responsible for rendering; the wire is plain text.
                    spp::ContentBlock::Html { source, .. } => text_parts.push(source.clone()),
                    spp::ContentBlock::ToolUse { id, name, input } => {
                        tool_calls.push(api::RequestToolCall {
                            id: id.clone(),
                            call_type: "function".into(),
                            function: api::RequestFunction {
                                name: name.clone(),
                                // OpenAI expects arguments as a JSON string.
                                arguments: serde_json::to_string(input)
                                    .unwrap_or_else(|_| "{}".into()),
                            },
                        });
                    }
                    // Thinking blocks are internal; skip in history reconstruction.
                    spp::ContentBlock::Thinking { .. } => {}
                    spp::ContentBlock::Image { .. } => {}
                    spp::ContentBlock::ToolResult { .. } => {
                        // ToolResult in an assistant turn is a protocol error;
                        // ignore gracefully.
                    }
                }
            }

            let content = if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join(""))
            };

            out.push(api::RequestMessage::Assistant {
                content,
                tool_calls,
            });
        }
        spp::Role::User => {
            // A user turn may be mixed: regular content + tool results.
            // Tool results become separate `tool` role messages.
            let mut user_parts: Vec<api::ContentPart> = Vec::new();

            for b in &m.content {
                match b {
                    spp::ContentBlock::Text { text } => {
                        user_parts.push(api::ContentPart::Text { text: text.clone() });
                    }
                    // Html source is echoed back as plain text; see assistant arm above.
                    spp::ContentBlock::Html { source, .. } => {
                        user_parts.push(api::ContentPart::Text {
                            text: source.clone(),
                        });
                    }
                    spp::ContentBlock::Image { source } => {
                        if let Some(part) = image_to_openai(source) {
                            user_parts.push(part);
                        }
                    }
                    spp::ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        // Flush any accumulated user content first.
                        if !user_parts.is_empty() {
                            let parts = std::mem::take(&mut user_parts);
                            out.push(user_message_from_parts(parts));
                        }
                        let result_text = flatten_tool_result(content, *is_error);
                        out.push(api::RequestMessage::Tool {
                            tool_call_id: tool_use_id.clone(),
                            content: result_text,
                        });
                    }
                    spp::ContentBlock::Thinking { .. } => {}
                    spp::ContentBlock::ToolUse { .. } => {}
                }
            }

            if !user_parts.is_empty() {
                out.push(user_message_from_parts(user_parts));
            }
        }
    }
}

fn user_message_from_parts(parts: Vec<api::ContentPart>) -> api::RequestMessage {
    if parts.len() == 1 {
        if let api::ContentPart::Text { text } = &parts[0] {
            return api::RequestMessage::User {
                content: api::UserContent::Text(text.clone()),
            };
        }
    }
    api::RequestMessage::User {
        content: api::UserContent::Parts(parts),
    }
}

fn flatten_tool_result(blocks: &[spp::ContentBlock], is_error: bool) -> String {
    let text: String = blocks
        .iter()
        .filter_map(|b| {
            if let spp::ContentBlock::Text { text } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    if is_error {
        format!("Error: {text}")
    } else {
        text
    }
}

fn image_to_openai(source: &spp::ImageSource) -> Option<api::ContentPart> {
    let url = match source {
        spp::ImageSource::Base64 { media_type, data } => {
            let mime = match media_type {
                spp::MediaType::Jpeg => "image/jpeg",
                spp::MediaType::Png => "image/png",
                spp::MediaType::Gif => "image/gif",
                spp::MediaType::Webp => "image/webp",
            };
            format!("data:{mime};base64,{data}")
        }
        spp::ImageSource::Url { url } => url.clone(),
    };
    Some(api::ContentPart::ImageUrl {
        image_url: api::ImageUrl { url, detail: None },
    })
}

fn tool_to_openai(t: &spp::ToolDef) -> api::Tool {
    api::Tool {
        tool_type: "function".into(),
        function: api::FunctionDef {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.input_schema.clone(),
        },
    }
}

pub(crate) fn parse_tool_arguments(args: &str) -> serde_json::Value {
    if args.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(args).unwrap_or_else(|_| serde_json::json!({}))
    }
}

pub(crate) fn usage_from_openai(u: api::UsageStats) -> spp::Usage {
    spp::Usage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn translates_simple_text_request() {
        let req = spp::CompleteRequest::text("gpt-4o-mini", "hello", 64);
        let body = request_to_openai(&req, false);
        assert_eq!(body.model, "gpt-4o-mini");
        assert_eq!(body.max_tokens, 64);
        assert!(!body.stream);
        assert_eq!(body.messages.len(), 1);
        assert!(
            matches!(&body.messages[0], api::RequestMessage::User { content: api::UserContent::Text(t) } if t == "hello")
        );
    }

    #[test]
    fn system_prompt_becomes_first_message() {
        let req = spp::CompleteRequest {
            model: "gpt-4o".into(),
            messages: vec![spp::Message {
                role: spp::Role::User,
                content: vec![spp::ContentBlock::Text { text: "hi".into() }],
            }],
            system: Some("You are helpful.".into()),
            tools: vec![],
            temperature: None,
            top_p: None,
            max_tokens: 32,
            stop_sequences: vec![],
            stream: false,
            thinking: None,
            metadata: None,
        };
        let body = request_to_openai(&req, false);
        assert_eq!(body.messages.len(), 2);
        assert!(matches!(
            &body.messages[0],
            api::RequestMessage::System { .. }
        ));
    }

    #[test]
    fn assistant_role_with_tool_calls() {
        let req = spp::CompleteRequest {
            model: "gpt-4o".into(),
            messages: vec![
                spp::Message {
                    role: spp::Role::User,
                    content: vec![spp::ContentBlock::Text {
                        text: "call ls".into(),
                    }],
                },
                spp::Message {
                    role: spp::Role::Assistant,
                    content: vec![spp::ContentBlock::ToolUse {
                        id: "call_abc".into(),
                        name: "ls".into(),
                        input: json!({"path": "/tmp"}),
                    }],
                },
                spp::Message {
                    role: spp::Role::User,
                    content: vec![spp::ContentBlock::ToolResult {
                        tool_use_id: "call_abc".into(),
                        content: vec![spp::ContentBlock::Text {
                            text: "a\nb".into(),
                        }],
                        is_error: false,
                    }],
                },
            ],
            system: None,
            tools: vec![],
            temperature: None,
            top_p: None,
            max_tokens: 32,
            stop_sequences: vec![],
            stream: false,
            thinking: None,
            metadata: None,
        };
        let body = request_to_openai(&req, false);
        // messages: user("call ls") + assistant(tool_call) + tool(result)
        assert_eq!(body.messages.len(), 3);

        let asst = &body.messages[1];
        assert!(
            matches!(asst, api::RequestMessage::Assistant { tool_calls, .. } if tool_calls.len() == 1)
        );

        let tool_msg = &body.messages[2];
        match tool_msg {
            api::RequestMessage::Tool {
                tool_call_id,
                content,
            } => {
                assert_eq!(tool_call_id, "call_abc");
                assert_eq!(content, "a\nb");
            }
            _ => panic!("expected tool message"),
        }
    }

    #[test]
    fn tool_result_error_is_prefixed() {
        let content = vec![spp::ContentBlock::Text {
            text: "not found".into(),
        }];
        assert_eq!(flatten_tool_result(&content, true), "Error: not found");
        assert_eq!(flatten_tool_result(&content, false), "not found");
    }

    #[test]
    fn parses_minimal_non_streaming_response() {
        let raw = json!({
            "id": "chatcmpl-123",
            "model": "gpt-4o-mini",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "hi there"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 2,
                "total_tokens": 7
            }
        });
        let r: api::ChatCompletionResponse = serde_json::from_value(raw).unwrap();
        let resp = response_from_openai(r);
        assert_eq!(resp.id, "chatcmpl-123");
        assert_eq!(resp.stop_reason, spp::StopReason::EndTurn);
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.usage.output_tokens, 2);
        match &resp.content[0] {
            spp::ContentBlock::Text { text } => assert_eq!(text, "hi there"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn parses_tool_use_response() {
        let raw = json!({
            "id": "chatcmpl-456",
            "model": "gpt-4o",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_xyz",
                        "type": "function",
                        "function": {
                            "name": "ls",
                            "arguments": "{\"path\":\"/tmp\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let r: api::ChatCompletionResponse = serde_json::from_value(raw).unwrap();
        let resp = response_from_openai(r);
        assert_eq!(resp.stop_reason, spp::StopReason::ToolUse);
        match &resp.content[0] {
            spp::ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_xyz");
                assert_eq!(name, "ls");
                assert_eq!(input["path"], "/tmp");
            }
            _ => panic!("expected tool_use"),
        }
    }

    #[test]
    fn stop_reason_mapping() {
        assert_eq!(stop_reason_from_str(Some("stop")), spp::StopReason::EndTurn);
        assert_eq!(
            stop_reason_from_str(Some("length")),
            spp::StopReason::MaxTokens
        );
        assert_eq!(
            stop_reason_from_str(Some("tool_calls")),
            spp::StopReason::ToolUse
        );
        assert_eq!(
            stop_reason_from_str(Some("content_filter")),
            spp::StopReason::Refusal
        );
        assert_eq!(stop_reason_from_str(None), spp::StopReason::Other);
    }
}
