//! Gemini SSE → SPP [`StreamEvent`] adapter.
//!
//! Gemini's `:streamGenerateContent?alt=sse` endpoint emits a series of
//! `data:` JSON payloads. Each payload is an incremental
//! [`api::GenerateContentResponse`] whose `candidates[0].content.parts`
//! arrays accumulate as the model speaks. There is no per-part start/stop
//! framing — the provider drops a new `parts` array each chunk and we
//! diff it against what we've already seen to derive the SPP block-level
//! event vocabulary.
//!
//! Behaviour:
//!
//! - The first chunk synthesises a [`StreamEvent::MessageStart`] using the
//!   response id and `modelVersion` (or fallbacks).
//! - For each chunk, we walk its parts left-to-right. A part at an index
//!   we have not yet opened produces a
//!   [`StreamEvent::ContentBlockStart`]; subsequent text appended to the
//!   same index produces [`StreamEvent::ContentBlockDelta`]s. Function
//!   calls arrive whole, so they're emitted as a `ContentBlockStart` only.
//! - When the next chunk's parts move past an open block, that block is
//!   closed with a [`StreamEvent::ContentBlockStop`]. The final flush at
//!   stream end closes any still-open blocks.
//! - `finishReason` and `usageMetadata` from the last chunk feed a final
//!   [`StreamEvent::MessageDelta`] + [`StreamEvent::MessageStop`].

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use savvagent_fence::{FenceChunk, FenceParser};
use savvagent_mcp::StreamEmitter;
use savvagent_protocol::{self as spp, BlockDelta, ContentBlock, StreamEvent, Usage, UsageDelta};

use crate::api;
use crate::translate::{stop_reason_from_gemini, synthesize_tool_use_id};

