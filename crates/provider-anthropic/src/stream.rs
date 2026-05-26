//! Anthropic SSE → SPP [`StreamEvent`] adapter.
//!
//! Anthropic streams the Messages API as Server-Sent Events whose `data:`
//! payloads are JSON objects with a `type` field. We parse them, translate
//! into [`spp::StreamEvent`], emit each over the [`StreamEmitter`], and
//! accumulate enough state to assemble the final [`spp::CompleteResponse`]
//! when the stream ends.

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use savvagent_fence::{FenceChunk, FenceParser};
use savvagent_mcp::StreamEmitter;
use savvagent_protocol::{
    self as spp, BlockDelta, ContentBlock, StopReason, StreamEvent, Usage, UsageDelta,
};
use serde::Deserialize;
use std::collections::HashMap;

use crate::translate::stop_reason_from_str;

/// Drive an Anthropic SSE response to completion, emitting SPP events along
/// the way and returning the assembled [`spp::CompleteResponse`].
pub async fn consume_sse(
    resp: reqwest::Response,
    emit: &dyn StreamEmitter,
) -> Result<spp::CompleteResponse, spp::ProviderError> {
    let mut acc = Accumulator::default();
    let mut sse = SseDecoder::new(resp);

    while let Some(frame) = sse.next().await? {
        let SseFrame { event, data } = frame;
        if event.as_deref() == Some("ping") {
            let _ = emit.emit(StreamEvent::Ping).await;
            continue;
        }

        let raw: AnthropicEvent = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(e) => {
                let _ = emit
                    .emit(StreamEvent::Warning {
                        message: format!("invalid SSE payload: {e}"),
                    })
                    .await;
                continue;
            }
        };

        for ev in acc.consume(raw) {
            // Best-effort: a disconnected emitter should not abort the call,
            // because the host can still want the final result.
            let _ = emit.emit(ev).await;
        }
    }

    acc.finish()
}

#[derive(Default)]
struct Accumulator {
    id: Option<String>,
    model: Option<String>,
    /// Block partial state, keyed by upstream Anthropic block index.
    blocks: Vec<BlockState>,
    stop_reason: Option<StopReason>,
    stop_sequence: Option<String>,
    usage: Usage,
    /// Monotonic SPP-output block-index allocator. Independent from the
    /// upstream Anthropic index space because a single upstream text block
    /// may fan out into multiple SPP Text/Html blocks (one per
    /// `html-canvas` fence transition).
    local_next_index: u32,
    /// First local SPP index assigned to each upstream block. Used to
    /// route non-text deltas (`input_json_delta`, `thinking_delta`,
    /// `signature_delta`) back to the right SPP block.
    upstream_to_local: HashMap<u32, u32>,
    /// The SPP block currently open in the output stream (one at a time).
    current_local_block: Option<LocalBlock>,
    /// Final assembled content blocks for `CompleteResponse`, in the
    /// order they were streamed (i.e., reflecting any fence-driven
    /// Text/Html splits).
    final_blocks: Vec<ContentBlock>,
    /// Extracts ``` ```html-canvas ``` fences from streaming TextDelta
    /// fragments. Reset between upstream text blocks so an unclosed
    /// fence at one block boundary doesn't leak into the next.
    fence_parser: FenceParser,
}

#[derive(Debug)]
enum BlockState {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        partial_json: String,
    },
    Thinking {
        text: String,
        signature: Option<String>,
    },
    Image,
}

/// Tracks the SPP block currently open in the output stream so we can
/// close-and-reopen across fence transitions and emit the right `Stop`
/// when the upstream's enclosing block ends.
#[derive(Debug)]
struct LocalBlock {
    index: u32,
    kind: LocalBlockKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalBlockKind {
    Text,
    Html,
    ToolUse,
    Thinking,
    Image,
}

impl Accumulator {
    fn ensure_block(&mut self, idx: usize, init: BlockState) {
        while self.blocks.len() <= idx {
            self.blocks.push(BlockState::Text(String::new()));
        }
        self.blocks[idx] = init;
    }

