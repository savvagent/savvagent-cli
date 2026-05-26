//! Pure translation between SPP and Gemini `generateContent` types.
//!
//! Gemini differs from SPP/Anthropic in two structural ways that this
//! module hides:
//!
//! 1. **Role naming.** Gemini uses `"user"`/`"model"`; SPP uses `"user"`
//!    /`"assistant"`.
//! 2. **Tool-call identity.** SPP carries a `tool_use_id` linking each
//!    `ToolUse` to its matching `ToolResult`; Gemini pairs `functionCall`
//!    and `functionResponse` parts by `name` only. To bridge that, we
//!    synthesize stable ids on the way out (Gemini → SPP) that embed the
//!    function name, and use a request-scoped lookup table on the way in
//!    (SPP → Gemini) to recover the name from any id.
//!
//! The synthesized id format is `gemini-<name>-<index>`. We never parse it
//! — the conversation history is always available — but encoding the name
//! makes ids self-documenting in transcripts.

use std::collections::HashMap;

use savvagent_protocol::{self as spp};

use crate::api;

/// Translate an SPP request into a Gemini `generateContent` body.
pub fn request_to_gemini(req: &spp::CompleteRequest) -> api::GenerateContentRequest {
    let id_to_name = build_tool_id_lookup(&req.messages);
    let contents = req
        .messages
        .iter()
        .map(|m| message_to_gemini(m, &id_to_name))
        .collect();
    let system_instruction = req.system.as_ref().map(|text| api::Content {
        role: None,
        parts: vec![api::Part::text(text.clone())],
    });
    let tools = if req.tools.is_empty() {
        Vec::new()
    } else {
        vec![api::Tool {
            function_declarations: req.tools.iter().map(tool_to_gemini).collect(),
        }]
    };
    let generation_config = build_generation_config(req);
    api::GenerateContentRequest {
        contents,
        system_instruction,
        tools,
        generation_config,
        metadata: req.metadata.clone(),
    }
}

/// Translate a Gemini non-streaming response into an SPP [`spp::CompleteResponse`].
pub fn response_from_gemini(r: api::GenerateContentResponse) -> spp::CompleteResponse {
    let id = r.response_id.unwrap_or_else(|| "gemini-response".into());
    let model = r.model_version.unwrap_or_default();
    let usage = usage_from_gemini(r.usage_metadata.unwrap_or_default());

    let candidate = r.candidates.into_iter().next();
    let (content, stop_reason) = match candidate {
        Some(c) => {
            let blocks: Vec<spp::ContentBlock> = c
                .content
                .map(|c| {
                    let mut counter = 0u32;
                    c.parts
                        .into_iter()
                        .filter_map(|p| part_to_spp(p, &mut counter))
                        .collect()
                })
                .unwrap_or_default();
            // Gemini does not signal tool use in `finishReason` (it stays
            // "STOP" even when the candidate carries a functionCall part).
            // Override based on the actual content so the host can drive its
            // tool-use loop on stop_reason like every other provider. If we
            // are overriding away from a Refusal/Other signal, warn so the
            // original isn't silently lost.
            let mapped = stop_reason_from_gemini(c.finish_reason.as_deref());
            let has_tool_use = blocks
                .iter()
                .any(|b| matches!(b, spp::ContentBlock::ToolUse { .. }));
            let stop = if has_tool_use {
                if matches!(mapped, spp::StopReason::Refusal | spp::StopReason::Other) {
                    tracing::warn!(
                        original_stop_reason = ?mapped,
                        "gemini emitted a functionCall alongside a refusal/other finishReason; overriding stop_reason to ToolUse"
                    );
                }
                spp::StopReason::ToolUse
            } else {
                mapped
            };
            (blocks, stop)
        }
        None => {
            let stop = if let Some(pf) = &r.prompt_feedback {
                if pf.block_reason.is_some() {
                    spp::StopReason::Refusal
                } else {
                    spp::StopReason::Other
                }
            } else {
                spp::StopReason::Other
            };
            (Vec::new(), stop)
        }
    };

    spp::CompleteResponse {
        id,
        model,
        content,
        stop_reason,
        stop_sequence: None,
        usage,
    }
}

