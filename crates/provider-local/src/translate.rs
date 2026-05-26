//! Pure translation between SPP and Ollama `/api/chat` types.
//!
//! Ollama's tool-call schema follows the OpenAI convention:
//! - Request carries `tools: [{ type: "function", function: { name, description, parameters } }]`
//! - Response message carries `tool_calls: [{ id?, function: { name, arguments } }]`
//! - Tool results come back as `role: "tool"` messages
//!
//! For models that don't support tool calling, the `tools` array is simply
//! omitted and any tool-result messages are rendered as user text so the
//! conversation stays valid.

use savvagent_protocol::{self as spp};

use crate::api;

/// Translate an SPP request into an Ollama `/api/chat` body.
pub fn request_to_ollama(req: &spp::CompleteRequest, stream: bool) -> api::ChatRequest {
    let has_tool_support = !req.tools.is_empty();

    let mut messages: Vec<api::Message> = Vec::new();

    // Ollama expects a system message first when a system prompt is present.
    if let Some(sys) = &req.system {
        messages.push(api::Message::text("system", sys.clone()));
    }

    for m in &req.messages {
        push_messages_for_spp(m, &mut messages, has_tool_support);
    }

    let tools: Vec<api::Tool> = req.tools.iter().map(tool_to_ollama).collect();

    let options = build_options(req);

    api::ChatRequest {
        model: req.model.clone(),
        messages,
        stream,
        tools,
        options,
    }
}

/// Translate a final (non-streaming) Ollama response into SPP.
pub fn response_from_ollama(r: api::ChatResponse) -> spp::CompleteResponse {
    let model = r.model.unwrap_or_default();
    let usage = spp::Usage {
        input_tokens: r.prompt_eval_count.unwrap_or(0),
        output_tokens: r.eval_count.unwrap_or(0),
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    };
    let stop_reason = stop_reason_from_ollama(r.done_reason.as_deref());

    let content = r
        .message
        .map(|m| message_content_to_spp(&m))
        .unwrap_or_default();

    spp::CompleteResponse {
        id: format!("ollama-{}", uuid_v4_simple()),
        model,
        content,
        stop_reason,
        stop_sequence: None,
        usage,
    }
}

pub(crate) fn stop_reason_from_ollama(reason: Option<&str>) -> spp::StopReason {
    match reason {
        Some("stop") => spp::StopReason::EndTurn,
        Some("length") => spp::StopReason::MaxTokens,
        Some("tool_calls") => spp::StopReason::ToolUse,
        _ => spp::StopReason::EndTurn,
    }
}

/// Extract SPP content blocks from an Ollama response message.
pub(crate) fn message_content_to_spp(m: &api::Message) -> Vec<spp::ContentBlock> {
    let mut blocks = Vec::new();

    // Text content.
    if let Some(text) = message_text(m) {
        if !text.is_empty() {
            blocks.push(spp::ContentBlock::Text { text });
        }
    }

    // Tool calls.
    for (idx, tc) in m.tool_calls.iter().enumerate() {
        let id = tc
            .id
            .clone()
            .unwrap_or_else(|| format!("ollama-{}-{idx}", tc.function.name));
        blocks.push(spp::ContentBlock::ToolUse {
            id,
            name: tc.function.name.clone(),
            input: tc.function.arguments.clone(),
        });
    }

    blocks
}

/// Extract the text string from an Ollama message `content` field.
///
/// Ollama sends content as either a plain JSON string or sometimes as an
/// array of content parts (rare in current versions). We handle both.
pub(crate) fn message_text(m: &api::Message) -> Option<String> {
    match &m.content {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Array(parts)) => {
            // If the model ever sends content parts, concatenate text fields.
            let text: String = parts
                .iter()
                .filter_map(|p| p.get("text")?.as_str().map(String::from))
                .collect::<Vec<_>>()
                .join("");
            if text.is_empty() { None } else { Some(text) }
        }
        Some(serde_json::Value::Null) | None => None,
        // Fallback: serialize whatever we got.
        Some(other) => Some(other.to_string()),
    }
}