    fn consume(&mut self, ev: AnthropicEvent) -> Vec<StreamEvent> {
        match ev {
            AnthropicEvent::MessageStart { message } => {
                self.id = Some(message.id.clone());
                self.model = Some(message.model.clone());
                self.usage.input_tokens = message.usage.input_tokens;
                self.usage.cache_creation_input_tokens = message.usage.cache_creation_input_tokens;
                self.usage.cache_read_input_tokens = message.usage.cache_read_input_tokens;
                vec![StreamEvent::MessageStart {
                    id: message.id,
                    model: message.model,
                    usage: self.usage.clone(),
                }]
            }
            AnthropicEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                // Allocate the upstream-block-state slot first; the SPP
                // emission below depends only on the block *kind*, not on
                // whether the slot existed.
                let mut events = Vec::new();
                match content_block {
                    SseContentBlock::Text { text } => {
                        // Defer emission of ContentBlockStart for Text
                        // blocks: the first TextDelta might split into a
                        // Text or Html chunk, and we'd like to emit the
                        // right SPP block kind on first sight rather than
                        // emit Text then immediately close it.
                        self.ensure_block(index as usize, BlockState::Text(text));
                    }
                    SseContentBlock::ToolUse { id, name, input } => {
                        self.close_current_streaming_block(&mut events);
                        let local_idx = self.alloc_local(index);
                        events.push(StreamEvent::ContentBlockStart {
                            index: local_idx,
                            block: ContentBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.unwrap_or(serde_json::json!({})),
                            },
                        });
                        self.current_local_block = Some(LocalBlock {
                            index: local_idx,
                            kind: LocalBlockKind::ToolUse,
                        });
                        self.ensure_block(
                            index as usize,
                            BlockState::ToolUse {
                                id,
                                name,
                                partial_json: String::new(),
                            },
                        );
                    }
                    SseContentBlock::Thinking {
                        thinking,
                        signature,
                    } => {
                        self.close_current_streaming_block(&mut events);
                        let local_idx = self.alloc_local(index);
                        events.push(StreamEvent::ContentBlockStart {
                            index: local_idx,
                            block: ContentBlock::Thinking {
                                text: thinking.clone(),
                                signature: signature.clone(),
                            },
                        });
                        self.current_local_block = Some(LocalBlock {
                            index: local_idx,
                            kind: LocalBlockKind::Thinking,
                        });
                        self.ensure_block(
                            index as usize,
                            BlockState::Thinking {
                                text: thinking,
                                signature,
                            },
                        );
                    }
                    SseContentBlock::Image => {
                        self.close_current_streaming_block(&mut events);
                        let local_idx = self.alloc_local(index);
                        events.push(StreamEvent::ContentBlockStart {
                            index: local_idx,
                            block: ContentBlock::Text {
                                text: String::new(),
                            },
                        });
                        self.current_local_block = Some(LocalBlock {
                            index: local_idx,
                            kind: LocalBlockKind::Image,
                        });
                        self.ensure_block(index as usize, BlockState::Image);
                    }
                };
                events
            }
            AnthropicEvent::ContentBlockDelta { index, delta } => {
                let mut events = Vec::new();
                match delta {
                    SseDelta::TextDelta { text } => {
                        // Accumulate the raw upstream text for any
                        // downstream consumer that inspects state, then
                        // feed it through the fence parser.
                        if let Some(BlockState::Text(buf)) = self.blocks.get_mut(index as usize) {
                            buf.push_str(&text);
                        }
                        let chunks = self.fence_parser.push(&text);
                        for chunk in chunks {
                            self.emit_fence_chunk(chunk, &mut events);
                        }
                    }
                    SseDelta::InputJsonDelta { partial_json } => {
                        if let Some(BlockState::ToolUse {
                            partial_json: buf, ..
                        }) = self.blocks.get_mut(index as usize)
                        {
                            buf.push_str(&partial_json);
                        }
                        let local_idx =
                            self.upstream_to_local.get(&index).copied().unwrap_or(index);
                        events.push(StreamEvent::ContentBlockDelta {
                            index: local_idx,
                            delta: BlockDelta::InputJsonDelta { partial_json },
                        });
                    }
                    SseDelta::ThinkingDelta { thinking } => {
                        if let Some(BlockState::Thinking { text, .. }) =
                            self.blocks.get_mut(index as usize)
                        {
                            text.push_str(&thinking);
                        }
                        let local_idx =
                            self.upstream_to_local.get(&index).copied().unwrap_or(index);
                        events.push(StreamEvent::ContentBlockDelta {
                            index: local_idx,
                            delta: BlockDelta::ThinkingDelta { text: thinking },
                        });
                    }
                    SseDelta::SignatureDelta { signature } => {
                        if let Some(BlockState::Thinking { signature: sig, .. }) =
                            self.blocks.get_mut(index as usize)
                        {
                            *sig = Some(signature.clone());
                        }
                        let local_idx =
                            self.upstream_to_local.get(&index).copied().unwrap_or(index);
                        events.push(StreamEvent::ContentBlockDelta {
                            index: local_idx,
                            delta: BlockDelta::SignatureDelta { signature },
                        });
                    }
                };
                events
            }
            AnthropicEvent::ContentBlockStop { index } => {
                let mut events = Vec::new();
                // If this upstream block was a Text block, drain the
                // fence parser first so any buffered trailing content
                // is emitted before we close.
                let is_text = matches!(self.blocks.get(index as usize), Some(BlockState::Text(_)));
                if is_text {
                    let parser = std::mem::take(&mut self.fence_parser);
                    let finish = parser.finish();
                    for chunk in finish.chunks {
                        self.emit_fence_chunk(chunk, &mut events);
                    }
                    if finish.unclosed_fence {
                        tracing::warn!(
                            upstream_block = index,
                            "anthropic text block ended with unclosed html-canvas fence"
                        );
                    }
                }
                self.close_current_streaming_block(&mut events);
                events
            }
            AnthropicEvent::MessageDelta { delta, usage } => {
                if let Some(reason) = delta.stop_reason.as_deref() {
                    self.stop_reason = Some(stop_reason_from_str(reason));
                }
                if delta.stop_sequence.is_some() {
                    self.stop_sequence = delta.stop_sequence.clone();
                }
                if let Some(out) = usage.output_tokens {
                    self.usage.output_tokens = self.usage.output_tokens.saturating_add(out);
                }
                vec![StreamEvent::MessageDelta {
                    stop_reason: self.stop_reason,
                    stop_sequence: self.stop_sequence.clone(),
                    usage_delta: UsageDelta {
                        output_tokens: usage.output_tokens,
                        cache_read_input_tokens: usage.cache_read_input_tokens,
                    },
                }]
            }
            AnthropicEvent::MessageStop => vec![StreamEvent::MessageStop],
            AnthropicEvent::Ping => vec![StreamEvent::Ping],
            AnthropicEvent::Error { error } => vec![StreamEvent::Warning {
                message: format!("{}: {}", error.kind, error.message),
            }],
        }
    }

    fn finish(self) -> Result<spp::CompleteResponse, spp::ProviderError> {
        let id = self
            .id
            .ok_or_else(|| stream_decode_error("missing message_start"))?;
        let model = self.model.unwrap_or_default();
        // The streamed output may have reshuffled the upstream's single
        // text block into a sequence of Text/Html blocks driven by
        // html-canvas fences. `final_blocks` records that streamed
        // structure verbatim; mid-stream tool_use/thinking blocks were
        // appended at the moment their upstream ContentBlockStop arrived
        // (via `close_current_streaming_block`). To assemble the final
        // response we also need to handle the case where no streaming
        // happened (e.g., tool_use upstream blocks that were closed but
        // also had partial_json deltas — these are already accounted for).
        //
        // The fallback path (`self.final_blocks` empty + `self.blocks`
        // populated) preserves the prior behavior for the rare case that
        // a stream ended without any ContentBlockStop events: we walk
        // self.blocks and synthesize content from BlockState.
        let mut content = self.final_blocks;
        if content.is_empty() {
            for b in self.blocks {
                match b {
                    BlockState::Text(text) => content.push(ContentBlock::Text { text }),
                    BlockState::ToolUse {
                        id,
                        name,
                        partial_json,
                    } => {
                        let input = if partial_json.is_empty() {
                            serde_json::json!({})
                        } else {
                            serde_json::from_str(&partial_json).map_err(|e| {
                                stream_decode_error(&format!("tool_use partial_json invalid: {e}"))
                            })?
                        };
                        content.push(ContentBlock::ToolUse { id, name, input });
                    }
                    BlockState::Thinking { text, signature } => {
                        content.push(ContentBlock::Thinking { text, signature });
                    }
                    BlockState::Image => {}
                }
            }
        }
        Ok(spp::CompleteResponse {
            id,
            model,
            content,
            stop_reason: self.stop_reason.unwrap_or(StopReason::EndTurn),
            stop_sequence: self.stop_sequence,
            usage: self.usage,
        })
    }

    /// Allocate a fresh SPP block index and record the upstream→local
    /// mapping. Returns the allocated local index.
    fn alloc_local(&mut self, upstream_idx: u32) -> u32 {
        let local = self.local_next_index;
        self.local_next_index += 1;
        self.upstream_to_local.entry(upstream_idx).or_insert(local);
        local
    }

    /// Allocate a local index without recording an upstream mapping
    /// (used for synthesized Html blocks that don't correspond to a
    /// distinct upstream block).
    fn alloc_local_unmapped(&mut self) -> u32 {
        let local = self.local_next_index;
        self.local_next_index += 1;
        local
    }

    /// Dispatch a fence-parser-produced chunk: open or extend the matching
    /// streaming block, closing-and-reopening across kind switches.
    fn emit_fence_chunk(&mut self, chunk: FenceChunk, out: &mut Vec<StreamEvent>) {
        match chunk {
            FenceChunk::Text(text) => self.emit_text_chunk(text, out),
            FenceChunk::Html(html) => self.emit_html_chunk(html, out),
        }
    }

    fn emit_text_chunk(&mut self, text: String, out: &mut Vec<StreamEvent>) {
        // If a non-text streaming block is open, close it first.
        if let Some(cur) = &self.current_local_block {
            if cur.kind != LocalBlockKind::Text {
                self.close_current_streaming_block(out);
            }
        }
        let idx = match &self.current_local_block {
            Some(LocalBlock {
                index,
                kind: LocalBlockKind::Text,
            }) => *index,
            _ => {
                let idx = self.alloc_local_unmapped();
                out.push(StreamEvent::ContentBlockStart {
                    index: idx,
                    block: ContentBlock::Text {
                        text: String::new(),
                    },
                });
                self.current_local_block = Some(LocalBlock {
                    index: idx,
                    kind: LocalBlockKind::Text,
                });
                idx
            }
        };
        out.push(StreamEvent::ContentBlockDelta {
            index: idx,
            delta: BlockDelta::TextDelta { text: text.clone() },
        });
        // Maintain a parallel record of the streamed structure for the
        // eventual CompleteResponse.
        match self.final_blocks.last_mut() {
            Some(ContentBlock::Text { text: buf }) => buf.push_str(&text),
            _ => self.final_blocks.push(ContentBlock::Text { text }),
        }
    }

    fn emit_html_chunk(&mut self, html: String, out: &mut Vec<StreamEvent>) {
        if let Some(cur) = &self.current_local_block {
            if cur.kind != LocalBlockKind::Html {
                self.close_current_streaming_block(out);
            }
        }
        let idx = match &self.current_local_block {
            Some(LocalBlock {
                index,
                kind: LocalBlockKind::Html,
            }) => *index,
            _ => {
                let idx = self.alloc_local_unmapped();
                out.push(StreamEvent::ContentBlockStart {
                    index: idx,
                    block: ContentBlock::Html {
                        source: String::new(),
                        state: None,
                    },
                });
                self.current_local_block = Some(LocalBlock {
                    index: idx,
                    kind: LocalBlockKind::Html,
                });
                idx
            }
        };
        out.push(StreamEvent::ContentBlockDelta {
            index: idx,
            delta: BlockDelta::HtmlSourceDelta {
                source: html.clone(),
            },
        });
        match self.final_blocks.last_mut() {
            Some(ContentBlock::Html { source: buf, .. }) => buf.push_str(&html),
            _ => self.final_blocks.push(ContentBlock::Html {
                source: html,
                state: None,
            }),
        }
    }

    /// Close whichever local block is currently open (Text/Html
    /// streaming, ToolUse/Thinking/Image atomic), emitting the SPP
    /// `ContentBlockStop` and appending the assembled block to
    /// `final_blocks` so it lands in the eventual `CompleteResponse`.
    fn close_current_streaming_block(&mut self, out: &mut Vec<StreamEvent>) {
        let Some(cur) = self.current_local_block.take() else {
            return;
        };
        out.push(StreamEvent::ContentBlockStop { index: cur.index });

        // For non-streaming-text blocks, append the finished block now.
        // Streaming Text/Html blocks already pushed themselves to
        // `final_blocks` as deltas arrived; their `last_mut()` slot is
        // the current contents, so we just leave it in place.
        match cur.kind {
            LocalBlockKind::Text | LocalBlockKind::Html => {
                // Already in final_blocks; nothing to do.
            }
            LocalBlockKind::ToolUse => {
                // Find the upstream block by reverse-lookup of
                // upstream_to_local. There should be exactly one
                // mapping pointing at `cur.index` for the ToolUse case.
                let upstream = self
                    .upstream_to_local
                    .iter()
                    .find(|(_, v)| **v == cur.index)
                    .map(|(k, _)| *k);
                if let Some(u) = upstream {
                    if let Some(BlockState::ToolUse {
                        id,
                        name,
                        partial_json,
                    }) = self.blocks.get(u as usize)
                    {
                        let input = if partial_json.is_empty() {
                            serde_json::json!({})
                        } else {
                            serde_json::from_str(partial_json).unwrap_or(serde_json::json!({}))
                        };
                        self.final_blocks.push(ContentBlock::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input,
                        });
                    }
                }
            }
            LocalBlockKind::Thinking => {
                let upstream = self
                    .upstream_to_local
                    .iter()
                    .find(|(_, v)| **v == cur.index)
                    .map(|(k, _)| *k);
                if let Some(u) = upstream {
                    if let Some(BlockState::Thinking { text, signature }) =
                        self.blocks.get(u as usize)
                    {
                        self.final_blocks.push(ContentBlock::Thinking {
                            text: text.clone(),
                            signature: signature.clone(),
                        });
                    }
                }
            }
            LocalBlockKind::Image => {
                // Anthropic doesn't actually send Image blocks downstream;
                // we treat them as no-ops in the final response (matching
                // the prior behavior in the original `finish` path).
            }
        }
    }
}