pub(crate) fn stop_reason_from_gemini(s: Option<&str>) -> spp::StopReason {
    match s {
        Some("STOP") => spp::StopReason::EndTurn,
        Some("MAX_TOKENS") => spp::StopReason::MaxTokens,
        Some("SAFETY")
        | Some("RECITATION")
        | Some("BLOCKLIST")
        | Some("PROHIBITED_CONTENT")
        | Some("SPII")
        | Some("IMAGE_SAFETY") => spp::StopReason::Refusal,
        // Gemini emits a synthetic FUNCTION_CALL stop reason in some flows;
        // hosts rely on the presence of a `tool_use` block more than this.
        Some("MALFORMED_FUNCTION_CALL") => spp::StopReason::Other,
        Some("OTHER") | None | Some("FINISH_REASON_UNSPECIFIED") => spp::StopReason::Other,
        _ => spp::StopReason::Other,
    }
}

pub(crate) fn synthesize_tool_use_id(name: &str, index: u32) -> String {
    format!("gemini-{name}-{index}")
}

/// Build a `tool_use_id → function_name` lookup from prior assistant turns
/// in the request, so we can rebuild a `functionResponse` from a SPP
/// `ToolResult` without storing per-session state.
fn build_tool_id_lookup(messages: &[spp::Message]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for m in messages {
        if !matches!(m.role, spp::Role::Assistant) {
            continue;
        }
        for b in &m.content {
            if let spp::ContentBlock::ToolUse { id, name, .. } = b {
                map.insert(id.clone(), name.clone());
            }
        }
    }
    map
}

fn message_to_gemini(m: &spp::Message, id_to_name: &HashMap<String, String>) -> api::Content {
    let role = Some(
        match m.role {
            spp::Role::User => "user",
            spp::Role::Assistant => "model",
        }
        .to_string(),
    );
    let mut parts = Vec::with_capacity(m.content.len());
    for b in &m.content {
        push_part_for_block(b, id_to_name, &mut parts);
    }
    if parts.is_empty() {
        // Gemini rejects messages with zero parts; keep the turn intact by
        // sending an empty text part rather than dropping the turn.
        parts.push(api::Part::text(String::new()));
    }
    api::Content { role, parts }
}

fn push_part_for_block(
    b: &spp::ContentBlock,
    id_to_name: &HashMap<String, String>,
    out: &mut Vec<api::Part>,
) {
    match b {
        spp::ContentBlock::Text { text } => out.push(api::Part::text(text.clone())),
        spp::ContentBlock::ToolUse { name, input, .. } => out.push(api::Part {
            function_call: Some(api::FunctionCall {
                name: name.clone(),
                args: input.clone(),
            }),
            ..api::Part::default()
        }),
        spp::ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            // Gemini routes function results by name; recover the name from
            // the matching ToolUse earlier in the conversation. If absent
            // (model never called it), fall back to a placeholder so we don't
            // silently drop the turn.
            let name = id_to_name
                .get(tool_use_id)
                .cloned()
                .unwrap_or_else(|| "unknown_tool".to_string());
            let result_text = flatten_tool_result_text(content);
            let response = if *is_error {
                serde_json::json!({ "error": result_text })
            } else {
                serde_json::json!({ "result": result_text })
            };
            out.push(api::Part {
                function_response: Some(api::FunctionResponse { name, response }),
                ..api::Part::default()
            });
        }
        spp::ContentBlock::Image { source } => {
            if let Some(part) = image_to_gemini(source) {
                out.push(part);
            }
        }
        spp::ContentBlock::Thinking { text, signature } => out.push(api::Part {
            text: Some(text.clone()),
            thought: Some(true),
            thought_signature: signature.clone(),
            ..api::Part::default()
        }),
        // Html source is echoed to Gemini as plain text so the model can
        // reference its own prior canvas output. The host is what renders.
        spp::ContentBlock::Html { source, .. } => out.push(api::Part::text(source.clone())),
    }
}

