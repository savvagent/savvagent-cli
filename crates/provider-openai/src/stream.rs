//! OpenAI SSE → SPP [`StreamEvent`] adapter.
//!
//! OpenAI's Chat Completions streaming endpoint emits `data:` SSE lines whose
//! payloads are JSON `ChatCompletionChunk` objects. The stream ends with the
//! sentinel line `data: [DONE]`.
//!
//! The translation strategy mirrors the Anthropic adapter:
//!
//! - The first chunk synthesises a [`StreamEvent::MessageStart`].
//! - Text deltas in `choices[0].delta.content` map to `ContentBlockDelta`.
//! - Tool-call deltas in `choices[0].delta.tool_calls` map to either
//!   `ContentBlockStart` (first occurrence of an index) + `InputJsonDelta`
//!   fragments (subsequent `arguments` fragments), or `ContentBlockStop` once
//!   all arguments have been received (at `finish_reason = "tool_calls"`).
//! - The `[DONE]` sentinel triggers a `MessageDelta` + `MessageStop`.
//! - When `stream_options.include_usage = true`, the final chunk before
//!   `[DONE]` carries `usage`; we emit a final `MessageDelta` with that.

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use savvagent_fence::{FenceChunk, FenceParser};
use savvagent_mcp::{EmitError, StreamEmitter};
use savvagent_protocol::{self as spp, BlockDelta, ContentBlock, StreamEvent, Usage, UsageDelta};

use crate::api;
use crate::translate::{parse_tool_arguments, stop_reason_from_str, usage_from_openai};

/// Drive an OpenAI SSE streaming response to completion.
///
/// If the consumer disconnects (i.e. [`StreamEmitter::emit`] returns
/// [`EmitError::Disconnected`]) we abandon the call rather than continue
/// pulling chunks from upstream and burning tokens. Transport-level emit
/// errors are tolerated — those are typically transient hiccups in the MCP
/// progress channel and the caller will still get the final structured
/// response.
pub async fn consume_sse(
    resp: reqwest::Response,
    emit: &dyn StreamEmitter,
) -> Result<spp::CompleteResponse, spp::ProviderError> {
    let mut acc = Accumulator::default();
    let mut sse = SseDecoder::new(resp);

    while let SseItem::Chunk(chunk) = sse.next().await? {
        for ev in acc.consume_chunk(chunk) {
            if let Err(EmitError::Disconnected) = emit.emit(ev).await {
                return Err(consumer_disconnected());
            }
        }
    }

    for ev in acc.flush() {
        if let Err(EmitError::Disconnected) = emit.emit(ev).await {
            return Err(consumer_disconnected());
        }
    }

    acc.finish()
}

fn consumer_disconnected() -> spp::ProviderError {
    spp::ProviderError {
        kind: spp::ErrorKind::Internal,
        message: "stream consumer disconnected".into(),
        retry_after_ms: None,
        provider_code: None,
    }
}

#[derive(Default)]
struct Accumulator {
    started: bool,
    id: Option<String>,
    model: Option<String>,
    usage: Usage,
    stop_reason: Option<spp::StopReason>,
    /// Per-block accumulator state indexed by SPP block index.
    blocks: Vec<BlockState>,
    /// Next SPP block index to assign.
    next_block: u32,
    /// The currently-open streaming Text or Html block, if any. We close
    /// and reopen across kind switches so each html-canvas fence becomes
    /// its own SPP block.
    current_stream_block: Option<CurrentStreamBlock>,
    /// Per-OpenAI-tool-call-index → SPP block index mapping.
    tool_block_map: Vec<Option<u32>>,
    /// Extracts ``` ```html-canvas ``` fences from `delta.content`.
    fence_parser: FenceParser,
}

#[derive(Debug)]
enum BlockState {
    Text {
        buf: String,
    },
    Html {
        source: String,
    },
    ToolUse {
        id: String,
        name: String,
        json_buf: String,
    },
}