fn stream_decode_error(msg: &str) -> spp::ProviderError {
    spp::ProviderError {
        kind: spp::ErrorKind::Internal,
        message: format!("stream decode error: {msg}"),
        retry_after_ms: None,
        provider_code: None,
    }
}

// ---- Anthropic SSE event JSON shapes ----

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicEvent {
    MessageStart {
        message: SseMessage,
    },
    ContentBlockStart {
        index: u32,
        content_block: SseContentBlock,
    },
    ContentBlockDelta {
        index: u32,
        delta: SseDelta,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        delta: SseMessageDelta,
        usage: SseUsageDelta,
    },
    MessageStop,
    Ping,
    Error {
        error: SseError,
    },
}

#[derive(Debug, Deserialize)]
struct SseMessage {
    id: String,
    model: String,
    #[serde(default)]
    usage: SseInitialUsage,
}

#[derive(Debug, Default, Deserialize)]
struct SseInitialUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SseContentBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Option<serde_json::Value>,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    #[serde(other)]
    Image,
}

// Variants mirror Anthropic's `delta.type` wire field (`text_delta`,
// `input_json_delta`, …) one-for-one, so the shared `Delta` postfix is
// required by the protocol — not a naming smell.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SseDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
}

#[derive(Debug, Deserialize)]
struct SseMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_sequence: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SseUsageDelta {
    #[serde(default)]
    output_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SseError {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

// ---- Tiny SSE byte-stream decoder ----

struct SseDecoder {
    inner: futures::stream::BoxStream<'static, reqwest::Result<Bytes>>,
    buf: BytesMut,
}

#[derive(Debug)]
struct SseFrame {
    event: Option<String>,
    data: String,
}

impl SseDecoder {
    fn new(resp: reqwest::Response) -> Self {
        Self {
            inner: resp.bytes_stream().boxed(),
            buf: BytesMut::with_capacity(8 * 1024),
        }
    }

    async fn next(&mut self) -> Result<Option<SseFrame>, spp::ProviderError> {
        loop {
            if let Some(frame) = self.try_pop_frame() {
                return Ok(Some(frame));
            }
            match self.inner.next().await {
                Some(Ok(chunk)) => self.buf.extend_from_slice(&chunk),
                Some(Err(e)) => {
                    return Err(spp::ProviderError {
                        kind: spp::ErrorKind::Network,
                        message: e.to_string(),
                        retry_after_ms: None,
                        provider_code: None,
                    });
                }
                None => return Ok(self.try_pop_frame()),
            }
        }
    }

    fn try_pop_frame(&mut self) -> Option<SseFrame> {
        let end = {
            let bytes = &self.buf[..];
            let mut sep_idx = None;
            let len = bytes.len();
            let mut i = 0;
            while i + 1 < len {
                if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
                    sep_idx = Some(i + 2);
                    break;
                }
                if i + 3 < len
                    && bytes[i] == b'\r'
                    && bytes[i + 1] == b'\n'
                    && bytes[i + 2] == b'\r'
                    && bytes[i + 3] == b'\n'
                {
                    sep_idx = Some(i + 4);
                    break;
                }
                i += 1;
            }
            sep_idx?
        };
        let frame_bytes = self.buf.split_to(end);
        let text = std::str::from_utf8(&frame_bytes).ok()?;
        let mut event = None;
        let mut data_lines = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("event:") {
                event = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
            // ignore comment lines (starting with ':') and id:/retry: fields.
        }
        // empty frames (e.g. from a stray separator) are ignored
        if data_lines.is_empty() && event.is_none() {
            return self.try_pop_frame();
        }
        Some(SseFrame {
            event,
            data: data_lines.join("\n"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulator_assembles_text() {
        let mut acc = Accumulator::default();
        acc.consume(AnthropicEvent::MessageStart {
            message: SseMessage {
                id: "m1".into(),
                model: "claude-x".into(),
                usage: SseInitialUsage {
                    input_tokens: 5,
                    ..Default::default()
                },
            },
        });
        acc.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: SseContentBlock::Text {
                text: String::new(),
            },
        });
        acc.consume(AnthropicEvent::ContentBlockDelta {
            index: 0,
            delta: SseDelta::TextDelta { text: "hi".into() },
        });
        acc.consume(AnthropicEvent::ContentBlockDelta {
            index: 0,
            delta: SseDelta::TextDelta {
                text: " there".into(),
            },
        });
        acc.consume(AnthropicEvent::ContentBlockStop { index: 0 });
        acc.consume(AnthropicEvent::MessageDelta {
            delta: SseMessageDelta {
                stop_reason: Some("end_turn".into()),
                stop_sequence: None,
            },
            usage: SseUsageDelta {
                output_tokens: Some(2),
                ..Default::default()
            },
        });
        acc.consume(AnthropicEvent::MessageStop);

        let out = acc.finish().unwrap();
        assert_eq!(out.id, "m1");
        assert_eq!(out.usage.output_tokens, 2);
        assert_eq!(out.stop_reason, StopReason::EndTurn);
        match &out.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hi there"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn accumulator_assembles_tool_use_input() {
        let mut acc = Accumulator::default();
        acc.consume(AnthropicEvent::MessageStart {
            message: SseMessage {
                id: "m2".into(),
                model: "claude-x".into(),
                usage: Default::default(),
            },
        });
        acc.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: SseContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "ls".into(),
                input: None,
            },
        });
        acc.consume(AnthropicEvent::ContentBlockDelta {
            index: 0,
            delta: SseDelta::InputJsonDelta {
                partial_json: "{\"path\":\"".into(),
            },
        });
        acc.consume(AnthropicEvent::ContentBlockDelta {
            index: 0,
            delta: SseDelta::InputJsonDelta {
                partial_json: "/tmp\"}".into(),
            },
        });
        acc.consume(AnthropicEvent::ContentBlockStop { index: 0 });
        acc.consume(AnthropicEvent::MessageDelta {
            delta: SseMessageDelta {
                stop_reason: Some("tool_use".into()),
                stop_sequence: None,
            },
            usage: SseUsageDelta {
                output_tokens: Some(7),
                ..Default::default()
            },
        });
        acc.consume(AnthropicEvent::MessageStop);

        let out = acc.finish().unwrap();
        assert_eq!(out.stop_reason, StopReason::ToolUse);
        match &out.content[0] {
            ContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "ls");
                assert_eq!(input["path"], "/tmp");
            }
            _ => panic!("expected tool_use"),
        }
    }

    /// When the model emits text containing a `html-canvas` fenced HTML
    /// block, the SPP stream must split the upstream's single text block
    /// into a Text block plus a separate Html block (each with its own
    /// SPP block index), with fenced markers stripped:
    ///   ContentBlockStart(Text) + TextDelta + ContentBlockStop
    ///   ContentBlockStart(Html) + HtmlSourceDelta + ContentBlockStop
    #[test]
    fn stream_emits_html_blocks_for_canvas_fence() {
        let mut acc = Accumulator::default();
        acc.consume(AnthropicEvent::MessageStart {
            message: SseMessage {
                id: "m-html".into(),
                model: "claude-x".into(),
                usage: SseInitialUsage::default(),
            },
        });
        // The upstream sends ONE text block whose deltas contain text +
        // fenced HTML + closer + trailing text. We assert the SPP output
        // splits this into separate Text / Html blocks.
        acc.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: SseContentBlock::Text {
                text: String::new(),
            },
        });
        let mut events = Vec::new();
        events.extend(acc.consume(AnthropicEvent::ContentBlockDelta {
            index: 0,
            delta: SseDelta::TextDelta {
                text: "Here:\n```html-canvas\n<p>hi</p>\n```\n".into(),
            },
        }));
        events.extend(acc.consume(AnthropicEvent::ContentBlockStop { index: 0 }));
        events.extend(acc.consume(AnthropicEvent::MessageDelta {
            delta: SseMessageDelta {
                stop_reason: Some("end_turn".into()),
                stop_sequence: None,
            },
            usage: SseUsageDelta {
                output_tokens: Some(5),
                ..Default::default()
            },
        }));
        events.extend(acc.consume(AnthropicEvent::MessageStop));

        let content_events: Vec<&StreamEvent> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    StreamEvent::ContentBlockStart { .. }
                        | StreamEvent::ContentBlockDelta { .. }
                        | StreamEvent::ContentBlockStop { .. }
                )
            })
            .collect();

        assert!(
            matches!(
                content_events[0],
                StreamEvent::ContentBlockStart {
                    block: ContentBlock::Text { text },
                    ..
                } if text.is_empty()
            ),
            "expected ContentBlockStart Text(\"\") first, got {:?}",
            content_events[0]
        );
        assert!(
            matches!(
                content_events[1],
                StreamEvent::ContentBlockDelta {
                    delta: BlockDelta::TextDelta { text },
                    ..
                } if text == "Here:\n"
            ),
            "expected TextDelta(\"Here:\\n\"), got {:?}",
            content_events[1]
        );
        assert!(
            matches!(content_events[2], StreamEvent::ContentBlockStop { .. }),
            "expected ContentBlockStop after text, got {:?}",
            content_events[2]
        );
        assert!(
            matches!(
                content_events[3],
                StreamEvent::ContentBlockStart {
                    block: ContentBlock::Html { source, .. },
                    ..
                } if source.is_empty()
            ),
            "expected ContentBlockStart Html(\"\"), got {:?}",
            content_events[3]
        );
        assert!(
            matches!(
                content_events[4],
                StreamEvent::ContentBlockDelta {
                    delta: BlockDelta::HtmlSourceDelta { source },
                    ..
                } if source == "<p>hi</p>\n"
            ),
            "expected HtmlSourceDelta(\"<p>hi</p>\\n\"), got {:?}",
            content_events[4]
        );
        assert!(
            matches!(content_events[5], StreamEvent::ContentBlockStop { .. }),
            "expected ContentBlockStop after html, got {:?}",
            content_events[5]
        );

        let out = acc.finish().unwrap();
        assert_eq!(out.content.len(), 2);
        match &out.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Here:\n"),
            other => panic!("expected Text first, got {other:?}"),
        }
        match &out.content[1] {
            ContentBlock::Html { source, .. } => assert_eq!(source, "<p>hi</p>\n"),
            other => panic!("expected Html second, got {other:?}"),
        }
    }
}