fn flatten_tool_result_text(blocks: &[spp::ContentBlock]) -> String {
    let mut buf = String::new();
    for b in blocks {
        if let spp::ContentBlock::Text { text } = b {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(text);
        }
    }
    buf
}

fn image_to_gemini(s: &spp::ImageSource) -> Option<api::Part> {
    match s {
        spp::ImageSource::Base64 { media_type, data } => Some(api::Part {
            inline_data: Some(api::InlineData {
                mime_type: match media_type {
                    spp::MediaType::Jpeg => "image/jpeg".into(),
                    spp::MediaType::Png => "image/png".into(),
                    spp::MediaType::Gif => "image/gif".into(),
                    spp::MediaType::Webp => "image/webp".into(),
                },
                data: data.clone(),
            }),
            ..api::Part::default()
        }),
        // Gemini supports `fileData { fileUri, mimeType }` for URLs but
        // requires the URI to be hosted on Google Cloud. Inlining is the
        // safer cross-provider default; URL-sourced images would need the
        // host to fetch and base64 them first.
        spp::ImageSource::Url { .. } => None,
    }
}

fn tool_to_gemini(t: &spp::ToolDef) -> api::FunctionDeclaration {
    api::FunctionDeclaration {
        name: t.name.clone(),
        description: t.description.clone(),
        parameters: crate::schema::sanitize(&t.input_schema),
    }
}

fn build_generation_config(req: &spp::CompleteRequest) -> Option<api::GenerationConfig> {
    let thinking = req.thinking.as_ref().map(|t| api::ThinkingConfig {
        thinking_budget: Some(t.budget_tokens as i32),
        include_thoughts: Some(true),
    });
    if req.temperature.is_none()
        && req.top_p.is_none()
        && req.stop_sequences.is_empty()
        && thinking.is_none()
    {
        // max_tokens is always present on SPP, so the config is never fully
        // empty in practice — but keep the shape minimal when the host
        // sent only the required fields.
        return Some(api::GenerationConfig {
            max_output_tokens: Some(req.max_tokens),
            ..api::GenerationConfig::default()
        });
    }
    Some(api::GenerationConfig {
        temperature: req.temperature,
        top_p: req.top_p,
        max_output_tokens: Some(req.max_tokens),
        stop_sequences: req.stop_sequences.clone(),
        thinking_config: thinking,
    })
}

/// Translate a Gemini `Part` into one SPP [`spp::ContentBlock`]. The
/// `tool_use_counter` is incremented for each `functionCall` part to make
/// synthesized `tool_use_id`s unique across the response.
pub(crate) fn part_to_spp(p: api::Part, tool_use_counter: &mut u32) -> Option<spp::ContentBlock> {
    if let Some(fc) = p.function_call {
        let id = synthesize_tool_use_id(&fc.name, *tool_use_counter);
        *tool_use_counter += 1;
        return Some(spp::ContentBlock::ToolUse {
            id,
            name: fc.name,
            input: fc.args,
        });
    }
    if let Some(text) = p.text {
        if matches!(p.thought, Some(true)) {
            return Some(spp::ContentBlock::Thinking {
                text,
                signature: p.thought_signature,
            });
        }
        return Some(spp::ContentBlock::Text { text });
    }
    if let Some(inline) = p.inline_data {
        let mt = match inline.mime_type.as_str() {
            "image/jpeg" => spp::MediaType::Jpeg,
            "image/gif" => spp::MediaType::Gif,
            "image/webp" => spp::MediaType::Webp,
            _ => spp::MediaType::Png,
        };
        return Some(spp::ContentBlock::Image {
            source: spp::ImageSource::Base64 {
                media_type: mt,
                data: inline.data,
            },
        });
    }
    // function_response parts shouldn't appear in model output; if they do,
    // drop them rather than fabricating a content block.
    None
}