fn push_messages_for_spp(m: &spp::Message, out: &mut Vec<api::Message>, has_tool_support: bool) {
    match m.role {
        spp::Role::User => {
            // Walk the blocks in their original order so the conversation
            // context the model sees mirrors what the host built. Adjacent
            // text blocks are merged into a single user message; each
            // tool_result is emitted as its own message (tool-role when the
            // model supports tools, otherwise rendered as user text).
            let mut text_buf: Vec<&str> = Vec::new();

            // Helper: flush any buffered text as one user message.
            fn flush_text(buf: &mut Vec<&str>, out: &mut Vec<api::Message>) {
                if !buf.is_empty() {
                    let combined = buf.join("\n");
                    out.push(api::Message::text("user", combined));
                    buf.clear();
                }
            }

            for b in &m.content {
                match b {
                    spp::ContentBlock::Text { text } => text_buf.push(text.as_str()),
                    spp::ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        // Preserve order: flush pending text BEFORE the
                        // tool_result so the assistant sees them in the
                        // sequence the host produced.
                        flush_text(&mut text_buf, out);
                        let result_text = flatten_tool_result_text(content);
                        let result = if *is_error {
                            format!("[error] {result_text}")
                        } else {
                            result_text
                        };
                        if has_tool_support {
                            out.push(api::Message::tool_result(tool_use_id.as_str(), result));
                        } else {
                            // No tool calling on this model — render the
                            // result as user text so the conversation
                            // round-trips intact.
                            out.push(api::Message::text(
                                "user",
                                format!("[tool result for {}]: {}", tool_use_id, result),
                            ));
                        }
                    }
                    spp::ContentBlock::Image { .. } => {
                        // Ollama's vision support is model-dependent; skip
                        // images rather than crashing.
                    }
                    // Html source is echoed back as plain text; see provider-{anthropic,openai,gemini} translators.
                    spp::ContentBlock::Html { source, .. } => text_buf.push(source.as_str()),
                    _ => {}
                }
            }

            // Trailing text, if any.
            flush_text(&mut text_buf, out);
        }
        spp::Role::Assistant => {
            // SPP assistant messages can contain text and/or tool_use blocks.
            // Reconstruct an Ollama assistant message with both.
            let mut text_buf = String::new();
            let mut tool_calls: Vec<api::ToolCall> = Vec::new();

            for b in &m.content {
                match b {
                    spp::ContentBlock::Text { text } => {
                        if !text_buf.is_empty() {
                            text_buf.push('\n');
                        }
                        text_buf.push_str(text.as_str());
                    }
                    spp::ContentBlock::ToolUse { id, name, input } => {
                        tool_calls.push(api::ToolCall {
                            id: Some(id.clone()),
                            function: api::FunctionCall {
                                name: name.clone(),
                                arguments: input.clone(),
                            },
                        });
                    }
                    // Html source is echoed back as plain text; see provider-{anthropic,openai,gemini} translators.
                    spp::ContentBlock::Html { source, .. } => {
                        if !text_buf.is_empty() {
                            text_buf.push('\n');
                        }
                        text_buf.push_str(source.as_str());
                    }
                    _ => {}
                }
            }

            let content = if text_buf.is_empty() {
                None
            } else {
                Some(serde_json::Value::String(text_buf))
            };

            out.push(api::Message {
                role: "assistant".into(),
                content,
                tool_calls,
            });
        }
    }
}