/// Pointer to the streaming block accumulator currently open.
#[derive(Debug)]
struct CurrentStreamBlock {
    index: u32,
    kind: StreamBlockKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamBlockKind {
    Text,
    Html,
}

impl Accumulator {
    fn consume_chunk(&mut self, chunk: api::ChatCompletionChunk) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        if !self.started {
            self.started = true;
            self.id = Some(chunk.id.clone());
            self.model = Some(chunk.model.clone());
            events.push(StreamEvent::MessageStart {
                id: chunk.id.clone(),
                model: chunk.model.clone(),
                usage: self.usage.clone(),
            });
        }

        // Capture usage from a chunk (typically the last one when
        // `include_usage = true`).
        if let Some(u) = chunk.usage {
            let spp_usage = usage_from_openai(u);
            self.usage = spp_usage;
        }

        let choice = chunk.choices.into_iter().next();
        let Some(choice) = choice else {
            return events;
        };

        if let Some(reason) = choice.finish_reason.as_deref() {
            self.stop_reason = Some(stop_reason_from_str(Some(reason)));
        }

        let delta = choice.delta;

        // Text delta — route through the fence parser so html-canvas
        // fenced HTML is extracted into its own ContentBlock::Html block.
        if let Some(text) = delta.content {
            if !text.is_empty() {
                let fence_chunks = self.fence_parser.push(&text);
                for chunk in fence_chunks {
                    self.emit_fence_chunk(chunk, &mut events);
                }
            }
        }

        // Tool-call deltas.
        for tc in delta.tool_calls {
            let oi = tc.index as usize;
            // Grow the map to cover this index.
            while self.tool_block_map.len() <= oi {
                self.tool_block_map.push(None);
            }

            if self.tool_block_map[oi].is_none() {
                // First delta for this tool-call: allocate an SPP block.
                let id = tc.id.unwrap_or_default();
                let name = tc
                    .function
                    .as_ref()
                    .and_then(|f| f.name.clone())
                    .unwrap_or_default();
                let block_idx = self.alloc_block(BlockState::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    json_buf: String::new(),
                });
                self.tool_block_map[oi] = Some(block_idx);
                events.push(StreamEvent::ContentBlockStart {
                    index: block_idx,
                    block: ContentBlock::ToolUse {
                        id,
                        name,
                        input: serde_json::json!({}),
                    },
                });
            }

            let block_idx = self.tool_block_map[oi].expect("just inserted");
            if let Some(func) = tc.function {
                if let Some(args_frag) = func.arguments {
                    if !args_frag.is_empty() {
                        if let Some(BlockState::ToolUse { json_buf, .. }) =
                            self.blocks.get_mut(block_idx as usize)
                        {
                            json_buf.push_str(&args_frag);
                        }
                        events.push(StreamEvent::ContentBlockDelta {
                            index: block_idx,
                            delta: BlockDelta::InputJsonDelta {
                                partial_json: args_frag,
                            },
                        });
                    }
                }
            }
        }

