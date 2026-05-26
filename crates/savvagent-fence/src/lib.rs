//! Streaming parser that extracts `savvagent-canvas` HTML fences from
//! model text output and emits a sequence of [`FenceChunk::Text`] and
//! [`FenceChunk::Html`] chunks.
//!
//! Fence syntax: a line beginning with ```` ```html-canvas ```` opens
//! an HTML block; a line that is exactly ` ``` ` (three backticks)
//! closes it. Anything between the open and close (inclusive of
//! whitespace) becomes a single `Html` chunk. Other code fences
//! (e.g. ```` ```rust ````, ```` ```html ```` without the
//! `-canvas` suffix) are passed through as text.
//!
//! The parser is push-based so it works on streaming token deltas: feed
//! each text fragment with [`FenceParser::push`], get back a `Vec` of
//! chunks. Call [`FenceParser::finish`] at end-of-stream to flush any
//! buffered text or to surface an unclosed-fence warning.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(missing_debug_implementations)]
#![warn(missing_docs)]

/// One unit of parsed output from [`FenceParser::push`] / `finish`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceChunk {
    /// Plain text outside any html-canvas fence.
    Text(String),
    /// HTML content from inside a `html-canvas` fence (fence lines
    /// themselves are stripped).
    Html(String),
}

/// Outcome of [`FenceParser::finish`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishResult {
    /// Flushed chunks (any buffered text or unclosed-fence content).
    pub chunks: Vec<FenceChunk>,
    /// `true` if a fence was opened but never closed before EOF; the
    /// open content was flushed as `Html` regardless.
    pub unclosed_fence: bool,
}

/// Push-based fence parser.
#[derive(Debug, Default)]
pub struct FenceParser {
    /// Carry-over bytes from the previous push that didn't form a
    /// complete line yet.
    buf: String,
    /// True iff we are currently inside an open html-canvas fence.
    inside_canvas: bool,
}

impl FenceParser {
    /// Construct an empty parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of text; returns any complete chunks parsable so far.
    ///
    /// The parser walks line-by-line because fence markers are line-anchored.
    /// The trailing partial line (post-last-`\n` bytes) stays buffered only
    /// while it could still extend into a fence marker; if the partial tail
    /// has already diverged from any possible fence-marker shape, it's
    /// flushed eagerly so streaming UX isn't held until the next newline.
    pub fn push(&mut self, fragment: &str) -> Vec<FenceChunk> {
        let mut out = Vec::new();
        self.buf.push_str(fragment);

        // Walk line-by-line, but keep any trailing incomplete line in `buf`.
        while let Some(nl) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=nl).collect();
            self.consume_line(&line, &mut out);
        }

        // Eagerly flush any safe prefix of the partial-line tail so per-token
        // streaming doesn't stall until a newline. The tail must continue to
        // hold any bytes that could still extend into a fence marker.
        let tail = std::mem::take(&mut self.buf);
        let (flushable, hold) = split_safe_flush(&tail, self.inside_canvas);
        if !flushable.is_empty() {
            if self.inside_canvas {
                append_html(&mut out, flushable);
            } else {
                append_text(&mut out, flushable);
            }
        }
        self.buf.push_str(hold);

        out
    }

    /// End of stream — flush remaining buffered content.
    pub fn finish(mut self) -> FinishResult {
        let mut chunks = Vec::new();
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.consume_line(&line, &mut chunks);
        }
        FinishResult {
            chunks,
            unclosed_fence: self.inside_canvas,
        }
    }

    fn consume_line(&mut self, line: &str, out: &mut Vec<FenceChunk>) {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if !self.inside_canvas {
            if trimmed.trim_start() == "```html-canvas" {
                self.inside_canvas = true;
                return; // fence line consumed; no emission
            }
            append_text(out, line);
        } else {
            // TODO(canvas-phase2): handle embedded ``` inside HTML bodies
            // (e.g., <pre><code>``` </code></pre>). Current behavior treats
            // any line == "```" as the closing fence; the documented
            // limitation has a regression test in
            // `embedded_triple_backticks_close_fence_known_limitation`.
            if trimmed.trim_start() == "```" {
                self.inside_canvas = false;
                return; // closing fence consumed; no emission
            }
            append_html(out, line);
        }
    }
}

fn append_text(out: &mut Vec<FenceChunk>, s: &str) {
    if let Some(FenceChunk::Text(t)) = out.last_mut() {
        t.push_str(s);
    } else {
        out.push(FenceChunk::Text(s.to_string()));
    }
}