/// Drive a Gemini SSE response to completion.
pub async fn consume_sse(
    resp: reqwest::Response,
    emit: &dyn StreamEmitter,
) -> Result<spp::CompleteResponse, spp::ProviderError> {
    let mut acc = Accumulator::default();
    let mut sse = SseDecoder::new(resp);

    while let Some(frame) = sse.next().await? {
        let chunk: api::GenerateContentResponse = match serde_json::from_str(&frame.data) {
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

        for ev in acc.consume_chunk(chunk) {
            let _ = emit.emit(ev).await;
        }
    }

    for ev in acc.flush() {
        let _ = emit.emit(ev).await;
    }

    acc.finish()
}

#[derive(Default)]
struct Accumulator {
    started: bool,
    id: Option<String>,
    model: Option<String>,
    usage: Usage,
    stop_reason: Option<spp::StopReason>,
    /// Gemini reports `finishReason="STOP"` for tool-call turns just like
    /// plain-text ones, so we track whether any `functionCall` part has been
    /// observed and override the stop reason to `ToolUse` ourselves.
    saw_function_call: bool,
    /// Open block state, indexed by SPP block index. `None` slots represent
    /// blocks already closed.
    blocks: Vec<Option<BlockState>>,
    tool_use_counter: u32,
    /// Buffered final-content blocks for the eventual `CompleteResponse`.
    final_blocks: Vec<ContentBlock>,
    /// Extracts ``` ```html-canvas ``` fences from streaming Text parts
    /// (Thinking parts bypass the parser because thinking content doesn't
    /// include user-visible html-canvas).
    fence_parser: FenceParser,
}

#[derive(Debug, Clone)]
enum BlockState {
    Text {
        buf: String,
    },
    /// HTML extracted from a ``` ```html-canvas ``` fence inside a Text part.
    Html {
        source: String,
    },
    Thinking {
        buf: String,
        signature: Option<String>,
    },
    /// Function calls arrive whole in Gemini's stream — recording the
    /// completed block here so we emit the right `ContentBlockStop` later.
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Image (inline data) — same: emitted whole.
    Image {
        source: spp::ImageSource,
    },
}

impl Accumulator {
    fn consume_chunk(&mut self, chunk: api::GenerateContentResponse) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        if !self.started {
            self.started = true;
            self.id = chunk
                .response_id
                .clone()
                .or_else(|| Some("gemini-response".into()));
            self.model = chunk.model_version.clone();
            if let Some(u) = &chunk.usage_metadata {
                self.usage.input_tokens = u.prompt_token_count;
                self.usage.cache_read_input_tokens = u.cached_content_token_count;
            }
            events.push(StreamEvent::MessageStart {
                id: self.id.clone().unwrap_or_default(),
                model: self.model.clone().unwrap_or_default(),
                usage: self.usage.clone(),
            });
        } else if let Some(u) = &chunk.usage_metadata {
            // Refresh prompt/cache totals if the provider re-states them.
            if u.prompt_token_count > self.usage.input_tokens {
                self.usage.input_tokens = u.prompt_token_count;
            }
            if u.cached_content_token_count.is_some() {
                self.usage.cache_read_input_tokens = u.cached_content_token_count;
            }
        }

        let candidate = chunk.candidates.into_iter().next();
        let (parts, finish_reason) = match candidate {
            Some(c) => (
                c.content.map(|c| c.parts).unwrap_or_default(),
                c.finish_reason,
            ),
            None => (Vec::new(), None),
        };

        for part in parts {
            self.consume_part(part, &mut events);
        }

        if let Some(reason) = finish_reason.as_deref() {
            self.stop_reason = Some(stop_reason_from_gemini(Some(reason)));
        }
        self.coerce_tool_use_stop_reason();

        if let Some(u) = chunk.usage_metadata {
            let new_out = u.candidates_token_count.unwrap_or(0);
            let delta_out = new_out.saturating_sub(self.usage.output_tokens);
            self.usage.output_tokens = new_out;
            if delta_out > 0 || self.stop_reason.is_some() {
                events.push(StreamEvent::MessageDelta {
                    stop_reason: self.stop_reason,
                    stop_sequence: None,
                    usage_delta: UsageDelta {
                        output_tokens: if delta_out > 0 { Some(delta_out) } else { None },
                        cache_read_input_tokens: u.cached_content_token_count,
                    },
                });
            }
        } else if self.stop_reason.is_some() && finish_reason.is_some() {
            events.push(StreamEvent::MessageDelta {
                stop_reason: self.stop_reason,
                stop_sequence: None,
                usage_delta: UsageDelta::default(),
            });
        }

        events
    }

    /// Process a single Gemini part, updating internal state and pushing the
    /// SPP events it produces into `out`.
    fn consume_part(&mut self, part: api::Part, out: &mut Vec<StreamEvent>) {
        // Function call: arrives as a whole part, never streamed in fragments.
        if let Some(fc) = part.function_call.clone() {
            self.saw_function_call = true;
            let idx = self.next_block_index();
            let id = synthesize_tool_use_id(&fc.name, self.tool_use_counter);
            self.tool_use_counter += 1;
            let block = ContentBlock::ToolUse {
                id: id.clone(),
                name: fc.name.clone(),
                input: fc.args.clone(),
            };
            out.push(StreamEvent::ContentBlockStart {
                index: idx as u32,
                block: block.clone(),
            });
            // Gemini sends the full args object atomically; the SPP wire
            // format models partial JSON with `input_json_delta` events,
            // but emitting a delta here would be redundant. Just close.
            out.push(StreamEvent::ContentBlockStop { index: idx as u32 });
            self.blocks.push(Some(BlockState::ToolUse {
                id,
                name: fc.name,
                input: fc.args,
            }));
            // Move the block straight to "closed".
            self.close_block(idx, out, /*already_emitted_stop=*/ true);
            return;
        }

        // Inline image data: also whole.
        if let Some(inline) = part.inline_data.clone() {
            let mt = match inline.mime_type.as_str() {
                "image/jpeg" => spp::MediaType::Jpeg,
                "image/gif" => spp::MediaType::Gif,
                "image/webp" => spp::MediaType::Webp,
                _ => spp::MediaType::Png,
            };
            let source = spp::ImageSource::Base64 {
                media_type: mt,
                data: inline.data,
            };
            let idx = self.next_block_index();
            out.push(StreamEvent::ContentBlockStart {
                index: idx as u32,
                block: ContentBlock::Image {
                    source: source.clone(),
                },
            });
            out.push(StreamEvent::ContentBlockStop { index: idx as u32 });
            self.blocks.push(Some(BlockState::Image { source }));
            self.close_block(idx, out, /*already_emitted_stop=*/ true);
            return;
        }

        // Text / thinking parts: append to the open text-or-thinking block,
        // or open a new one if none exists or the previous block was a
        // different kind.
        let Some(text) = part.text else {
            return;
        };
        let is_thinking = matches!(part.thought, Some(true));
        let signature = part.thought_signature;

        if is_thinking {
            self.emit_thinking_fragment(text, signature, out);
        } else {
            // Route through the fence parser so html-canvas blocks
            // become their own SPP Html block, not text-disguised HTML.
            let chunks = self.fence_parser.push(&text);
            for chunk in chunks {
                match chunk {
                    FenceChunk::Text(t) => self.emit_text_chunk(t, out),
                    FenceChunk::Html(t) => self.emit_html_chunk(t, out),
                }
            }
        }
    }

    /// Append `text` to the currently-open Text block, or open a new
    /// one if the last block is a different kind (or none is open).
    fn emit_text_chunk(&mut self, text: String, out: &mut Vec<StreamEvent>) {
        match self.last_open_block_mut() {
            Some((idx, BlockState::Text { buf })) => {
                buf.push_str(&text);
                out.push(StreamEvent::ContentBlockDelta {
                    index: idx as u32,
                    delta: BlockDelta::TextDelta { text },
                });
            }
            _ => {
                self.close_last_open(out);
                let idx = self.next_block_index();
                out.push(StreamEvent::ContentBlockStart {
                    index: idx as u32,
                    block: ContentBlock::Text {
                        text: String::new(),
                    },
                });
                if !text.is_empty() {
                    out.push(StreamEvent::ContentBlockDelta {
                        index: idx as u32,
                        delta: BlockDelta::TextDelta { text: text.clone() },
                    });
                }
                self.blocks.push(Some(BlockState::Text { buf: text }));
            }
        }
    }

    /// Append `html` to the currently-open Html block, or open a new
    /// one if the last block is a different kind (or none is open).
    fn emit_html_chunk(&mut self, html: String, out: &mut Vec<StreamEvent>) {
        match self.last_open_block_mut() {
            Some((idx, BlockState::Html { source })) => {
                source.push_str(&html);
                out.push(StreamEvent::ContentBlockDelta {
                    index: idx as u32,
                    delta: BlockDelta::HtmlSourceDelta { source: html },
                });
            }
            _ => {
                self.close_last_open(out);
                let idx = self.next_block_index();
                out.push(StreamEvent::ContentBlockStart {
                    index: idx as u32,
                    block: ContentBlock::Html {
                        source: String::new(),
                        state: None,
                    },
                });
                if !html.is_empty() {
                    out.push(StreamEvent::ContentBlockDelta {
                        index: idx as u32,
                        delta: BlockDelta::HtmlSourceDelta {
                            source: html.clone(),
                        },
                    });
                }
                self.blocks.push(Some(BlockState::Html { source: html }));
            }
        }
    }

    /// Append a `thought: true` part to the open Thinking block, or open
    /// a new one if the last block is a different kind.
    fn emit_thinking_fragment(
        &mut self,
        text: String,
        signature: Option<String>,
        out: &mut Vec<StreamEvent>,
    ) {
        match self.last_open_block_mut() {
            Some((
                idx,
                BlockState::Thinking {
                    buf,
                    signature: sig,
                },
            )) => {
                buf.push_str(&text);
                if let Some(new_sig) = signature {
                    *sig = Some(new_sig.clone());
                    out.push(StreamEvent::ContentBlockDelta {
                        index: idx as u32,
                        delta: BlockDelta::SignatureDelta { signature: new_sig },
                    });
                }
                out.push(StreamEvent::ContentBlockDelta {
                    index: idx as u32,
                    delta: BlockDelta::ThinkingDelta { text },
                });
            }
            _ => {
                self.close_last_open(out);
                let idx = self.next_block_index();
                out.push(StreamEvent::ContentBlockStart {
                    index: idx as u32,
                    block: ContentBlock::Thinking {
                        text: String::new(),
                        signature: signature.clone(),
                    },
                });
                if !text.is_empty() {
                    out.push(StreamEvent::ContentBlockDelta {
                        index: idx as u32,
                        delta: BlockDelta::ThinkingDelta { text: text.clone() },
                    });
                }
                self.blocks.push(Some(BlockState::Thinking {
                    buf: text,
                    signature,
                }));
            }
        }
    }

    fn last_open_block_mut(&mut self) -> Option<(usize, &mut BlockState)> {
        // Reverse-scan to find the most recently opened block; if it's
        // not a streamable kind (ToolUse/Image), we've already closed it.
        for (i, slot) in self.blocks.iter_mut().enumerate().rev() {
            if let Some(state) = slot {
                match state {
                    BlockState::Text { .. }
                    | BlockState::Html { .. }
                    | BlockState::Thinking { .. } => {
                        return Some((i, state));
                    }
                    BlockState::ToolUse { .. } | BlockState::Image { .. } => return None,
                }
            }
        }
        None
    }

    fn close_last_open(&mut self, out: &mut Vec<StreamEvent>) {
        for i in (0..self.blocks.len()).rev() {
            if self.blocks[i].is_some() {
                self.close_block(i, out, /*already_emitted_stop=*/ false);
                return;
            }
        }
    }

    fn close_block(&mut self, idx: usize, out: &mut Vec<StreamEvent>, already_emitted_stop: bool) {
        let Some(state) = self.blocks[idx].take() else {
            return;
        };
        if !already_emitted_stop {
            out.push(StreamEvent::ContentBlockStop { index: idx as u32 });
        }
        let block = match state {
            BlockState::Text { buf } => ContentBlock::Text { text: buf },
            BlockState::Html { source } => ContentBlock::Html {
                source,
                state: None,
            },
            BlockState::Thinking { buf, signature } => ContentBlock::Thinking {
                text: buf,
                signature,
            },
            BlockState::ToolUse { id, name, input } => ContentBlock::ToolUse { id, name, input },
            BlockState::Image { source } => ContentBlock::Image { source },
        };
        self.final_blocks.push(block);
    }

    fn next_block_index(&self) -> usize {
        self.blocks.len()
    }

    fn flush(&mut self) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        // Drain any buffered html-canvas fence content first so we
        // don't leave half-flushed text/html behind the open block.
        let parser = std::mem::take(&mut self.fence_parser);
        let finish = parser.finish();
        for chunk in finish.chunks {
            match chunk {
                FenceChunk::Text(t) => self.emit_text_chunk(t, &mut events),
                FenceChunk::Html(t) => self.emit_html_chunk(t, &mut events),
            }
        }
        if finish.unclosed_fence {
            tracing::warn!("gemini stream ended with unclosed html-canvas fence");
        }

        let len = self.blocks.len();
        for i in 0..len {
            if self.blocks[i].is_some() {
                self.close_block(i, &mut events, /*already_emitted_stop=*/ false);
            }
        }
        if self.stop_reason.is_none() {
            self.stop_reason = Some(spp::StopReason::EndTurn);
        }
        self.coerce_tool_use_stop_reason();
        events.push(StreamEvent::MessageStop);
        events
    }

    fn finish(self) -> Result<spp::CompleteResponse, spp::ProviderError> {
        if !self.started {
            return Err(stream_decode_error("stream produced no chunks"));
        }
        Ok(spp::CompleteResponse {
            id: self.id.unwrap_or_default(),
            model: self.model.unwrap_or_default(),
            content: self.final_blocks,
            stop_reason: self.stop_reason.unwrap_or(spp::StopReason::EndTurn),
            stop_sequence: None,
            usage: self.usage,
        })
    }

    /// Tool-use blocks are authoritative regardless of the upstream
    /// `finishReason`. Gemini emits `"STOP"` for tool-call turns just like
    /// plain-text ones, and may even emit `"SAFETY"` /
    /// `"MALFORMED_FUNCTION_CALL"` alongside a `functionCall` part. Once any
    /// function_call has been observed in this stream, force the stop
    /// reason to `ToolUse` — and warn loudly when overriding away from a
    /// `Refusal`/`Other` so the original signal isn't silently lost.
    fn coerce_tool_use_stop_reason(&mut self) {
        if !self.saw_function_call || matches!(self.stop_reason, Some(spp::StopReason::ToolUse)) {
            return;
        }
        if matches!(
            self.stop_reason,
            Some(spp::StopReason::Refusal) | Some(spp::StopReason::Other)
        ) {
            tracing::warn!(
                original_stop_reason = ?self.stop_reason,
                "gemini emitted a functionCall alongside a refusal/other finishReason; overriding stop_reason to ToolUse"
            );
        }
        self.stop_reason = Some(spp::StopReason::ToolUse);
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

// ---- Tiny SSE byte-stream decoder (Gemini emits plain `data:` lines) ----

struct SseDecoder {
    inner: futures::stream::BoxStream<'static, reqwest::Result<Bytes>>,
    buf: BytesMut,
}

#[derive(Debug)]
struct SseFrame {
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
        let mut data_lines = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
        }
        if data_lines.is_empty() {
            return self.try_pop_frame();
        }
        Some(SseFrame {
            data: data_lines.join("\n"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chunk(v: serde_json::Value) -> api::GenerateContentResponse {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn accumulator_assembles_text_across_chunks() {
        let mut acc = Accumulator::default();
        let _ = acc.consume_chunk(chunk(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "hello"}]},
                "index": 0
            }],
            "responseId": "r1",
            "modelVersion": "gemini-x",
            "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 1}
        })));
        let _ = acc.consume_chunk(chunk(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": " world"}]},
                "finishReason": "STOP",
                "index": 0
            }],
            "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 4}
        })));
        let _ = acc.flush();
        let out = acc.finish().unwrap();
        assert_eq!(out.id, "r1");
        assert_eq!(out.usage.output_tokens, 4);
        assert_eq!(out.stop_reason, spp::StopReason::EndTurn);
        match &out.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello world"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn accumulator_emits_function_call_atomically() {
        let mut acc = Accumulator::default();
        let evs = acc.consume_chunk(chunk(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"functionCall": {"name": "ls", "args": {"path": "/tmp"}}}
                ]},
                "finishReason": "STOP",
                "index": 0
            }],
            "responseId": "r2",
            "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 3}
        })));
        // We expect message_start + content_block_start + content_block_stop +
        // message_delta in some order. Find the ToolUse block.
        let has_tool_use_start = evs.iter().any(|e| {
            matches!(
                e,
                StreamEvent::ContentBlockStart {
                    block: ContentBlock::ToolUse { name, .. },
                    ..
                } if name == "ls"
            )
        });
        assert!(has_tool_use_start, "missing tool_use start: {evs:#?}");
        let _ = acc.flush();
        let out = acc.finish().unwrap();
        assert_eq!(out.stop_reason, spp::StopReason::ToolUse);
        match &out.content[0] {
            ContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "ls");
                assert_eq!(input["path"], "/tmp");
            }
            _ => panic!("expected tool_use"),
        }
    }

    /// Gemini occasionally splits a tool-call turn across chunks: a chunk
    /// carrying just the `functionCall` part, followed by a chunk that
    /// carries only `finishReason="STOP"`. The accumulator must still end
    /// with `stop_reason=ToolUse` — the override has to re-evaluate on every
    /// chunk, not just the one that flips `saw_function_call`.
    #[test]
    fn accumulator_function_call_split_across_chunks_yields_tool_use_stop() {
        let mut acc = Accumulator::default();
        let _ = acc.consume_chunk(chunk(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"functionCall": {"name": "ls", "args": {"path": "/tmp"}}}
                ]},
                "index": 0
            }],
            "responseId": "r_split"
        })));
        let _ = acc.consume_chunk(chunk(json!({
            "candidates": [{
                "content": {"role": "model", "parts": []},
                "finishReason": "STOP",
                "index": 0
            }],
            "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 3}
        })));
        let _ = acc.flush();
        let out = acc.finish().unwrap();
        assert_eq!(out.stop_reason, spp::StopReason::ToolUse);
        match &out.content[0] {
            ContentBlock::ToolUse { name, .. } => assert_eq!(name, "ls"),
            _ => panic!("expected tool_use"),
        }
    }

    /// The other split-chunk ordering: `finishReason="STOP"` arrives before
    /// any function_call part. The override must fire when the later chunk
    /// finally emits the function_call, not just when the finishReason
    /// chunk is processed.
    #[test]
    fn accumulator_stop_before_function_call_still_yields_tool_use() {
        let mut acc = Accumulator::default();
        let _ = acc.consume_chunk(chunk(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "calling tool"}
                ]},
                "finishReason": "STOP",
                "index": 0
            }],
            "responseId": "r_reverse"
        })));
        let _ = acc.consume_chunk(chunk(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"functionCall": {"name": "ls", "args": {}}}
                ]},
                "index": 0
            }],
            "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 3}
        })));
        let _ = acc.flush();
        let out = acc.finish().unwrap();
        assert_eq!(out.stop_reason, spp::StopReason::ToolUse);
    }

    #[test]
    fn accumulator_handles_thinking_then_text() {
        let mut acc = Accumulator::default();
        let _ = acc.consume_chunk(chunk(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "let me think", "thought": true}
                ]},
                "index": 0
            }],
            "responseId": "r3"
        })));
        let _ = acc.consume_chunk(chunk(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "answer is 42"}
                ]},
                "finishReason": "STOP",
                "index": 0
            }],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 5}
        })));
        let _ = acc.flush();
        let out = acc.finish().unwrap();
        assert_eq!(out.content.len(), 2);
        assert!(
            matches!(&out.content[0], ContentBlock::Thinking { text, .. } if text == "let me think")
        );
        assert!(matches!(&out.content[1], ContentBlock::Text { text } if text == "answer is 42"));
    }

    /// When the model emits text followed by an `html-canvas` fenced HTML
    /// block, the SPP stream must split into a Text block and a separate
    /// Html block: ContentBlockStart(Text) + TextDelta + ContentBlockStop
    /// followed by ContentBlockStart(Html) + HtmlSourceDelta +
    /// ContentBlockStop. The fenced markers themselves are stripped.
    #[test]
    fn stream_emits_html_blocks_for_canvas_fence() {
        let mut acc = Accumulator::default();
        let evs = acc.consume_chunk(chunk(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "Here:\n```html-canvas\n<p>hi</p>\n```\n"}
                ]},
                "finishReason": "STOP",
                "index": 0
            }],
            "responseId": "r-html",
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 5}
        })));
        let flush_evs = acc.flush();
        let stream: Vec<StreamEvent> = evs.into_iter().chain(flush_evs).collect();

        let content_events: Vec<&StreamEvent> = stream
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