fn usage_from_gemini(u: api::UsageMetadata) -> spp::Usage {
    spp::Usage {
        input_tokens: u.prompt_token_count,
        output_tokens: u.candidates_token_count.unwrap_or(0),
        cache_creation_input_tokens: None,
        cache_read_input_tokens: u.cached_content_token_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_text_message() {
        let req = spp::CompleteRequest::text("gemini-x", "hello", 64);
        let body = request_to_gemini(&req);
        assert_eq!(body.contents.len(), 1);
        assert_eq!(body.contents[0].role.as_deref(), Some("user"));
        assert_eq!(body.contents[0].parts[0].text.as_deref(), Some("hello"));
        let cfg = body.generation_config.expect("generation config");
        assert_eq!(cfg.max_output_tokens, Some(64));
    }

    #[test]
    fn translates_assistant_role_to_model() {
        let req = spp::CompleteRequest {
            model: "x".into(),
            messages: vec![spp::Message {
                role: spp::Role::Assistant,
                content: vec![spp::ContentBlock::Text { text: "ok".into() }],
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
        let body = request_to_gemini(&req);
        assert_eq!(body.contents[0].role.as_deref(), Some("model"));
    }

    #[test]
    fn tool_use_id_resolves_back_to_function_name() {
        // Round-trip: assistant emits a function call, host echoes the
        // assistant turn back, then sends a ToolResult in a user turn.
        // The SPP→Gemini translator must use the original function name on
        // the resulting functionResponse.
        let id = synthesize_tool_use_id("ls", 0);
        let req = spp::CompleteRequest {
            model: "x".into(),
            messages: vec![
                spp::Message {
                    role: spp::Role::User,
                    content: vec![spp::ContentBlock::Text {
                        text: "list /tmp".into(),
                    }],
                },
                spp::Message {
                    role: spp::Role::Assistant,
                    content: vec![spp::ContentBlock::ToolUse {
                        id: id.clone(),
                        name: "ls".into(),
                        input: json!({"path": "/tmp"}),
                    }],
                },
                spp::Message {
                    role: spp::Role::User,
                    content: vec![spp::ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
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
        let body = request_to_gemini(&req);
        let resp_part = &body.contents[2].parts[0];
        let fr = resp_part
            .function_response
            .as_ref()
            .expect("expected functionResponse");
        assert_eq!(fr.name, "ls");
        assert_eq!(fr.response["result"], "a\nb");
    }

    #[test]
    fn parses_minimal_response() {
        let raw = json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": "hi back"}]
                },
                "finishReason": "STOP",
                "index": 0
            }],
            "usageMetadata": {
                "promptTokenCount": 5,
                "candidatesTokenCount": 2
            },
            "modelVersion": "gemini-x",
            "responseId": "resp_1"
        });
        let r: api::GenerateContentResponse = serde_json::from_value(raw).unwrap();
        let resp = response_from_gemini(r);
        assert_eq!(resp.id, "resp_1");
        assert_eq!(resp.stop_reason, spp::StopReason::EndTurn);
        assert_eq!(resp.usage.output_tokens, 2);
        match &resp.content[0] {
            spp::ContentBlock::Text { text } => assert_eq!(text, "hi back"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn parses_function_call_response() {
        let raw = json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": "ls",
                            "args": {"path": "/tmp"}
                        }
                    }]
                },
                "finishReason": "STOP",
                "index": 0
            }],
            "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 4}
        });
        let r: api::GenerateContentResponse = serde_json::from_value(raw).unwrap();
        let resp = response_from_gemini(r);
        assert_eq!(resp.stop_reason, spp::StopReason::ToolUse);
        match &resp.content[0] {
            spp::ContentBlock::ToolUse { name, input, id } => {
                assert_eq!(name, "ls");
                assert_eq!(input["path"], "/tmp");
                assert!(id.starts_with("gemini-ls-"));
            }
            _ => panic!("expected tool_use"),
        }
    }

    #[test]
    fn safety_finish_reason_maps_to_refusal() {
        let raw = json!({
            "candidates": [{
                "content": {"role": "model", "parts": []},
                "finishReason": "SAFETY",
                "index": 0
            }]
        });
        let r: api::GenerateContentResponse = serde_json::from_value(raw).unwrap();
        let resp = response_from_gemini(r);
        assert_eq!(resp.stop_reason, spp::StopReason::Refusal);
    }
}