fn append_html(out: &mut Vec<FenceChunk>, s: &str) {
    if let Some(FenceChunk::Html(t)) = out.last_mut() {
        t.push_str(s);
    } else {
        out.push(FenceChunk::Html(s.to_string()));
    }
}

/// Given a partial-line tail (no `\n`), split it into a prefix that's
/// definitely-not-a-fence-marker and can be flushed immediately, and a
/// suffix that might still extend into a fence marker and must be held
/// for the next push.
///
/// The fence markers we care about are:
/// - Outside canvas: `` "```html-canvas" `` after optional leading whitespace.
/// - Inside canvas: `` "```" `` after optional leading whitespace.
///
/// As long as the *trimmed-start* of the tail is a prefix of the marker
/// shape, we hold; once the tail diverges (extra chars, wrong char), we
/// can flush everything safely as text/html.
fn split_safe_flush(tail: &str, inside_canvas: bool) -> (&str, &str) {
    // Anything is allowed to be preceded by horizontal whitespace; once the
    // first non-whitespace char appears, the tail commits to either being a
    // fence marker or not.
    let ws_len: usize = tail
        .chars()
        .take_while(|c| matches!(c, ' ' | '\t'))
        .map(|c| c.len_utf8())
        .sum();

    let body = &tail[ws_len..];
    let target = if inside_canvas {
        "```"
    } else {
        "```html-canvas"
    };

    // If the trimmed body is still a prefix of the marker (including the
    // empty case where we only have leading whitespace), keep buffering.
    // Otherwise, the partial line is committed to be content and is safe
    // to flush.
    if target.starts_with(body) {
        ("", tail)
    } else {
        (tail, "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_fence_is_pure_text() {
        let mut p = FenceParser::new();
        let chunks = p.push("hello\nworld\n");
        assert_eq!(chunks, vec![FenceChunk::Text("hello\nworld\n".into())]);
        let fin = p.finish();
        assert!(fin.chunks.is_empty());
        assert!(!fin.unclosed_fence);
    }

    #[test]
    fn complete_fence_extracts_html() {
        let mut p = FenceParser::new();
        let mut chunks = p.push("Here:\n");
        chunks.extend(p.push("```html-canvas\n"));
        chunks.extend(p.push("<!doctype html><body>x</body>\n"));
        chunks.extend(p.push("```\n"));
        chunks.extend(p.push("trailing\n"));

        assert_eq!(
            chunks,
            vec![
                FenceChunk::Text("Here:\n".into()),
                FenceChunk::Html("<!doctype html><body>x</body>\n".into()),
                FenceChunk::Text("trailing\n".into()),
            ],
        );
        let fin = p.finish();
        assert!(fin.chunks.is_empty());
        assert!(!fin.unclosed_fence);
    }

    #[test]
    fn other_code_fences_pass_through_as_text() {
        let mut p = FenceParser::new();
        let chunks = p.push("```rust\nfn x() {}\n```\n");
        assert_eq!(
            chunks,
            vec![FenceChunk::Text("```rust\nfn x() {}\n```\n".into())]
        );
    }

    #[test]
    fn plain_html_fence_is_not_canvas() {
        // ```html (no -canvas) must be treated as a code sample.
        let mut p = FenceParser::new();
        let chunks = p.push("```html\n<p>x</p>\n```\n");
        assert_eq!(
            chunks,
            vec![FenceChunk::Text("```html\n<p>x</p>\n```\n".into())]
        );
    }

    #[test]
    fn split_across_pushes() {
        let mut p = FenceParser::new();
        // First push: partial fence-opener prefix; nothing emitted yet —
        // the tail "```ht" could still grow into "```html-canvas".
        let chunks_1 = p.push("```ht");
        assert!(chunks_1.is_empty(), "partial opener must buffer");

        // Second push: opener completes on the `\n`, then "<b>" is
        // flushed eagerly because it can't extend into the closing fence.
        let chunks_2 = p.push("ml-canvas\n<b>");
        assert_eq!(chunks_2, vec![FenceChunk::Html("<b>".into())]);

        // Third push: rest of the html body + closing fence.
        let chunks_3 = p.push("hi</b>\n```\n");
        assert_eq!(chunks_3, vec![FenceChunk::Html("hi</b>\n".into())]);

        // The concatenation of all chunks reconstructs the html body in order.
        let mut all = chunks_1;
        all.extend(chunks_2);
        all.extend(chunks_3);
        let html: String = all
            .iter()
            .map(|c| match c {
                FenceChunk::Html(s) => s.as_str(),
                FenceChunk::Text(_) => "",
            })
            .collect();
        assert_eq!(html, "<b>hi</b>\n");
    }

    #[test]
    fn unclosed_fence_flushed_at_finish() {
        let mut p = FenceParser::new();
        // The html body "<body>" can't extend into the closing "```" fence
        // marker, so it's flushed eagerly on the push that produces it
        // (after the opener line is consumed).
        let chunks = p.push("```html-canvas\n<body>");
        assert_eq!(chunks, vec![FenceChunk::Html("<body>".into())]);
        let fin = p.finish();
        assert!(
            fin.chunks.is_empty(),
            "body was already flushed during push; finish has nothing left"
        );
        assert!(fin.unclosed_fence);
    }

    /// Eager flush regression: when streaming token-by-token sub-line text,
    /// the parser must not buffer the partial line if it has already
    /// committed to being content (not a fence marker). The TUI relies on
    /// seeing each token chunk as it arrives.
    #[test]
    fn partial_line_text_flushes_eagerly() {
        let mut p = FenceParser::new();
        let chunks_1 = p.push("hel");
        assert_eq!(chunks_1, vec![FenceChunk::Text("hel".into())]);
        let chunks_2 = p.push("lo");
        assert_eq!(chunks_2, vec![FenceChunk::Text("lo".into())]);
        // No newline arrived — but each token was flushed independently.
        let fin = p.finish();
        assert!(fin.chunks.is_empty());
        assert!(!fin.unclosed_fence);
    }

    /// A partial line that *could* still become a fence opener must
    /// stay buffered until either it diverges from the opener shape or
    /// a newline arrives.
    #[test]
    fn partial_fence_opener_prefix_is_buffered() {
        let mut p = FenceParser::new();
        // Each of these is a prefix of "```html-canvas" — all must buffer.
        for &prefix in &["`", "``", "```", "```h", "```html-canva"] {
            let mut q = FenceParser::new();
            let chunks = q.push(prefix);
            assert!(
                chunks.is_empty(),
                "prefix {prefix:?} must buffer (could extend to opener)"
            );
        }
        // Leading whitespace before a partial opener also buffers.
        let chunks = p.push("  ``");
        assert!(chunks.is_empty(), "whitespace + partial opener must buffer");
    }

    #[test]
    fn closing_fence_without_trailing_newline_is_not_unclosed() {
        let mut p = FenceParser::new();
        // Open fence + html body, both terminated by \n so they're consumed.
        let chunks_open = p.push("```html-canvas\n<p>hi</p>\n");
        assert_eq!(chunks_open, vec![FenceChunk::Html("<p>hi</p>\n".into())]);
        // Close fence arrives without trailing newline; stays in buf.
        let chunks_close = p.push("```");
        assert!(chunks_close.is_empty(), "no newline yet — nothing emitted");
        // finish() must process the buffered closing fence and clear the
        // unclosed flag.
        let fin = p.finish();
        assert!(fin.chunks.is_empty());
        assert!(
            !fin.unclosed_fence,
            "closing fence in buffer at finish should close cleanly",
        );
    }

    #[test]
    fn embedded_triple_backticks_close_fence_known_limitation() {
        // KNOWN LIMITATION: a literal "```" line inside the HTML body
        // (e.g., from a <pre><code> showing markdown source) closes the
        // fence prematurely. The parser is line-based and does not parse
        // HTML to distinguish a code-block illustration from a real
        // closing fence.
        //
        // If a future need arises (e.g., model output that quotes
        // savvagent-canvas examples), this test should be updated to
        // assert the correct behavior and the parser extended (e.g.,
        // track <pre>/<code> nesting or require the closing fence to be
        // followed by a sentinel).
        let mut p = FenceParser::new();
        let chunks = p.push("```html-canvas\n<pre><code>\n```\n</code></pre>\n```\n");
        // Observed (current) behavior: the embedded ``` closes the fence
        // after "<pre><code>\n"; the rest is plain text.
        assert_eq!(
            chunks,
            vec![
                FenceChunk::Html("<pre><code>\n".into()),
                FenceChunk::Text("</code></pre>\n```\n".into()),
            ],
        );
    }
}