        events
    }

    fn flush(&mut self) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        // Drain any buffered text/html from the fence parser before
        // closing the currently-open streaming block.
        let parser = std::mem::take(&mut self.fence_parser);
        let finish = parser.finish();
        for chunk in finish.chunks {
            self.emit_fence_chunk(chunk, &mut events);
        }
        if finish.unclosed_fence {
            tracing::warn!("openai stream ended with unclosed html-canvas fence");
        }

        // Track which block index (if any) the current stream block had
        // *before* we close it, so we don't emit a duplicate Stop for it.
        let stream_idx = self.current_stream_block.as_ref().map(|c| c.index);
        // Close the currently-open streaming block, if any.
        self.close_stream_block(&mut events);

        // Close every tool-use block. (Tool-use blocks don't carry their
        // own Stop event mid-stream — OpenAI streams them as a series of
        // arguments fragments and we emit the Stop only at end-of-stream.
        // Streaming text/html blocks are already closed above.)
        for (i, block) in self.blocks.iter().enumerate() {
            let idx = i as u32;
            if Some(idx) == stream_idx {
                continue;
            }
            if matches!(block, BlockState::ToolUse { .. }) {
                events.push(StreamEvent::ContentBlockStop { index: idx });
            }
        }

        if self.stop_reason.is_none() {
            self.stop_reason = Some(spp::StopReason::EndTurn);
        }
        events.push(StreamEvent::MessageDelta {
            stop_reason: self.stop_reason,
            stop_sequence: None,
            usage_delta: UsageDelta {
                output_tokens: Some(self.usage.output_tokens),
                cache_read_input_tokens: None,
            },
        });
        events.push(StreamEvent::MessageStop);
        events
    }

    fn finish(self) -> Result<spp::CompleteResponse, spp::ProviderError> {
        if !self.started {
            return Err(stream_decode_error("stream produced no chunks"));
        }
        let mut content = Vec::new();
        for block in self.blocks {
            match block {
                BlockState::Text { buf } => {
                    content.push(ContentBlock::Text { text: buf });
                }
                BlockState::Html { source } => {
                    content.push(ContentBlock::Html {
                        source,
                        state: None,
                    });
                }
                BlockState::ToolUse { id, name, json_buf } => {
                    let input = parse_tool_arguments(&json_buf);
                    content.push(ContentBlock::ToolUse { id, name, input });
                }
            }
        }
        Ok(spp::CompleteResponse {
            id: self.id.unwrap_or_default(),
            model: self.model.unwrap_or_default(),
            content,
            stop_reason: self.stop_reason.unwrap_or(spp::StopReason::EndTurn),
            stop_sequence: None,
            usage: self.usage,
        })
    }

    fn alloc_block(&mut self, state: BlockState) -> u32 {
        let idx = self.next_block;
        self.blocks.push(state);
        self.next_block += 1;
        idx
    }

    /// Dispatch a single fence-parsed chunk: open or extend the matching
    /// streaming block, closing-and-reopening when the chunk kind differs
    /// from the currently-open block.
    fn emit_fence_chunk(&mut self, chunk: FenceChunk, out: &mut Vec<StreamEvent>) {
        match chunk {
            FenceChunk::Text(text) => self.emit_stream_chunk(StreamBlockKind::Text, text, out),
            FenceChunk::Html(source) => self.emit_stream_chunk(StreamBlockKind::Html, source, out),
        }
    }

    fn emit_stream_chunk(
        &mut self,
        kind: StreamBlockKind,
        text: String,
        out: &mut Vec<StreamEvent>,
    ) {
        // If a streaming block of a different kind is open, close it.
        if let Some(cur) = &self.current_stream_block {
            if cur.kind != kind {
                self.close_stream_block(out);
            }
        }

        // Lazy-open the streaming block if none is currently open.
        if self.current_stream_block.is_none() {
            let block_state = match kind {
                StreamBlockKind::Text => BlockState::Text { buf: String::new() },
                StreamBlockKind::Html => BlockState::Html {
                    source: String::new(),
                },
            };
            let idx = self.alloc_block(block_state);
            let start_block = match kind {
                StreamBlockKind::Text => ContentBlock::Text {
                    text: String::new(),
                },
                StreamBlockKind::Html => ContentBlock::Html {
                    source: String::new(),
                    state: None,
                },
            };
            out.push(StreamEvent::ContentBlockStart {
                index: idx,
                block: start_block,
            });
            self.current_stream_block = Some(CurrentStreamBlock { index: idx, kind });
        }

        let cur = self
            .current_stream_block
            .as_ref()
            .expect("just opened above");
        let idx = cur.index;
        // Append to the persistent block state.
        match (kind, self.blocks.get_mut(idx as usize)) {
            (StreamBlockKind::Text, Some(BlockState::Text { buf })) => buf.push_str(&text),
            (StreamBlockKind::Html, Some(BlockState::Html { source })) => source.push_str(&text),
            _ => {}
        }
        let delta = match kind {
            StreamBlockKind::Text => BlockDelta::TextDelta { text },
            StreamBlockKind::Html => BlockDelta::HtmlSourceDelta { source: text },
        };
        out.push(StreamEvent::ContentBlockDelta { index: idx, delta });
    }

    fn close_stream_block(&mut self, out: &mut Vec<StreamEvent>) {
        if let Some(cur) = self.current_stream_block.take() {
            out.push(StreamEvent::ContentBlockStop { index: cur.index });
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

// ---- SSE decoder ----

#[derive(Debug)]
enum SseItem {
    Chunk(api::ChatCompletionChunk),
    Done,
}

struct SseDecoder {
    inner: futures::stream::BoxStream<'static, reqwest::Result<Bytes>>,
    buf: BytesMut,
}

impl SseDecoder {
    fn new(resp: reqwest::Response) -> Self {
        Self {
            inner: resp.bytes_stream().boxed(),
            buf: BytesMut::with_capacity(8 * 1024),
        }
    }

    /// Build a decoder from a raw byte-chunk stream. For tests where we don't
    /// want to spin up an HTTP server just to feed bytes into the decoder.
    #[cfg(test)]
    fn from_stream(s: futures::stream::BoxStream<'static, reqwest::Result<Bytes>>) -> Self {
        Self {
            inner: s,
            buf: BytesMut::with_capacity(8 * 1024),
        }
    }

    async fn next(&mut self) -> Result<SseItem, spp::ProviderError> {
        loop {
            if let Some(item) = self.try_pop()? {
                return Ok(item);
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
                None => {
                    // Stream ended. If we still have buffered bytes that
                    // didn't form a complete `\n\n`-terminated frame, the
                    // upstream truncated mid-frame — surface that as a
                    // network error rather than silently report success.
                    if !self.buf.is_empty() {
                        return Err(spp::ProviderError {
                            kind: spp::ErrorKind::Network,
                            message: "stream truncated mid-frame".into(),
                            retry_after_ms: None,
                            provider_code: None,
                        });
                    }
                    return Ok(SseItem::Done);
                }
            }
        }
    }

    fn try_pop(&mut self) -> Result<Option<SseItem>, spp::ProviderError> {
        let end = {
            let bytes = &self.buf[..];
            let len = bytes.len();
            let mut i = 0;
            let mut sep = None;
            while i + 1 < len {
                if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
                    sep = Some(i + 2);
                    break;
                }
                if i + 3 < len
                    && bytes[i] == b'\r'
                    && bytes[i + 1] == b'\n'
                    && bytes[i + 2] == b'\r'
                    && bytes[i + 3] == b'\n'
                {
                    sep = Some(i + 4);
                    break;
                }
                i += 1;
            }
            match sep {
                Some(s) => s,
                None => return Ok(None),
            }
        };

        let frame_bytes = self.buf.split_to(end);
        let text = match std::str::from_utf8(&frame_bytes) {
            Ok(t) => t,
            Err(_) => return Ok(None),
        };

        let mut data_lines: Vec<&str> = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
            }
        }

        if data_lines.is_empty() {
            return self.try_pop();
        }

        let data = data_lines.join("");
        if data.trim() == "[DONE]" {
            return Ok(Some(SseItem::Done));
        }

        let chunk: api::ChatCompletionChunk = match serde_json::from_str(&data) {
            Ok(c) => c,
            Err(_) => {
                // Silently skip unparseable frames (e.g. ping lines).
                return self.try_pop();
            }
        };
        Ok(Some(SseItem::Chunk(chunk)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chunk(v: serde_json::Value) -> api::ChatCompletionChunk {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn accumulator_assembles_text_across_chunks() {
        let mut acc = Accumulator::default();
        let _ = acc.consume_chunk(chunk(json!({
            "id": "c1",
            "model": "gpt-4o-mini",
            "choices": [{"delta": {"content": "hel"}, "finish_reason": null}]
        })));
        let _ = acc.consume_chunk(chunk(json!({
            "id": "c1",
            "model": "gpt-4o-mini",
            "choices": [{"delta": {"content": "lo"}, "finish_reason": null}]
        })));
        let _ = acc.consume_chunk(chunk(json!({
            "id": "c1",
            "model": "gpt-4o-mini",
            "choices": [{"delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
        })));
        let _ = acc.flush();
        let out = acc.finish().unwrap();
        assert_eq!(out.id, "c1");
        assert_eq!(out.stop_reason, spp::StopReason::EndTurn);
        assert_eq!(out.usage.output_tokens, 3);
        match &out.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn accumulator_assembles_tool_call() {
        let mut acc = Accumulator::default();
        // First chunk: tool call opens with id + name
        let _ = acc.consume_chunk(chunk(json!({
            "id": "c2",
            "model": "gpt-4o",
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "type": "function",
                        "function": {"name": "ls", "arguments": ""}
                    }]
                },
                "finish_reason": null
            }]
        })));
        // Second chunk: arguments fragment
        let _ = acc.consume_chunk(chunk(json!({
            "id": "c2",
            "model": "gpt-4o",
            "choices": [{
                "delta": {
                    "tool_calls": [{"index": 0, "function": {"arguments": "{\"path\":"}}]
                },
                "finish_reason": null
            }]
        })));
        // Third chunk: finish the arguments
        let _ = acc.consume_chunk(chunk(json!({
            "id": "c2",
            "model": "gpt-4o",
            "choices": [{
                "delta": {
                    "tool_calls": [{"index": 0, "function": {"arguments": "\"/tmp\"}"}}]
                },
                "finish_reason": "tool_calls"
            }]
        })));
        let _ = acc.flush();
        let out = acc.finish().unwrap();
        assert_eq!(out.stop_reason, spp::StopReason::ToolUse);
        match &out.content[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_abc");
                assert_eq!(name, "ls");
                assert_eq!(input["path"], "/tmp");
            }
            _ => panic!("expected tool_use, got {:?}", out.content[0]),
        }
    }

    #[test]
    fn accumulator_handles_empty_stream() {
        let acc = Accumulator::default();
        let result = acc.finish();
        assert!(result.is_err(), "empty stream must return an error");
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
            "id": "c-html",
            "model": "gpt-4o-mini",
            "choices": [{
                "delta": {
                    "content": "Here:\n```html-canvas\n<p>hi</p>\n```\n"
                },
                "finish_reason": null
            }]
        })));
        let _ = acc.consume_chunk(chunk(json!({
            "id": "c-html",
            "model": "gpt-4o-mini",
            "choices": [{"delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
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

    /// SSE byte streams that end without a terminating `\n\n` for the final
    /// frame must surface as a `Network` error, not silently report `Done`
    /// (which previously masked truncation and lost the partial chunk).
    #[tokio::test]
    async fn sse_decoder_errors_on_truncated_trailing_frame() {
        // Valid first frame, then a partial second frame missing `\n\n`.
        let bytes = bytes::Bytes::from_static(
            b"data: {\"id\":\"c1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"c1\",\"model\":\"gpt-4o-mini\""
        );
        let s = futures::stream::iter(vec![Ok::<_, reqwest::Error>(bytes)]).boxed();
        let mut dec = SseDecoder::from_stream(s);

        // First call yields the well-formed chunk.
        let first = dec.next().await.expect("first chunk");
        assert!(matches!(first, SseItem::Chunk(_)));

        // Second call sees buffered bytes with no terminator and the inner
        // stream exhausted — must error.
        let err = dec
            .next()
            .await
            .expect_err("truncated trailing frame must error");
        assert_eq!(err.kind, spp::ErrorKind::Network);
        assert!(
            err.message.contains("truncated"),
            "unexpected message: {}",
            err.message
        );
    }
}