fn flatten_tool_result_text(blocks: &[spp::ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| {
            if let spp::ContentBlock::Text { text } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_to_ollama(t: &spp::ToolDef) -> api::Tool {
    api::Tool {
        kind: "function".into(),
        function: api::FunctionDef {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.input_schema.clone(),
        },
    }
}

fn build_options(req: &spp::CompleteRequest) -> Option<api::Options> {
    let opts = api::Options {
        temperature: req.temperature,
        top_p: req.top_p,
        num_predict: Some(req.max_tokens),
        stop: req.stop_sequences.clone(),
    };
    // Return None only when everything is default to keep the payload clean.
    if opts.temperature.is_none()
        && opts.top_p.is_none()
        && opts.num_predict == Some(0)
        && opts.stop.is_empty()
    {
        return None;
    }
    Some(opts)
}

/// Cheap pseudo-unique id using timestamp + a counter — avoids pulling uuid
/// into every response path.
fn uuid_v4_simple() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let n = CTR.fetch_add(1, Ordering::Relaxed);
    format!("{ts:x}-{n:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn simple_req() -> spp::CompleteRequest {
        spp::CompleteRequest::text("llama3.2", "hello", 64)
    }

    #[test]
    fn text_message_translates_to_user_role() {
        let req = simple_req();
        let body = request_to_ollama(&req, false);
        assert_eq!(body.model, "llama3.2");
        assert!(!body.stream);
        // One user message, no system.
        assert_eq!(body.messages.len(), 1);
        assert_eq!(body.messages[0].role, "user");
        let content = body.messages[0].content.as_ref().unwrap();
        assert_eq!(content, &json!("hello"));
    }

    #[test]
    fn system_prompt_becomes_first_message() {
        let req = spp::CompleteRequest {
            model: "llama3.2".into(),
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
        let body = request_to_ollama(&req, false);
        assert_eq!(body.messages[0].role, "system");
        assert_eq!(body.messages[1].role, "user");
    }

    #[test]
    fn assistant_turn_preserves_text() {
        let req = spp::CompleteRequest {
            model: "x".into(),
            messages: vec![spp::Message {
                role: spp::Role::Assistant,
                content: vec![spp::ContentBlock::Text {
                    text: "sure".into(),
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
        let body = request_to_ollama(&req, false);
        assert_eq!(body.messages[0].role, "assistant");
        assert_eq!(body.messages[0].content, Some(json!("sure")));
    }

    #[test]
    fn tool_def_becomes_function_tool() {
        use savvagent_protocol::ToolDef;
        let req = spp::CompleteRequest {
            model: "llama3.1".into(),
            messages: vec![spp::Message {
                role: spp::Role::User,
                content: vec![spp::ContentBlock::Text {
                    text: "call ls".into(),
                }],
            }],
            system: None,
            tools: vec![ToolDef {
                name: "ls".into(),
                description: "list dir".into(),
                input_schema: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
            }],
            temperature: None,
            top_p: None,
            max_tokens: 32,
            stop_sequences: vec![],
            stream: false,
            thinking: None,
            metadata: None,
        };
        let body = request_to_ollama(&req, false);
        assert_eq!(body.tools.len(), 1);
        assert_eq!(body.tools[0].kind, "function");
        assert_eq!(body.tools[0].function.name, "ls");
    }

    #[test]
    fn tool_result_with_tool_support_uses_tool_role() {
        use savvagent_protocol::ToolDef;
        let req = spp::CompleteRequest {
            model: "llama3.1".into(),
            messages: vec![
                spp::Message {
                    role: spp::Role::Assistant,
                    content: vec![spp::ContentBlock::ToolUse {
                        id: "call-1".into(),
                        name: "ls".into(),
                        input: json!({"path": "/tmp"}),
                    }],
                },
                spp::Message {
                    role: spp::Role::User,
                    content: vec![spp::ContentBlock::ToolResult {
                        tool_use_id: "call-1".into(),
                        content: vec![spp::ContentBlock::Text {
                            text: "a\nb".into(),
                        }],
                        is_error: false,
                    }],
                },
            ],
            system: None,
            tools: vec![ToolDef {
                name: "ls".into(),
                description: "list".into(),
                input_schema: json!({}),
            }],
            temperature: None,
            top_p: None,
            max_tokens: 32,
            stop_sequences: vec![],
            stream: false,
            thinking: None,
            metadata: None,
        };
        let body = request_to_ollama(&req, false);
        // assistant + tool result
        let tool_msg = body.messages.iter().find(|m| m.role == "tool").unwrap();
        let content = tool_msg.content.as_ref().unwrap();
        assert_eq!(content["tool_call_id"], "call-1");
        assert_eq!(content["content"], "a\nb");
    }

    #[test]
    fn tool_result_without_tool_support_uses_user_role() {
        // When no tools are defined, tool results should be sent as user messages.
        let req = spp::CompleteRequest {
            model: "llama3.2".into(),
            messages: vec![spp::Message {
                role: spp::Role::User,
                content: vec![spp::ContentBlock::ToolResult {
                    tool_use_id: "call-1".into(),
                    content: vec![spp::ContentBlock::Text {
                        text: "result".into(),
                    }],
                    is_error: false,
                }],
            }],
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
        let body = request_to_ollama(&req, false);
        assert!(body.messages.iter().any(|m| m.role == "user"));
        assert!(!body.messages.iter().any(|m| m.role == "tool"));
    }

    #[test]
    fn response_from_ollama_parses_text() {
        let raw = serde_json::from_value::<crate::api::ChatResponse>(json!({
            "model": "llama3.2",
            "message": { "role": "assistant", "content": "hello back" },
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 5,
            "eval_count": 2
        }))
        .unwrap();
        let resp = response_from_ollama(raw);
        assert_eq!(resp.model, "llama3.2");
        assert_eq!(resp.stop_reason, spp::StopReason::EndTurn);
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.usage.output_tokens, 2);
        match &resp.content[0] {
            spp::ContentBlock::Text { text } => assert_eq!(text, "hello back"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn response_from_ollama_parses_tool_call() {
        let raw = serde_json::from_value::<crate::api::ChatResponse>(json!({
            "model": "llama3.1",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "tc-1",
                    "function": { "name": "ls", "arguments": { "path": "/tmp" } }
                }]
            },
            "done": true,
            "done_reason": "tool_calls"
        }))
        .unwrap();
        let resp = response_from_ollama(raw);
        match &resp.content[0] {
            spp::ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tc-1");
                assert_eq!(name, "ls");
                assert_eq!(input["path"], "/tmp");
            }
            _ => panic!("expected tool_use"),
        }
        assert_eq!(resp.stop_reason, spp::StopReason::ToolUse);
    }

    #[test]
    fn user_message_with_interleaved_text_and_tool_results_preserves_order() {
        use savvagent_protocol::ToolDef;
        // SPP user message:
        //   [text "before"][tool_result A][text "between"][tool_result B][text "after"]
        // The Ollama messages must arrive in that exact order so the model
        // sees the same conversational sequence.
        let req = spp::CompleteRequest {
            model: "llama3.1".into(),
            messages: vec![spp::Message {
                role: spp::Role::User,
                content: vec![
                    spp::ContentBlock::Text {
                        text: "before".into(),
                    },
                    spp::ContentBlock::ToolResult {
                        tool_use_id: "call-A".into(),
                        content: vec![spp::ContentBlock::Text {
                            text: "result A".into(),
                        }],
                        is_error: false,
                    },
                    spp::ContentBlock::Text {
                        text: "between".into(),
                    },
                    spp::ContentBlock::ToolResult {
                        tool_use_id: "call-B".into(),
                        content: vec![spp::ContentBlock::Text {
                            text: "result B".into(),
                        }],
                        is_error: false,
                    },
                    spp::ContentBlock::Text {
                        text: "after".into(),
                    },
                ],
            }],
            system: None,
            tools: vec![ToolDef {
                name: "noop".into(),
                description: "noop".into(),
                input_schema: json!({}),
            }],
            temperature: None,
            top_p: None,
            max_tokens: 32,
            stop_sequences: vec![],
            stream: false,
            thinking: None,
            metadata: None,
        };
        let body = request_to_ollama(&req, false);

        // Expect: 5 messages, in this exact order.
        let summary: Vec<(String, String)> = body
            .messages
            .iter()
            .map(|m| {
                let s = match &m.content {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                (m.role.clone(), s)
            })
            .collect();

        assert_eq!(summary.len(), 5, "got {summary:?}");
        assert_eq!(summary[0].0, "user");
        assert_eq!(summary[0].1, "before");
        assert_eq!(summary[1].0, "tool");
        assert!(summary[1].1.contains("call-A"));
        assert!(summary[1].1.contains("result A"));
        assert_eq!(summary[2].0, "user");
        assert_eq!(summary[2].1, "between");
        assert_eq!(summary[3].0, "tool");
        assert!(summary[3].1.contains("call-B"));
        assert!(summary[3].1.contains("result B"));
        assert_eq!(summary[4].0, "user");
        assert_eq!(summary[4].1, "after");
    }

    #[test]
    fn user_message_without_tool_support_preserves_order_as_user_text() {
        // Same shape as above but with no tools defined — every block
        // becomes a user message and the order is still preserved.
        let req = spp::CompleteRequest {
            model: "llama3.2".into(),
            messages: vec![spp::Message {
                role: spp::Role::User,
                content: vec![
                    spp::ContentBlock::Text {
                        text: "first".into(),
                    },
                    spp::ContentBlock::ToolResult {
                        tool_use_id: "call-X".into(),
                        content: vec![spp::ContentBlock::Text {
                            text: "X result".into(),
                        }],
                        is_error: false,
                    },
                    spp::ContentBlock::Text {
                        text: "last".into(),
                    },
                ],
            }],
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
        let body = request_to_ollama(&req, false);
        assert!(body.messages.iter().all(|m| m.role == "user"));
        let texts: Vec<String> = body
            .messages
            .iter()
            .map(|m| match &m.content {
                Some(serde_json::Value::String(s)) => s.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(texts.len(), 3);
        assert_eq!(texts[0], "first");
        assert!(texts[1].contains("call-X") && texts[1].contains("X result"));
        assert_eq!(texts[2], "last");
    }

    #[test]
    fn options_include_max_tokens() {
        let req = spp::CompleteRequest {
            model: "llama3.2".into(),
            messages: vec![spp::Message {
                role: spp::Role::User,
                content: vec![spp::ContentBlock::Text { text: "hi".into() }],
            }],
            system: None,
            tools: vec![],
            temperature: Some(0.7),
            top_p: None,
            max_tokens: 256,
            stop_sequences: vec![],
            stream: false,
            thinking: None,
            metadata: None,
        };
        let body = request_to_ollama(&req, false);
        let opts = body.options.unwrap();
        assert_eq!(opts.num_predict, Some(256));
        assert_eq!(opts.temperature, Some(0.7));
    }

    #[test]
    fn html_block_in_assistant_turn_is_included_as_text() {
        // An assistant message containing a ContentBlock::Html must not be
        // silently dropped — multi-turn canvas conversations against Ollama
        // depend on this content being echoed back.
        let req = spp::CompleteRequest {
            model: "llama3.2".into(),
            messages: vec![spp::Message {
                role: spp::Role::Assistant,
                content: vec![spp::ContentBlock::Html {
                    source: "<p>x</p>".into(),
                    state: None,
                }],
            }],
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
        let body = request_to_ollama(&req, false);
        assert_eq!(body.messages.len(), 1);
        assert_eq!(body.messages[0].role, "assistant");
        let content = body.messages[0].content.as_ref().expect("content missing");
        assert!(
            content.as_str().unwrap_or("").contains("<p>x</p>"),
            "expected html source in content, got: {content:?}",
        );
    }

    #[test]
    fn html_block_in_user_turn_is_included_as_text() {
        // Same guarantee for the User role: Html source must not be dropped.
        let req = spp::CompleteRequest {
            model: "llama3.2".into(),
            messages: vec![spp::Message {
                role: spp::Role::User,
                content: vec![spp::ContentBlock::Html {
                    source: "<p>hello</p>".into(),
                    state: None,
                }],
            }],
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
        let body = request_to_ollama(&req, false);
        assert_eq!(body.messages.len(), 1);
        assert_eq!(body.messages[0].role, "user");
        let content = body.messages[0].content.as_ref().expect("content missing");
        assert!(
            content.as_str().unwrap_or("").contains("<p>hello</p>"),
            "expected html source in content, got: {content:?}",
        );
    }
}
