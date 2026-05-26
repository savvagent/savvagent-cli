# Inline HTML canvas — Phase 1 implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render model-emitted HTML inline in the conversation transcript as static images via Blitz + a terminal image protocol, with streaming source-preview, auto-export to disk, and plugin-contributed system prompt segments. No interactivity yet — that lands in Phase 2.

**Architecture:** SPP gains an `Html { source }` content block + `HtmlSourceDelta` stream delta. A new `savvagent-fence` crate parses ``` ```html-canvas ``` ``` sentinels out of streaming model text; provider crates wire it in so they emit `Html` blocks alongside `Text`. A new `savvagent-canvas` crate wraps Blitz behind a WIT-portable `ContentRenderer` trait (added to `savvagent-plugin`). The TUI's existing `Entry` enum (in `app.rs`) gains a `Canvas` variant; `ratatui-image` places rendered frames inline. A new `internal:html-canvas` built-in plugin owns the renderer factory, contributes a `SystemPromptSegment` that tells models to use the canvas, registers a `/save-canvas` slash, and auto-exports each canvas to `~/.savvagent/canvases/`.

**Tech Stack:** Rust 2024, `blitz` (pinned by Phase 0 spike), `ratatui-image`, `html5ever` (transitive via Blitz), `tokio` (existing async runtime), `serde`+`schemars` (existing for SPP), `serde_json` (existing).

**Spec:** `docs/superpowers/specs/2026-05-21-inline-html-canvas-design.md`. This plan covers **Phase 0 spike + Phase 1 only**. Phase 2 (mouse/keyboard interaction, soft freeze, focus management, Ctrl-O) ships in a separate plan once Phase 1 lands and the spike's findings are absorbed.

**Spec drift note:** The spec uses the placeholder name `LogItem` for the conversation-log item type. The actual codebase uses `Entry` (`crates/savvagent/src/app.rs:212`). This plan uses `Entry` consistently. Treat any spec mention of `LogItem` as referring to `Entry`.

**Release discipline:** Per the project's `feedback_phase_release_rollup` convention, this phase ends with a *scaffolding* `release(0.17.0)` commit — version bumps, CHANGELOG entries, README updates — but **no git tag is pushed**. Phase 2 ends with a `release(0.18.0)` scaffolding commit, and only after Phase 2 lands does the v0.18.0 tag get pushed (cargo-dist owns the actual release artifact build).

---

## File structure (Phase 0 + Phase 1)

**New crates:**
- `crates/savvagent-fence/` — sentinel-fence parser (`Cargo.toml`, `src/lib.rs`, `src/tests.rs`).
- `crates/savvagent-canvas/` — Blitz wrapper + `HtmlCanvas` implementing `ContentRenderer` (`Cargo.toml`, `src/lib.rs`, `src/canvas.rs`, `src/subset.rs`).

**New files in existing crates:**
- `crates/savvagent-plugin/src/content.rs` — `ContentRenderer` trait + supporting types (`Frame`, `PixelFormat`, `PixelSize`, `ContentBlockId`, `InputEvent`, `MouseEventPortable`, `MouseEventKind`, `MouseButton`, `InputOutcome`, `FocusableElement`, `Rect`).
- `crates/savvagent-plugin/src/prompt.rs` — `SystemPromptSegment` type.
- `crates/savvagent/src/plugin/builtin/html_canvas/` — the built-in plugin (`mod.rs`, `plugin.rs`, `prompt_text.rs`, `slash.rs`, `auto_export.rs`).
- `docs/superpowers/notes/2026-05-21-blitz-spike.md` — Phase 0 spike findings (created during Task 1).

**Modified files:**
- `crates/savvagent-protocol/src/content.rs` — add `ContentBlock::Html { source }`.
- `crates/savvagent-protocol/src/stream.rs` — add `BlockDelta::HtmlSourceDelta { source }`.
- `crates/savvagent-protocol/src/lib.rs` — re-exports.
- `crates/savvagent-protocol/SPEC.md` — document new variants; bump conformance level to **SPP v0.2.0**.
- `crates/savvagent-plugin/src/lib.rs` — module declarations + re-exports.
- `crates/savvagent-plugin/src/manifest.rs` — extend `Contributions` (add `content_renderers`, `prompt_segments`); extend `SlashSpec` (add `suppress_prompt_segments`).
- `crates/savvagent-plugin/src/plugin.rs` — add `Plugin::create_renderer` method with default impl.
- `crates/savvagent-plugin/src/effect.rs` — add `Effect::OpenUrl { url, target }` + `UrlTarget` enum.
- `crates/savvagent-plugin/src/error.rs` — add `PluginError::ContentRendererNotFound`.
- `crates/savvagent-host/src/default_prompt.rs` — accept caller-supplied `&[SystemPromptSegment]` and concatenate them after the conventions section.
- `crates/savvagent-host/src/session.rs` — gather active-plugin segments + honor per-slash suppression when composing the `system` field.
- `crates/savvagent/src/app.rs` — add `Entry::Canvas { id, source }` variant + `CanvasRegistry` field on `App`.
- `crates/savvagent/src/ui.rs` — render `Entry::Canvas` via `ratatui-image`; source-code fallback.
- `crates/savvagent/src/plugin/mod.rs` — register `HtmlCanvasPlugin` in `register_builtins()` and route `Effect::OpenUrl` (via `effects.rs`).
- `crates/savvagent/src/plugin/registry.rs` — extend `Indexes` with `content_renderers: HashMap<String, PluginId>`.
- `crates/savvagent/src/plugin/manifests.rs` — build the new index.
- `crates/savvagent/src/plugin/effects.rs` — handle `Effect::OpenUrl` (shell to `xdg-open` / `open`).
- `crates/provider-anthropic/src/stream.rs` — inject fence parser into the translator's text emission path.
- `crates/provider-gemini/src/stream.rs` — same.
- `crates/provider-openai/src/stream.rs` — same.
- `crates/provider-local/src/stream.rs` — same.
- `Cargo.toml` (workspace) — add `savvagent-fence`, `savvagent-canvas`, `blitz`, `ratatui-image` to `[workspace.dependencies]`.
- `README.md` — feature blurb, terminal compatibility matrix, tmux passthrough note.
- `CHANGELOG.md` — Phase 1 entry under `## [0.17.0] - unreleased`.

---

## Task 1: Phase 0 — Blitz embedding spike

**Files:**
- Create: `docs/superpowers/notes/2026-05-21-blitz-spike.md`
- Create: a throwaway crate `crates/_blitz-spike/` (deleted at the end of this task)
- Modify: `Cargo.toml` (workspace) — add the throwaway crate to `members` temporarily

The spec assumes Blitz exposes headless layout+paint and a way to dispatch synthetic input. Before committing to a pinned version and a `savvagent-canvas` design, verify the assumptions hands-on. Output is a notes doc that pins a version and either confirms the design or amends the spec.

- [ ] **Step 1: Identify a candidate Blitz version**

Run:

```bash
cargo search blitz --limit 5
```

Pick the most recent published `blitz` crate that matches the project's published-only-dependencies policy. Record the version chosen and the publish date in the notes doc.

- [ ] **Step 2: Create the throwaway crate skeleton**

```bash
mkdir -p crates/_blitz-spike/src
```

Create `crates/_blitz-spike/Cargo.toml`:

```toml
[package]
name = "_blitz-spike"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
# version filled in from Step 1
blitz = "<pinned-version>"
image = "0.25"
```

Add the crate to workspace `members` in the root `Cargo.toml`:

```toml
[workspace]
members = [
    # ... existing entries ...
    "crates/_blitz-spike",
]
```

- [ ] **Step 3: Write the spike example**

Create `crates/_blitz-spike/src/main.rs`. The exact API calls depend on the Blitz version chosen in Step 1; the goal is to exercise five capabilities and document each:

```rust
//! Phase 0 spike for the inline HTML canvas feature.
//!
//! Exercises: (1) parse + lay out a fixed-size HTML document, (2) paint
//! to an RGBA buffer, (3) save the buffer as a PNG, (4) report the
//! document's natural height at the given width, (5) attempt synthetic
//! mouse event dispatch and observe DOM state change.
//!
//! Findings recorded in
//! docs/superpowers/notes/2026-05-21-blitz-spike.md.

const SAMPLE_HTML: &str = r#"
<!doctype html>
<html>
  <head>
    <style>
      body { font-family: sans-serif; margin: 24px; }
      h1 { color: #2563eb; }
      .badge {
        display: inline-block;
        padding: 4px 10px;
        background: #fde68a;
        border-radius: 12px;
      }
      a:hover { text-decoration: underline; }
      details > summary { cursor: pointer; }
    </style>
  </head>
  <body>
    <h1>Plan: refactor X</h1>
    <p>Status: <span class="badge">in progress</span></p>
    <details>
      <summary>Details</summary>
      <p>Hidden by default.</p>
    </details>
    <p><a href="https://example.com">Reference</a></p>
  </body>
</html>
"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Step 5a: parse + layout at 800px width.
    // Step 5b: paint to RGBA.
    // Step 5c: save the buffer as `target/spike-output.png`.
    // Step 5d: print the natural height.
    // Step 5e: attempt to dispatch a click at the <summary> element's
    //          pixel position; re-paint; save as `target/spike-clicked.png`.
    //
    // The exact Blitz API calls go here. Document the actual API in
    // the notes doc.
    todo!("populate from Blitz API discovered during spike");
}
```

The `todo!()` is intentional — Step 4 fills it in *after* exploring Blitz's actual API.

- [ ] **Step 4: Explore Blitz API + populate the spike**

In a Rust playground or `cargo doc`-driven exploration of the pinned Blitz crate, discover:

1. How to parse an HTML string into Blitz's document model.
2. How to drive layout at a fixed viewport width.
3. How to paint to an RGBA pixel buffer.
4. How to query the document's natural content height at the given width.
5. Whether Blitz exposes synthetic event dispatch (click/hover/scroll). If so, the API. If not, what would need to be built on top.

Replace the `todo!()` with concrete code that exercises all five. Save the rendered PNGs.

- [ ] **Step 5: Run the spike**

```bash
cargo run -p _blitz-spike
```

Inspect `target/spike-output.png` (initial render) and `target/spike-clicked.png` (after synthetic click, if Blitz supports it).

- [ ] **Step 6: Write findings to the notes doc**

Create `docs/superpowers/notes/2026-05-21-blitz-spike.md`. Cover:

- **Pinned version:** `blitz = "<x.y.z>"` — chosen because <one sentence>.
- **Static rendering:** confirmed / has issues / blocked. Describe.
- **Pixel buffer access:** API shape (`fn paint(...) -> Vec<u8>` or equivalent).
- **Natural height query:** API shape.
- **Synthetic event dispatch:** supported as-built / supported with extra glue / not supported in this version. If extra glue is needed, sketch what (a separate layout-update entrypoint + manual focus tracking? a fork? a different crate?).
- **CSS subset coverage:** of the spec's listed properties, which Blitz handles cleanly, which degrade, which crash.
- **Decision:** confirm the spec as written / amend the spec (and what amendments).
- **Phase 2 risk update:** with what we now know, is Phase 2's eventing trait surface achievable as written, or does it need a host-side router on top?

- [ ] **Step 7: If the spec needs amendments, amend it**

If the spike surfaced material divergence from the spec (e.g., Blitz doesn't expose synthetic events at all, or natural height is not queryable), edit `docs/superpowers/specs/2026-05-21-inline-html-canvas-design.md` in place:

- Update the "Approach risks" section with the new findings.
- Update Phase 2's eventing approach if needed.
- Add a note to the "Open questions" section linking to the spike output.

If no amendments are needed, note this in the spike doc.

- [ ] **Step 8: Tear down the throwaway crate**

```bash
rm -rf crates/_blitz-spike
```

Remove `"crates/_blitz-spike"` from workspace `members` in the root `Cargo.toml`.

Confirm the workspace still builds:

```bash
cargo build --workspace
```

Expected: success.

- [ ] **Step 9: Commit**

```bash
git add docs/superpowers/notes/2026-05-21-blitz-spike.md
# If you amended the spec:
git add docs/superpowers/specs/2026-05-21-inline-html-canvas-design.md
git commit -m "docs(spike): blitz embedding spike for inline HTML canvas"
```

---

## Task 2: SPP — `ContentBlock::Html` variant

**Files:**
- Modify: `crates/savvagent-protocol/src/content.rs`

Add a new variant to the existing `ContentBlock` enum mirroring the `Text` variant's shape (one owned `String` field). The variant follows the existing `#[serde(tag = "type", rename_all = "snake_case")]` convention so the wire form is `{ "type": "html", "source": "..." }`.

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` block in `crates/savvagent-protocol/src/content.rs`:

```rust
    #[test]
    fn html_round_trip() {
        let block = ContentBlock::Html {
            source: "<!doctype html><body>hi</body>".into(),
        };
        let v = serde_json::to_value(&block).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "type": "html",
                "source": "<!doctype html><body>hi</body>",
            }),
        );
        let back: ContentBlock = serde_json::from_value(v).unwrap();
        assert_eq!(back, block);
    }
```

- [ ] **Step 2: Run the test; verify it fails**

```bash
cargo test -p savvagent-protocol content::tests::html_round_trip
```

Expected: FAIL with `no variant or associated item named 'Html' found for enum 'ContentBlock'`.

- [ ] **Step 3: Add the variant**

In `crates/savvagent-protocol/src/content.rs`, add the new variant to the existing `ContentBlock` enum (insert after the `Thinking` variant so it appears at the end):

```rust
    /// HTML source the host should render inline in the conversation
    /// transcript via a registered `ContentRenderer` plugin. Used for
    /// structured documents (plans, specs, status updates) where
    /// rendered HTML is more legible than markdown.
    ///
    /// The source is a complete HTML document; the renderer parses it
    /// fresh. Hosts that do not have a renderer registered render the
    /// source as a code block.
    Html {
        /// Complete HTML document source.
        source: String,
    },
```

- [ ] **Step 4: Run the test; verify it passes**

```bash
cargo test -p savvagent-protocol content::tests::html_round_trip
```

Expected: PASS.

- [ ] **Step 5: Run the full crate's tests**

```bash
cargo test -p savvagent-protocol
```

Expected: PASS (the new variant should not regress any existing serde test).

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-protocol/src/content.rs
git commit -m "feat(protocol): add ContentBlock::Html variant"
```

---

## Task 3: SPP — `BlockDelta::HtmlSourceDelta` variant

**Files:**
- Modify: `crates/savvagent-protocol/src/stream.rs`

Mirror the existing `TextDelta` shape: one owned `String` carrying a fragment of the HTML source. Concatenated by the host until `ContentBlockStop`.

- [ ] **Step 1: Write the failing test**

Append to a `#[cfg(test)] mod tests` block at the bottom of `crates/savvagent-protocol/src/stream.rs` (create the block if none exists yet):

```rust
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
```

- [ ] **Step 2: Run the test; verify it fails**

```bash
cargo test -p savvagent-protocol stream::tests::html_source_delta_round_trip
```

Expected: FAIL.

- [ ] **Step 3: Add the variant**

In `crates/savvagent-protocol/src/stream.rs`, add the new variant to the existing `BlockDelta` enum (insert after `SignatureDelta`):

```rust
    /// Append a fragment of HTML source to a `ContentBlock::Html` block
    /// during streaming. Hosts concatenate `source` fragments across
    /// deltas until `ContentBlockStop`, then hand the assembled source
    /// to the registered renderer.
    HtmlSourceDelta {
        /// HTML source fragment.
        source: String,
    },
```

- [ ] **Step 4: Run the test; verify it passes**

```bash
cargo test -p savvagent-protocol stream::tests::html_source_delta_round_trip
```

Expected: PASS.

- [ ] **Step 5: Run the full crate's tests**

```bash
cargo test -p savvagent-protocol
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-protocol/src/stream.rs
git commit -m "feat(protocol): add BlockDelta::HtmlSourceDelta variant"
```

---

## Task 4: SPP — bump SPEC.md to v0.2.0

**Files:**
- Modify: `crates/savvagent-protocol/SPEC.md`

Document the two new variants and bump the conformance version. Spec doc only; no code change.

- [ ] **Step 1: Update the version banner**

At the top of `crates/savvagent-protocol/SPEC.md`, change:

```markdown
# Savvagent Provider Protocol (SPP) — v0.1.0
```

to:

```markdown
# Savvagent Provider Protocol (SPP) — v0.2.0
```

- [ ] **Step 2: Document the `html` content block**

In the "Supported block types" list in `SPEC.md`, add (right after the `thinking` entry):

```markdown
- `html` — `{ "type": "html", "source": "<complete HTML document>" }`. Used
  by hosts that have an HTML renderer registered (e.g. inline rendering in
  a terminal that supports an image protocol). Hosts without a renderer
  render the source as a code block. The source is parsed fresh on each
  render; providers may emit it via [`StreamEvent::ContentBlockStart`] +
  zero or more [`StreamEvent::ContentBlockDelta`] with `html_source_delta`
  before [`StreamEvent::ContentBlockStop`].
```

- [ ] **Step 3: Document the `html_source_delta` stream delta**

In the `StreamEvent` example flow in `SPEC.md`, add a comment block after the `thinking` example showing the HTML streaming flow:

```text
content_block_start { index: 3, block: html("") }
content_block_delta { index: 3, delta: html_source_delta("<!docty") }
content_block_delta { index: 3, delta: html_source_delta("pe html><body>...") }
content_block_stop  { index: 3 }
```

- [ ] **Step 4: Document the additive-compat rule**

At the bottom of the SPEC.md "Conformance" section, append:

```markdown
### v0.2.0 changes (additive)

- Added `ContentBlock::Html` variant.
- Added `BlockDelta::HtmlSourceDelta` variant.

Providers emitting only v0.1.0 block types remain conformant; the new
types are additive. Hosts that do not handle `Html` blocks SHOULD render
the source as a code block to avoid silently dropping output.
```

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-protocol/SPEC.md
git commit -m "docs(protocol): bump SPP to v0.2.0 with html block + delta"
```

---

## Task 5: `savvagent-fence` crate

**Files:**
- Create: `crates/savvagent-fence/Cargo.toml`
- Create: `crates/savvagent-fence/src/lib.rs`
- Modify: `Cargo.toml` (workspace) — add member + workspace dep entry

A small stream-friendly parser that splits incoming text into `Text(String)` and `Html(String)` chunks by recognizing a ```` ```html-canvas ```` opening fence and its matching ` ``` ` closer. Has to handle: arrival-by-fragments (partial fences across deltas), code fences with other languages (passthrough as text), nested triple-backticks inside HTML (rare but possible — `<pre><code>`).

- [ ] **Step 1: Create the crate skeleton**

```bash
mkdir -p crates/savvagent-fence/src
```

Create `crates/savvagent-fence/Cargo.toml`:

```toml
[package]
name = "savvagent-fence"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true
description = "Streaming parser that extracts savvagent-canvas HTML fences from model text"

[dependencies]
# none — pure std

[dev-dependencies]
```

Add the crate to workspace `members` and `[workspace.dependencies]` in the root `Cargo.toml`:

```toml
[workspace]
members = [
    # ... existing ...
    "crates/savvagent-fence",
]

[workspace.dependencies]
# ... existing ...
savvagent-fence = { path = "crates/savvagent-fence", version = "0.17.0" }
```

(The version `0.17.0` matches the post-Phase-1 workspace bump from Task 22. Until Task 22 lands, the existing workspace version is `0.16.1`; substitute that here and update in Task 22 along with the workspace literal.)

- [ ] **Step 2: Write the failing tests**

Create `crates/savvagent-fence/src/lib.rs`:

```rust
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
    pub fn push(&mut self, fragment: &str) -> Vec<FenceChunk> {
        let mut out = Vec::new();
        self.buf.push_str(fragment);

        // Walk line-by-line, but keep any trailing incomplete line in `buf`.
        loop {
            match self.buf.find('\n') {
                Some(nl) => {
                    let line: String = self.buf.drain(..=nl).collect();
                    self.consume_line(&line, &mut out);
                }
                None => break,
            }
        }
        out
    }

    /// End of stream — flush remaining buffered content.
    pub fn finish(mut self) -> FinishResult {
        let mut chunks = Vec::new();
        let unclosed = self.inside_canvas;
        if !self.buf.is_empty() {
            // Flush trailing partial line.
            let line = std::mem::take(&mut self.buf);
            self.consume_line(&line, &mut chunks);
        }
        FinishResult {
            chunks,
            unclosed_fence: unclosed,
        }
    }

    fn consume_line(&mut self, line: &str, out: &mut Vec<FenceChunk>) {
        let trimmed = line.trim_end_matches(|c: char| c == '\n' || c == '\r');
        if !self.inside_canvas {
            if trimmed.trim_start() == "```html-canvas" {
                self.inside_canvas = true;
                return; // fence line consumed; no emission
            }
            append_text(out, line);
        } else {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_fence_is_pure_text() {
        let mut p = FenceParser::new();
        let chunks = p.push("hello\nworld\n");
        assert_eq!(
            chunks,
            vec![FenceChunk::Text("hello\nworld\n".into())]
        );
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
        let mut chunks = p.push("```ht");
        chunks.extend(p.push("ml-canvas\n<b>"));
        chunks.extend(p.push("hi</b>\n```\n"));
        assert_eq!(chunks, vec![FenceChunk::Html("<b>hi</b>\n".into())]);
    }

    #[test]
    fn unclosed_fence_flushed_at_finish() {
        let mut p = FenceParser::new();
        let chunks = p.push("```html-canvas\n<body>");
        assert!(chunks.is_empty());
        let fin = p.finish();
        assert_eq!(fin.chunks, vec![FenceChunk::Html("<body>".into())]);
        assert!(fin.unclosed_fence);
    }
}
```

- [ ] **Step 3: Build + run the tests**

```bash
cargo test -p savvagent-fence
```

Expected: all six tests pass on the first run (the code in Step 2 is the full impl). If anything fails, fix and re-run.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/savvagent-fence/
git commit -m "feat(fence): savvagent-fence crate for html-canvas extraction"
```

---

## Task 6: Wire fence parser into all four providers

**Files:**
- Modify: `crates/provider-anthropic/src/stream.rs`, `crates/provider-anthropic/Cargo.toml`
- Modify: `crates/provider-gemini/src/stream.rs`, `crates/provider-gemini/Cargo.toml`
- Modify: `crates/provider-openai/src/stream.rs`, `crates/provider-openai/Cargo.toml`
- Modify: `crates/provider-local/src/stream.rs`, `crates/provider-local/Cargo.toml`

Each provider already maps vendor stream deltas to SPP `StreamEvent`s. The fence parser sits between "vendor text fragment arrives" and "emit a `BlockDelta::TextDelta`": split the fragment into Text and Html chunks; for Text emit `TextDelta` on the current text block; for Html, close the current text block (`ContentBlockStop`), start a new `ContentBlock::Html("")` (`ContentBlockStart`), emit `HtmlSourceDelta`(s), and `ContentBlockStop` when the fence closes.

Because the existing translators differ slightly across providers, this task is one task with four sub-steps (one per provider). Each sub-step does the same shape of work; the exact insertion point is provider-specific.

- [ ] **Step 1: Add `savvagent-fence` as a workspace dep for each provider**

For each of `provider-anthropic`, `provider-gemini`, `provider-openai`, `provider-local`, edit `crates/provider-<x>/Cargo.toml` and add:

```toml
[dependencies]
# ... existing entries ...
savvagent-fence = { workspace = true }
```

- [ ] **Step 2: Write a shared fixture test for each provider**

For each provider, append to `crates/provider-<x>/src/stream.rs` (or the existing tests module — adapt to the file's conventions):

```rust
#[cfg(test)]
mod html_fence_tests {
    use super::*;
    use savvagent_protocol::{BlockDelta, ContentBlock, StreamEvent};

    /// Drive the translator with a stream containing one html-canvas
    /// fence and assert that the emitted SPP events have the expected
    /// content_block_start/delta/stop shape.
    #[tokio::test]
    async fn stream_emits_html_blocks_for_canvas_fence() {
        // Synthesize a minimal vendor stream that the translator turns
        // into text fragments containing:
        //   "Here:\n```html-canvas\n<p>hi</p>\n```\n"
        //
        // Assert the emitted SPP StreamEvents include:
        //   - ContentBlockStart with Text("")
        //   - ContentBlockDelta with TextDelta("Here:\n")
        //   - ContentBlockStop
        //   - ContentBlockStart with Html("")
        //   - ContentBlockDelta with HtmlSourceDelta("<p>hi</p>\n")
        //   - ContentBlockStop
        //
        // The exact vendor fixture format is provider-specific; use the
        // existing test patterns in this file as a template.
        todo!("provider-specific fixture; see step 2 instructions");
    }
}
```

The `todo!()` is intentional — Step 3 wires it once the fence integration is in place. The shape of the assertion above is the contract every provider must satisfy.

- [ ] **Step 3: Add a `FenceParser` to the translator's per-stream state**

For each provider, find the existing per-stream translator struct (named e.g. `AnthropicStream`, `GeminiStream`, etc.). Add a `fence_parser: savvagent_fence::FenceParser` field. Initialize via `FenceParser::new()` in the constructor.

```rust
use savvagent_fence::{FenceChunk, FenceParser};

pub(crate) struct <ProviderName>Stream {
    // ... existing fields ...
    fence_parser: FenceParser,
    // Track currently-open block index for fence-driven block boundaries.
    next_block_index: u32,
    current_block: Option<CurrentBlock>,
}

enum CurrentBlock {
    Text { index: u32 },
    Html { index: u32 },
}
```

- [ ] **Step 4: Replace direct `TextDelta` emission with fence-driven dispatch**

Find the path in each translator that currently does roughly:

```rust
// pseudocode of the existing path
let text = vendor_text_fragment(...);
emit(StreamEvent::ContentBlockDelta {
    index: self.current_text_index,
    delta: BlockDelta::TextDelta { text },
});
```

Replace it with the fence-aware path:

```rust
let text = vendor_text_fragment(...);
let chunks = self.fence_parser.push(&text);
for chunk in chunks {
    self.emit_chunk(chunk, sink).await?;
}
```

Add the `emit_chunk` helper on the translator:

```rust
async fn emit_chunk(
    &mut self,
    chunk: FenceChunk,
    sink: &mut StreamSink,
) -> Result<(), StreamError> {
    match chunk {
        FenceChunk::Text(text) => self.emit_text(text, sink).await,
        FenceChunk::Html(html) => self.emit_html(html, sink).await,
    }
}

async fn emit_text(
    &mut self,
    text: String,
    sink: &mut StreamSink,
) -> Result<(), StreamError> {
    let index = match self.current_block {
        Some(CurrentBlock::Text { index }) => index,
        Some(CurrentBlock::Html { index }) => {
            // Close the open html block; open a fresh text block.
            sink.send(StreamEvent::ContentBlockStop { index }).await?;
            self.current_block = None;
            self.open_text_block(sink).await?
        }
        None => self.open_text_block(sink).await?,
    };
    sink.send(StreamEvent::ContentBlockDelta {
        index,
        delta: BlockDelta::TextDelta { text },
    })
    .await?;
    Ok(())
}

async fn emit_html(
    &mut self,
    html: String,
    sink: &mut StreamSink,
) -> Result<(), StreamError> {
    let index = match self.current_block {
        Some(CurrentBlock::Html { index }) => index,
        Some(CurrentBlock::Text { index }) => {
            sink.send(StreamEvent::ContentBlockStop { index }).await?;
            self.current_block = None;
            self.open_html_block(sink).await?
        }
        None => self.open_html_block(sink).await?,
    };
    sink.send(StreamEvent::ContentBlockDelta {
        index,
        delta: BlockDelta::HtmlSourceDelta { source: html },
    })
    .await?;
    Ok(())
}

async fn open_text_block(
    &mut self,
    sink: &mut StreamSink,
) -> Result<u32, StreamError> {
    let index = self.next_block_index;
    self.next_block_index += 1;
    sink.send(StreamEvent::ContentBlockStart {
        index,
        block: ContentBlock::Text { text: String::new() },
    })
    .await?;
    self.current_block = Some(CurrentBlock::Text { index });
    Ok(index)
}

async fn open_html_block(
    &mut self,
    sink: &mut StreamSink,
) -> Result<u32, StreamError> {
    let index = self.next_block_index;
    self.next_block_index += 1;
    sink.send(StreamEvent::ContentBlockStart {
        index,
        block: ContentBlock::Html { source: String::new() },
    })
    .await?;
    self.current_block = Some(CurrentBlock::Html { index });
    Ok(index)
}
```

(Names `StreamSink` and `StreamError` are placeholders for whatever the provider crate already uses to forward events. Use the existing types.)

- [ ] **Step 5: Drain the parser at stream end**

In each translator's stream-completion path (where it currently emits `MessageStop`), add a `fence_parser.finish()` drain *before* the close events:

```rust
let finish = std::mem::take(&mut self.fence_parser).finish();
for chunk in finish.chunks {
    self.emit_chunk(chunk, sink).await?;
}
if finish.unclosed_fence {
    tracing::warn!("provider stream ended with unclosed html-canvas fence");
}
if let Some(block) = self.current_block.take() {
    let index = match block {
        CurrentBlock::Text { index } | CurrentBlock::Html { index } => index,
    };
    sink.send(StreamEvent::ContentBlockStop { index }).await?;
}
// ... then the existing MessageStop emission.
```

- [ ] **Step 6: Wire up the test fixtures**

Replace each provider's `todo!()` from Step 2 with an actual vendor fixture. For Anthropic, model the existing fixture pattern used by other tests in `stream.rs`. For each provider, the assertion shape stays the same — only the input format changes.

- [ ] **Step 7: Run each provider's tests**

```bash
cargo test -p provider-anthropic
cargo test -p provider-gemini
cargo test -p provider-openai
cargo test -p provider-local
```

Expected: each `stream_emits_html_blocks_for_canvas_fence` passes. All existing tests still pass.

- [ ] **Step 8: Run the full workspace tests**

```bash
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/provider-anthropic crates/provider-gemini crates/provider-openai crates/provider-local
git commit -m "feat(providers): extract html-canvas fences from streaming text"
```

---

## Task 7: Plugin trait — `ContentRenderer` + supporting types

**Files:**
- Create: `crates/savvagent-plugin/src/content.rs`
- Modify: `crates/savvagent-plugin/src/lib.rs`
- Modify: `crates/savvagent-plugin/src/error.rs`

Add the WIT-portable types Phase 1 needs (`Frame`, `PixelFormat`, `PixelSize`, `ContentBlockId`) plus the *full* `ContentRenderer` trait surface — even the Phase 2 methods (`dispatch`, `freeze`, `thaw`, `focusable_elements`, `set_focus`, `focused_index`). Phase 1 leaves the Phase 2 methods as default no-ops so the canvas renderer can implement the trait against a static-only impl now and add bodies in Phase 2.

Adding the full trait now (vs. extending later) avoids trait-signature churn between Phase 1 and Phase 2.

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod trait_smoke` in `crates/savvagent-plugin/src/lib.rs`:

```rust
    #[test]
    fn frame_round_trips_through_pixel_format() {
        use crate::content::{Frame, PixelFormat, PixelSize};
        let frame = Frame {
            width: 2,
            height: 1,
            format: PixelFormat::Rgba8,
            bytes: vec![255, 0, 0, 255, 0, 0, 255, 255],
        };
        assert_eq!(frame.width, 2);
        assert_eq!(frame.bytes.len(), 8);
        let size = PixelSize { width: 100, height: 50 };
        assert_eq!(size.width * size.height, 5_000);
    }
```

- [ ] **Step 2: Run the test; verify it fails**

```bash
cargo test -p savvagent-plugin trait_smoke::frame_round_trips_through_pixel_format
```

Expected: FAIL with `unresolved module 'content'`.

- [ ] **Step 3: Add `PluginError::ContentRendererNotFound`**

In `crates/savvagent-plugin/src/error.rs`, extend the existing `PluginError` enum with a new variant. Match the surrounding style:

```rust
    /// No registered plugin advertises a `ContentRendererSpec` for the
    /// given block_type. Returned by `Plugin::create_renderer` default impl.
    ContentRendererNotFound(String),
```

And update the `Display` impl with a branch:

```rust
            Self::ContentRendererNotFound(block_type) => write!(
                f,
                "no plugin claims content renderer for block_type '{}'",
                block_type
            ),
```

- [ ] **Step 4: Create `content.rs` with all Phase 1 and Phase 2 types**

Create `crates/savvagent-plugin/src/content.rs`:

```rust
//! WIT-portable types for plugins that render structured content blocks
//! inline in the conversation transcript.
//!
//! Phase 1 (this release) uses only [`Frame`], [`PixelSize`],
//! [`PixelFormat`], [`ContentBlockId`], and [`ContentRenderer::render`].
//! Phase 2 adds event dispatch, freeze/thaw, and focus traversal.
//! The full trait surface ships in Phase 1 with no-op defaults so
//! renderer implementations don't need a second trait-signature update
//! when Phase 2 lands.
//!
//! Portability rules (see `2026-05-12-v0.9.0-plugin-system-design.md` §9):
//! all owned data, explicit-width numerics, closed enums.

use async_trait::async_trait;

use crate::effect::Effect;
use crate::error::PluginError;
use crate::types::{KeyEventPortable, KeyMods};

/// Identifier the host assigns to a content block when constructing a
/// renderer. Opaque to plugins; used as a routing key by the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentBlockId(pub u32);

/// Pixel format of a rendered [`Frame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// Red, green, blue, alpha — 8 bits each, row-major, top-down.
    Rgba8,
    /// Blue, green, red, alpha — 8 bits each. Some terminals prefer this.
    Bgra8,
}

/// Pixel dimensions for [`ContentRenderer::render`] requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelSize {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels. Renderer is free to honor this loosely; the
    /// returned frame's height is authoritative.
    pub height: u32,
}

/// A rendered image frame returned by [`ContentRenderer::render`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pixel format of [`Frame::bytes`].
    pub format: PixelFormat,
    /// Raw pixel data, row-major, no padding. Length is
    /// `width * height * bytes_per_pixel(format)`.
    pub bytes: Vec<u8>,
}

/// Bounding box within a rendered frame, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// X offset in pixels from the frame's top-left.
    pub x: u32,
    /// Y offset in pixels from the frame's top-left.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// One focusable element inside a rendered content block.
///
/// Used by Phase 2 to draw focus chrome around the active element and
/// to expose a deterministic Tab-traversal order. Phase 1 renderers
/// return an empty vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusableElement {
    /// Plugin-defined identifier. Stable for a given renderer instance.
    pub id: String,
    /// Bounding box within the rendered frame.
    pub bounds: Rect,
}

/// Phase 2: input event delivered to a [`ContentRenderer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// A key event (already-translated to a portable representation).
    Key(KeyEventPortable),
    /// A mouse event with frame-relative pixel coordinates.
    Mouse(MouseEventPortable),
    /// Focus gained or lost — host informs the renderer of focus changes.
    Focus(FocusKind),
}

/// Kind of a [`InputEvent::Focus`] event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusKind {
    /// The renderer just received focus.
    Gained,
    /// The renderer just lost focus.
    Lost,
}

/// Phase 2: a mouse event in frame-relative pixel coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MouseEventPortable {
    /// Press / release / move / scroll.
    pub kind: MouseEventKind,
    /// Mouse button, if applicable (None for moves and scrolls).
    pub button: Option<MouseButton>,
    /// X offset in pixels from the rendered frame's top-left.
    pub x_pixel: u32,
    /// Y offset in pixels.
    pub y_pixel: u32,
    /// Modifier keys held at the time of the event.
    pub modifiers: KeyMods,
}

/// Kind of mouse interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    /// Button down.
    Press,
    /// Button up.
    Release,
    /// Pointer movement (no button required).
    Move,
    /// Scroll wheel rotated up.
    ScrollUp,
    /// Scroll wheel rotated down.
    ScrollDown,
}

/// Mouse buttons reported by terminal mouse protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Left button.
    Left,
    /// Middle button.
    Middle,
    /// Right button.
    Right,
}

/// Phase 2: outcome of [`ContentRenderer::dispatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputOutcome {
    /// Effects the host should apply (e.g. `Effect::OpenUrl` when a
    /// link is followed).
    pub effects: Vec<Effect>,
    /// `true` iff the renderer's frame needs re-rendering.
    pub dirty: bool,
}

/// Render + interaction surface for one inline content block. Phase 1
/// requires only `render`; Phase 2 implements the rest.
#[async_trait]
pub trait ContentRenderer: Send {
    /// Stable identifier for this renderer instance.
    fn id(&self) -> ContentBlockId;

    /// Render at the given size; returns a frame whose width matches the
    /// requested width and whose height is the document's natural height
    /// for that width.
    fn render(&mut self, size: PixelSize) -> Frame;

    /// Phase 2: dispatch an input event. Default returns an empty
    /// non-dirty outcome so Phase 1 renderers compile.
    async fn dispatch(
        &mut self,
        _event: InputEvent,
    ) -> Result<InputOutcome, PluginError> {
        Ok(InputOutcome {
            effects: Vec::new(),
            dirty: false,
        })
    }

    /// Phase 2: stop dispatching events; retain state for thaw.
    fn freeze(&mut self) {}

    /// Phase 2: resume from freeze.
    fn thaw(&mut self) {}

    /// Phase 2: return current focusable elements in tab order.
    fn focusable_elements(&self) -> Vec<FocusableElement> {
        Vec::new()
    }

    /// Phase 2: index of the currently focused element, or `None`.
    fn focused_index(&self) -> Option<u32> {
        None
    }

    /// Phase 2: programmatically move focus.
    fn set_focus(&mut self, _index: Option<u32>) {}
}
```

- [ ] **Step 5: Wire the module into `lib.rs`**

In `crates/savvagent-plugin/src/lib.rs`, add the module declaration + re-exports near the existing module declarations:

```rust
/// Content renderer trait surface (HTML canvas etc.).
pub mod content;
pub use content::{
    ContentBlockId, ContentRenderer, FocusableElement, FocusKind, Frame,
    InputEvent, InputOutcome, MouseButton, MouseEventKind,
    MouseEventPortable, PixelFormat, PixelSize, Rect,
};
```

- [ ] **Step 6: Run the test; verify it passes**

```bash
cargo test -p savvagent-plugin trait_smoke::frame_round_trips_through_pixel_format
```

Expected: PASS.

- [ ] **Step 7: Confirm WIT-portability CI grep still clean**

```bash
grep -E '^(ratatui|crossterm|tokio|anyhow) = ' crates/savvagent-plugin/Cargo.toml
```

Expected: no output. (We added no runtime deps; `async_trait` was already there.)

- [ ] **Step 8: Run the full crate tests**

```bash
cargo test -p savvagent-plugin
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/savvagent-plugin/
git commit -m "feat(plugin): ContentRenderer trait surface + Frame types"
```

---

## Task 8: Plugin trait — `SystemPromptSegment` + `Contributions` extension

**Files:**
- Create: `crates/savvagent-plugin/src/prompt.rs`
- Modify: `crates/savvagent-plugin/src/lib.rs`
- Modify: `crates/savvagent-plugin/src/manifest.rs`

Add the new contribution kind plus the per-slash suppression list. The new `Contributions` fields are additive; existing built-in plugins keep compiling because `Contributions::default()` returns empty vectors for everything.

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` in `crates/savvagent-plugin/src/manifest.rs`:

```rust
    #[test]
    fn manifest_can_carry_prompt_segments() {
        let m = Manifest {
            id: PluginId("internal:test-prompt".into()),
            name: "Test prompt".into(),
            version: "0.17.0".into(),
            description: "Test segments".into(),
            kind: PluginKind::Optional,
            contributions: Contributions {
                prompt_segments: vec![SystemPromptSegment {
                    id: "internal:test-prompt:hello".into(),
                    text: "Be helpful.".into(),
                }],
                ..Contributions::default()
            },
        };
        assert_eq!(m.contributions.prompt_segments.len(), 1);
        assert_eq!(m.contributions.prompt_segments[0].id, "internal:test-prompt:hello");
    }

    #[test]
    fn slash_spec_carries_suppress_list() {
        let s = SlashSpec {
            name: "commit".into(),
            summary: "create a commit".into(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec!["internal:html-canvas:default".into()],
        };
        assert_eq!(s.suppress_prompt_segments.len(), 1);
    }
```

- [ ] **Step 2: Run the tests; verify they fail**

```bash
cargo test -p savvagent-plugin manifest::tests::manifest_can_carry_prompt_segments manifest::tests::slash_spec_carries_suppress_list
```

Expected: FAIL with `no field 'prompt_segments'` and `no field 'suppress_prompt_segments'`.

- [ ] **Step 3: Create `prompt.rs`**

Create `crates/savvagent-plugin/src/prompt.rs`:

```rust
//! System-prompt segment contributions and per-slash suppression.
//!
//! A `SystemPromptSegment` is one named string the host concatenates
//! into the model's `system` field after the host's own default prompt
//! and project context. `SlashSpec::suppress_prompt_segments` lists
//! segment ids to drop for the duration of a specific slash command's
//! turn (e.g. `/commit` suppressing `internal:html-canvas:default`).
//!
//! See the inline-html-canvas spec § "Prompt contention and suppression".

/// A single contributable segment of the model's system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPromptSegment {
    /// Stable identifier. Convention: `<plugin_id>:<segment_name>`,
    /// e.g. `"internal:html-canvas:default"`.
    pub id: String,
    /// Segment text. Concatenated verbatim after the host default
    /// prompt and project context, joined by blank lines.
    pub text: String,
}
```

- [ ] **Step 4: Extend `Contributions` with `prompt_segments`**

In `crates/savvagent-plugin/src/manifest.rs`, modify the `Contributions` struct definition. The struct is `#[non_exhaustive]` so callers must use `..Contributions::default()`; adding a field is safe.

Add the `use` line at the top of the file:

```rust
use crate::prompt::SystemPromptSegment;
```

And append the field inside the struct (after `tool_summaries`):

```rust
    /// System-prompt segments this plugin contributes. Composed into
    /// the model's `system` field after the host's default prompt and
    /// project context (see `savvagent-host::default_prompt`).
    pub prompt_segments: Vec<SystemPromptSegment>,

    /// Content renderers this plugin provides. Each spec declares the
    /// SPP `ContentBlock` type tag this plugin handles via
    /// `Plugin::create_renderer`.
    pub content_renderers: Vec<ContentRendererSpec>,
```

And add the new spec type below `ToolSummarySpec`:

```rust
/// Registration descriptor for a content-renderer contribution.
///
/// The plugin handles `ContentBlock` values whose `type` discriminator
/// matches `block_type`. Two plugins both claiming `canonical = true`
/// for the same block type is a startup error; non-canonical
/// contributions act as fallbacks (lower-priority).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentRendererSpec {
    /// SPP content block type tag, matching the `type` discriminator
    /// (`"html"` for `ContentBlock::Html`).
    pub block_type: String,
    /// `true` if this plugin claims to be the primary renderer for
    /// this block type. Exactly one plugin per block type should be
    /// canonical.
    pub canonical: bool,
}
```

Update the existing `default_contributions_is_empty` test to include the new field:

```rust
    #[test]
    fn default_contributions_is_empty() {
        let c = Contributions::default();
        assert!(c.slash_commands.is_empty());
        assert!(c.screens.is_empty());
        assert!(c.themes.is_empty());
        assert!(c.providers.is_empty());
        assert!(c.hooks.is_empty());
        assert!(c.slots.is_empty());
        assert!(c.keybindings.is_empty());
        assert!(c.tool_summaries.is_empty());
        assert!(c.prompt_segments.is_empty());
        assert!(c.content_renderers.is_empty());
    }
```

- [ ] **Step 5: Extend `SlashSpec` with `suppress_prompt_segments`**

Modify the `SlashSpec` struct in `crates/savvagent-plugin/src/manifest.rs`:

```rust
pub struct SlashSpec {
    /// Command name without the leading `/`.
    pub name: String,
    /// One-line summary shown in the command palette.
    pub summary: String,
    /// Optional usage hint shown in the command palette after the command name.
    pub args_hint: Option<String>,
    /// True if no-arg invocation is a usage error.
    pub requires_arg: bool,
    /// Prompt segment ids to drop from the system prompt when this
    /// slash is invoked. Empty = no suppression.
    pub suppress_prompt_segments: Vec<String>,
}
```

- [ ] **Step 6: Update every existing `SlashSpec` literal**

Adding a field to a non-`#[non_exhaustive]` struct breaks every existing literal. Search for `SlashSpec {` across the workspace and add `suppress_prompt_segments: vec![],` to each.

```bash
grep -rn "SlashSpec {" crates/ --include="*.rs"
```

For each match, add the new field. Most will be in `crates/savvagent/src/plugin/builtin/<plugin>/mod.rs`. Example pattern:

```rust
SlashSpec {
    name: "theme".into(),
    summary: "Pick a TUI theme".into(),
    args_hint: Some("[list | <slug>]".into()),
    requires_arg: false,
    suppress_prompt_segments: vec![],   // <-- add this
}
```

Also add the field to any test fixtures that construct `SlashSpec` literally.

- [ ] **Step 7: Wire `prompt.rs` into `lib.rs` + re-export the new spec**

In `crates/savvagent-plugin/src/lib.rs`:

```rust
/// System-prompt segment contributions.
pub mod prompt;
pub use prompt::SystemPromptSegment;
```

And update the `manifest` re-export to include `ContentRendererSpec`:

```rust
pub use manifest::{
    Contributions, ContentRendererSpec, KeyScope, KeybindingSpec, Manifest,
    PluginKind, ProviderSpec, ScreenLayout, ScreenSpec, SlashSpec, SlotSpec,
    ToolSummarySpec,
};
```

- [ ] **Step 8: Run the tests; verify they pass**

```bash
cargo test -p savvagent-plugin
```

Expected: all tests pass. If any `SlashSpec` literal was missed in Step 6, the build will fail with a `missing field 'suppress_prompt_segments'` error — go back and add it.

```bash
cargo build --workspace
```

Expected: PASS (no missed literals).

- [ ] **Step 9: Commit**

```bash
git add crates/savvagent-plugin/ crates/savvagent/
git commit -m "feat(plugin): SystemPromptSegment + ContentRendererSpec + slash suppression"
```

---

## Task 9: Plugin trait — `create_renderer` method + `Effect::OpenUrl`

**Files:**
- Modify: `crates/savvagent-plugin/src/plugin.rs`
- Modify: `crates/savvagent-plugin/src/effect.rs`
- Modify: `crates/savvagent-plugin/src/lib.rs`

Add the trait method (with a default that returns `ContentRendererNotFound`) and the new `Effect` variant.

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod trait_smoke` in `crates/savvagent-plugin/src/lib.rs`:

```rust
    #[tokio::test]
    async fn dummy_plugin_create_renderer_default_returns_not_found() {
        use crate::content::ContentBlockId;

        let p = DummyPlugin;
        let r = p.create_renderer("html", ContentBlockId(0), "<p>x</p>");
        assert!(
            matches!(r, Err(PluginError::ContentRendererNotFound(ref t)) if t == "html"),
            "default impl should return ContentRendererNotFound",
        );
    }

    #[test]
    fn effect_open_url_variants() {
        use crate::effect::{Effect, UrlTarget};
        let e = Effect::OpenUrl {
            url: "https://example.com".into(),
            target: UrlTarget::SystemBrowser,
        };
        match e {
            Effect::OpenUrl { url, target } => {
                assert_eq!(url, "https://example.com");
                assert_eq!(target, UrlTarget::SystemBrowser);
            }
            _ => panic!("expected OpenUrl"),
        }
    }
```

- [ ] **Step 2: Run the tests; verify they fail**

```bash
cargo test -p savvagent-plugin trait_smoke::dummy_plugin_create_renderer_default_returns_not_found trait_smoke::effect_open_url_variants
```

Expected: FAIL.

- [ ] **Step 3: Add the trait method**

In `crates/savvagent-plugin/src/plugin.rs`, add a new method to the `Plugin` trait (after `summarize_tool_result`):

```rust
    /// Construct a fresh `ContentRenderer` for an inline content block.
    /// Called when the conversation log encounters a `ContentBlock` whose
    /// `type` discriminator matches one of this plugin's
    /// `ContentRendererSpec`s. Each invocation produces a new
    /// instance — per-block state lives in the returned renderer.
    ///
    /// Default impl returns [`PluginError::ContentRendererNotFound`].
    fn create_renderer(
        &self,
        block_type: &str,
        id: crate::content::ContentBlockId,
        source: &str,
    ) -> Result<Box<dyn crate::content::ContentRenderer>, PluginError> {
        let _ = (id, source);
        Err(PluginError::ContentRendererNotFound(block_type.to_string()))
    }
```

- [ ] **Step 4: Add `Effect::OpenUrl` and `UrlTarget`**

In `crates/savvagent-plugin/src/effect.rs`, add to the existing `Effect` enum (preserve the non-exhaustive marker if it's there):

```rust
    /// Open a URL. The host shells to `xdg-open` (Linux), `open` (macOS),
    /// or `start` (Windows) when `target == SystemBrowser`. When
    /// `target == ContinueConversation`, the host treats the URL as a
    /// follow-up user prompt instead.
    OpenUrl {
        /// Absolute URL. Plugins MUST validate this before emitting;
        /// the host treats untrusted URLs as a security risk.
        url: String,
        /// Where the URL should be opened.
        target: UrlTarget,
    },
```

And define `UrlTarget` (at the bottom of the file or wherever sibling enums live):

```rust
/// Destination for an [`Effect::OpenUrl`] effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlTarget {
    /// Open in the user's default system browser via
    /// `xdg-open`/`open`/`start`.
    SystemBrowser,
    /// Send the URL as a new user prompt in the active conversation
    /// (useful for relative paths the model means as
    /// "look at this file").
    ContinueConversation,
}
```

- [ ] **Step 5: Re-export `UrlTarget`**

In `crates/savvagent-plugin/src/lib.rs`, update the `effect` re-export:

```rust
pub use effect::{BoundAction, Effect, UrlTarget};
```

- [ ] **Step 6: Run the tests; verify they pass**

```bash
cargo test -p savvagent-plugin
```

Expected: PASS.

- [ ] **Step 7: Run the full workspace tests**

```bash
cargo test --workspace
```

Expected: PASS. The new trait method has a default impl so existing plugin implementations still compile. The new `Effect` variant requires anyone match-ing on `Effect` non-exhaustively to add a branch — `crates/savvagent/src/plugin/effects.rs` is the consumer; Task 12 handles its update. For now, the build may emit a "non-exhaustive match" warning in `effects.rs` — that's expected and gets resolved in Task 12.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent-plugin/
git commit -m "feat(plugin): Plugin::create_renderer + Effect::OpenUrl"
```

---

## Task 10: Host — compose prompt segments from active plugins

**Files:**
- Modify: `crates/savvagent-host/src/default_prompt.rs`
- Modify: `crates/savvagent-host/src/session.rs`
- Modify: `crates/savvagent-host/Cargo.toml`

The host already builds a default system prompt in `default_prompt.rs`. Add a small append-step: take a slice of `SystemPromptSegment`s plus an optional set of suppressed ids, filter, concatenate the survivors, and append after the existing conventions section.

The session (which owns the `Host` per turn) gathers the active-plugin segments + the per-slash suppression list and calls the new function.

- [ ] **Step 1: Add `savvagent-plugin` as a workspace dep for `savvagent-host`**

In `crates/savvagent-host/Cargo.toml`:

```toml
[dependencies]
# ... existing entries ...
savvagent-plugin = { workspace = true }
```

- [ ] **Step 2: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `crates/savvagent-host/src/default_prompt.rs`:

```rust
    #[test]
    fn append_prompt_segments_concatenates_in_order() {
        use savvagent_plugin::SystemPromptSegment;

        let base = "Default prompt body.\n";
        let segments = vec![
            SystemPromptSegment {
                id: "internal:a:default".into(),
                text: "A segment.".into(),
            },
            SystemPromptSegment {
                id: "internal:b:default".into(),
                text: "B segment.".into(),
            },
        ];
        let suppressed: &[&str] = &[];
        let out = append_prompt_segments(base, &segments, suppressed);
        let lines: Vec<&str> = out.split("\n\n").collect();
        assert!(out.starts_with(base), "base must be the prefix");
        assert!(lines.iter().any(|l| l.trim() == "A segment."));
        assert!(lines.iter().any(|l| l.trim() == "B segment."));
        // Order must match input order.
        let a_pos = out.find("A segment.").unwrap();
        let b_pos = out.find("B segment.").unwrap();
        assert!(a_pos < b_pos);
    }

    #[test]
    fn append_prompt_segments_honors_suppression() {
        use savvagent_plugin::SystemPromptSegment;

        let base = "Default.\n";
        let segments = vec![
            SystemPromptSegment {
                id: "internal:keep:s".into(),
                text: "KEEP".into(),
            },
            SystemPromptSegment {
                id: "internal:drop:s".into(),
                text: "DROP".into(),
            },
        ];
        let suppressed = &["internal:drop:s"];
        let out = append_prompt_segments(base, &segments, suppressed);
        assert!(out.contains("KEEP"));
        assert!(!out.contains("DROP"), "suppressed segment must not appear");
    }
```

- [ ] **Step 3: Run the tests; verify they fail**

```bash
cargo test -p savvagent-host default_prompt::tests::append_prompt_segments
```

Expected: FAIL with `cannot find function 'append_prompt_segments'`.

- [ ] **Step 4: Implement `append_prompt_segments`**

In `crates/savvagent-host/src/default_prompt.rs`, add the function near the existing prompt-rendering helpers (and a re-export at module root if the file structure prefers that):

```rust
use savvagent_plugin::SystemPromptSegment;

/// Append `segments` to a base prompt, dropping any segment whose `id`
/// appears in `suppressed_ids`. Survivors are concatenated in input
/// order, joined to the base by a blank line, and joined to each other
/// by blank lines.
///
/// The host uses this after composing the default prompt + project
/// context (`SAVVAGENT.md`) and before sending the `system` field on a
/// `CompleteRequest`. Per-slash suppression lists come from the
/// invoked `SlashSpec::suppress_prompt_segments`.
pub fn append_prompt_segments(
    base: &str,
    segments: &[SystemPromptSegment],
    suppressed_ids: &[&str],
) -> String {
    let mut out = String::with_capacity(
        base.len() + segments.iter().map(|s| s.text.len() + 2).sum::<usize>(),
    );
    out.push_str(base);

    for seg in segments {
        if suppressed_ids.iter().any(|s| *s == seg.id) {
            continue;
        }
        if !out.ends_with("\n\n") {
            if out.ends_with('\n') {
                out.push('\n');
            } else {
                out.push_str("\n\n");
            }
        }
        out.push_str(&seg.text);
    }
    out
}
```

- [ ] **Step 5: Run the tests; verify they pass**

```bash
cargo test -p savvagent-host default_prompt::tests::append_prompt_segments
```

Expected: both tests pass.

- [ ] **Step 6: Wire into `session.rs`**

In `crates/savvagent-host/src/session.rs`, locate the place that constructs the `CompleteRequest`'s `system` field for each turn. (Search for `default_prompt::` or `system:` to find it.)

The current code probably looks roughly like:

```rust
let system = build_default_prompt(...);
let req = CompleteRequest { system: Some(system), ... };
```

Change it to:

```rust
let base_prompt = build_default_prompt(...);
let segments = self.active_prompt_segments();    // see Step 7
let suppressed = self.suppressed_segments_for_turn();  // see Step 7
let suppressed_refs: Vec<&str> = suppressed.iter().map(|s| s.as_str()).collect();
let system = append_prompt_segments(&base_prompt, &segments, &suppressed_refs);
let req = CompleteRequest { system: Some(system), ... };
```

- [ ] **Step 7: Plumb the segments + suppression list through `Host`**

Add two new fields to the `Host` struct (or wherever per-conversation state lives):

```rust
pub struct Host {
    // ... existing ...
    pub(crate) prompt_segments: Vec<SystemPromptSegment>,
    pub(crate) pending_slash_suppression: Vec<String>,
}
```

Add setter methods:

```rust
impl Host {
    /// Replace the active prompt segments. Called by the TUI runtime
    /// each time the enabled-plugin set changes.
    pub fn set_prompt_segments(&mut self, segments: Vec<SystemPromptSegment>) {
        self.prompt_segments = segments;
    }

    /// Set the suppression list for the next turn. Called by the slash
    /// dispatcher *before* invoking `run_turn_streaming` when the
    /// dispatched slash has a non-empty `suppress_prompt_segments`.
    /// Cleared automatically after the turn completes.
    pub fn set_turn_suppression(&mut self, suppressed: Vec<String>) {
        self.pending_slash_suppression = suppressed;
    }

    pub(crate) fn active_prompt_segments(&self) -> Vec<SystemPromptSegment> {
        self.prompt_segments.clone()
    }

    pub(crate) fn suppressed_segments_for_turn(&self) -> Vec<String> {
        self.pending_slash_suppression.clone()
    }
}
```

Clear `pending_slash_suppression` at the end of `run_turn_streaming`:

```rust
// At the end of run_turn_streaming, before returning:
self.pending_slash_suppression.clear();
```

- [ ] **Step 8: Run the full workspace tests**

```bash
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/savvagent-host/
git commit -m "feat(host): compose plugin prompt segments + per-slash suppression"
```

---

## Task 11: `savvagent-canvas` crate — skeleton + Blitz integration

**Files:**
- Create: `crates/savvagent-canvas/Cargo.toml`
- Create: `crates/savvagent-canvas/src/lib.rs`
- Create: `crates/savvagent-canvas/src/canvas.rs`
- Modify: `Cargo.toml` (workspace) — add member + workspace deps

Implement `HtmlCanvas` against the Blitz API documented in the Phase 0 spike. The crate exposes the `HtmlCanvas` struct (impls `ContentRenderer`) and nothing else (the plugin shim lives in `savvagent` per Task 13).

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`:

```toml
[workspace]
members = [
    # ... existing ...
    "crates/savvagent-canvas",
]

[workspace.dependencies]
# ... existing ...
savvagent-canvas = { path = "crates/savvagent-canvas", version = "0.17.0" }
blitz = "<pinned-from-Phase-0-spike>"
```

(Substitute the pinned Blitz version from the Task 1 spike output.)

- [ ] **Step 2: Create the crate skeleton**

```bash
mkdir -p crates/savvagent-canvas/src
```

Create `crates/savvagent-canvas/Cargo.toml`:

```toml
[package]
name = "savvagent-canvas"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true
description = "HTML canvas renderer for savvagent inline conversation rendering"

[dependencies]
savvagent-plugin = { workspace = true }
async-trait = { workspace = true }
tracing = { workspace = true }
blitz = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt"] }
```

Create `crates/savvagent-canvas/src/lib.rs`:

```rust
//! Inline HTML canvas renderer for savvagent.
//!
//! Wraps Blitz to expose a [`HtmlCanvas`] implementing
//! [`savvagent_plugin::ContentRenderer`]. Phase 1 implements only
//! `render`; the eventing surface lands in Phase 2.
//!
//! See `docs/superpowers/specs/2026-05-21-inline-html-canvas-design.md`.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(missing_debug_implementations)]
#![warn(missing_docs)]

mod canvas;
mod subset;

pub use canvas::HtmlCanvas;
```

- [ ] **Step 3: Write the failing test**

Create `crates/savvagent-canvas/src/canvas.rs` with a `#[cfg(test)]` test that exercises construction + render:

```rust
//! `HtmlCanvas` — the static-rendering implementation of
//! `ContentRenderer` for SPP `ContentBlock::Html`.

use async_trait::async_trait;
use savvagent_plugin::{
    ContentBlockId, ContentRenderer, Frame, PixelFormat, PixelSize,
};

/// Static HTML canvas renderer. Phase 1: render-only; Phase 2 adds
/// event dispatch + focus + freeze/thaw.
#[derive(Debug)]
pub struct HtmlCanvas {
    id: ContentBlockId,
    source: String,
    // Blitz instance fields go here. Concrete shape determined by the
    // Phase 0 spike output. Possible shape (substitute the actual
    // Blitz types):
    //
    //   document: blitz::Document,
    //   renderer: blitz::Renderer,
}

impl HtmlCanvas {
    /// Construct a canvas from HTML source.
    pub fn new(id: ContentBlockId, source: &str) -> Self {
        // Validate the source against the savvagent-canvas subset and
        // log warnings for out-of-subset elements. Then build the
        // Blitz document.
        crate::subset::validate(source);
        Self {
            id,
            source: source.to_string(),
            // Initialize Blitz fields here per the spike's findings.
        }
    }
}

#[async_trait]
impl ContentRenderer for HtmlCanvas {
    fn id(&self) -> ContentBlockId {
        self.id
    }

    fn render(&mut self, size: PixelSize) -> Frame {
        // Drive Blitz to lay out `self.source` at `size.width`, paint
        // to an RGBA buffer, and return it.
        //
        // The exact API calls depend on the spike's findings. Aim:
        //
        //   1. (Re)build the Blitz document from `self.source` if not
        //      already built.
        //   2. Set viewport width to `size.width`.
        //   3. Layout.
        //   4. Paint to a Vec<u8> of length width*natural_height*4.
        //   5. Return Frame { width, height: natural_height, format:
        //      Rgba8, bytes }.
        let _ = size;
        todo!("populate from Blitz API per Phase 0 spike");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY_HTML: &str = "<!doctype html><body style='margin:0'>\
                             <div style='width:32px;height:16px;\
                             background:#ff0000'></div></body>";

    #[test]
    fn canvas_renders_at_requested_width() {
        let mut c = HtmlCanvas::new(ContentBlockId(7), TINY_HTML);
        let frame = c.render(PixelSize {
            width: 64,
            height: 0,  // 0 means "natural height"
        });
        assert_eq!(frame.format, PixelFormat::Rgba8);
        assert_eq!(frame.width, 64);
        assert!(frame.height > 0);
        assert_eq!(
            frame.bytes.len() as u32,
            frame.width * frame.height * 4,
            "Rgba8 byte count must match width*height*4",
        );
    }

    #[test]
    fn canvas_id_round_trips() {
        let c = HtmlCanvas::new(ContentBlockId(42), TINY_HTML);
        assert_eq!(c.id(), ContentBlockId(42));
    }
}
```

(The `todo!()` in `render` is the spike-driven implementation. Step 4 fills it in.)

- [ ] **Step 4: Implement `render` using the Blitz API from the spike**

Replace the `todo!()` in `HtmlCanvas::render` with concrete Blitz calls. The exact code depends on the spike's findings; the contract is:

- Input `size.width` is the requested pixel width (always > 0).
- Output `Frame::width == size.width`.
- Output `Frame::height` is the document's natural height at that width.
- Output `Frame::format == PixelFormat::Rgba8`.
- Output `Frame::bytes` is exactly `width * height * 4` bytes.

If the Blitz API ergonomically yields BGRA, convert before returning (or swap to `PixelFormat::Bgra8` and let consumers handle it — Rgba8 is the documented default for the spec's contract; prefer Rgba8 here).

- [ ] **Step 5: Run the tests; verify they pass**

```bash
cargo test -p savvagent-canvas
```

Expected: both tests pass.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/savvagent-canvas/
git commit -m "feat(canvas): HtmlCanvas Blitz-backed ContentRenderer (static render)"
```

---

## Task 12: `savvagent-canvas` — subset validator

**Files:**
- Create: `crates/savvagent-canvas/src/subset.rs`

A lightweight DOM walker that emits `tracing::warn!` for elements / attributes / properties outside the spec's HTML+CSS subset. Not a render error — the canvas still draws. The validator's value is debugging model output during development.

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent-canvas/src/subset.rs`:

```rust
//! Subset validator: walks parsed HTML and emits tracing warnings for
//! elements or attributes outside the savvagent-canvas supported set.
//!
//! Not a render error — Blitz renders what it can; the validator's
//! purpose is to surface "you used <iframe>; that's not in the
//! subset" warnings during development.

/// Validate `source` and emit `tracing::warn!` for anything outside
/// the documented subset. Returns the count of warnings emitted.
pub fn validate(source: &str) -> usize {
    let mut warnings = 0;
    // Light tokenizer (not full HTML parsing): just look for the
    // bracketed tag names. Good enough for surfacing the obvious
    // violations like <script>, <iframe>, <video>, <audio>, <embed>,
    // <object>, <canvas>, external <link rel="stylesheet">, etc.
    for tag in EXCLUDED_TAGS {
        let needle = format!("<{}", tag);
        if source.to_ascii_lowercase().contains(&needle) {
            warnings += 1;
            tracing::warn!(
                tag,
                "savvagent-canvas: <{}> is outside the subset; will not render \
                 as intended",
                tag,
            );
        }
    }
    if source.contains("rel=\"stylesheet\"") || source.contains("rel='stylesheet'") {
        warnings += 1;
        tracing::warn!(
            "savvagent-canvas: external stylesheets are not loaded; \
             use a <style> block instead"
        );
    }
    warnings
}

const EXCLUDED_TAGS: &[&str] = &[
    "script",
    "iframe",
    "object",
    "embed",
    "video",
    "audio",
    "canvas",  // HTML <canvas>, NOT the savvagent canvas concept
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_html_emits_no_warnings() {
        let n = validate("<!doctype html><body><h1>x</h1></body>");
        assert_eq!(n, 0);
    }

    #[test]
    fn script_tag_warns() {
        let n = validate("<!doctype html><body><script>alert(1)</script></body>");
        assert_eq!(n, 1);
    }

    #[test]
    fn external_stylesheet_warns() {
        let n = validate(
            "<!doctype html><head>\
             <link rel=\"stylesheet\" href=\"x.css\"></head><body></body>",
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn multiple_violations_count_independently() {
        let n = validate(
            "<!doctype html><body><script>x</script><iframe src='y'></iframe></body>",
        );
        assert_eq!(n, 2);
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p savvagent-canvas subset
```

Expected: all four tests pass (this is a one-shot implementation; no separate fail-then-pass cycle).

- [ ] **Step 3: Verify `HtmlCanvas::new` already calls `validate`**

The body of `HtmlCanvas::new` from Task 11 already includes `crate::subset::validate(source);`. Confirm with:

```bash
grep -n "subset::validate" crates/savvagent-canvas/src/canvas.rs
```

Expected: one match in `HtmlCanvas::new`.

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent-canvas/src/subset.rs
git commit -m "feat(canvas): subset validator with tracing warnings"
```

---

## Task 13: `internal:html-canvas` built-in plugin

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/html_canvas/mod.rs`
- Create: `crates/savvagent/src/plugin/builtin/html_canvas/plugin.rs`
- Create: `crates/savvagent/src/plugin/builtin/html_canvas/prompt_text.rs`
- Modify: `crates/savvagent/src/plugin/builtin/mod.rs`
- Modify: `crates/savvagent/src/plugin/mod.rs` — register in `register_builtins()`
- Modify: `crates/savvagent/Cargo.toml` — add `savvagent-canvas` dep

The plugin owns:
- The `internal:html-canvas:default` `SystemPromptSegment`.
- The `ContentRendererSpec { block_type: "html", canonical: true }`.
- `Plugin::create_renderer` returning `Box<HtmlCanvas>`.

`/save-canvas` and auto-export land in later tasks (15 and 16); this task only wires the renderer factory + prompt segment.

- [ ] **Step 1: Add `savvagent-canvas` to the TUI crate's deps**

In `crates/savvagent/Cargo.toml`:

```toml
[dependencies]
# ... existing ...
savvagent-canvas = { workspace = true }
```

- [ ] **Step 2: Create the plugin module**

```bash
mkdir -p crates/savvagent/src/plugin/builtin/html_canvas
```

Create `crates/savvagent/src/plugin/builtin/html_canvas/mod.rs`:

```rust
//! `internal:html-canvas` built-in plugin.
//!
//! Contributes:
//! - A SystemPromptSegment that tells the model to wrap structured
//!   documents in ```html-canvas fences.
//! - A ContentRendererSpec claiming the SPP "html" block type as
//!   canonical.
//! - The Plugin::create_renderer factory returning a fresh
//!   savvagent_canvas::HtmlCanvas per inline block.
//!
//! Phase 2 will add the OnFocusedCanvas keybinding for Ctrl-O
//! (open-in-browser); Phase 1 doesn't ship interactive bindings.

mod plugin;
mod prompt_text;

pub use plugin::HtmlCanvasPlugin;
```

Create `crates/savvagent/src/plugin/builtin/html_canvas/prompt_text.rs`:

```rust
//! Default system prompt segment text for internal:html-canvas.

/// The default segment text. Kept in its own file so it's easy to
/// review and i18n later.
pub const DEFAULT_PROMPT_TEXT: &str = "\
When responding to the user with a structured document — plan, spec, \
status update, design review, comparison table, anything where visual \
hierarchy and scannability matter — prefer HTML over markdown. Wrap \
the HTML in a ```html-canvas fenced block. The user's terminal renders \
it inline as a document.\n\
\n\
For code samples, terse replies, error messages, or output destined for \
another system (commit messages, PR comments, files on disk), use plain \
text or markdown — those are not rendered as canvases.\n\
\n\
Supported tags: <h1>-<h6>, <p>, <ul>, <ol>, <li>, <dl>, <dt>, <dd>, \
<table>, <thead>, <tbody>, <tr>, <th>, <td>, <pre>, <code>, <a>, <em>, \
<strong>, <mark>, <kbd>, <details>, <summary>, <blockquote>, <hr>, \
<section>, <header>, <footer>, <figure>, <figcaption>, <img> (data: URIs only), \
<svg>.\n\
\n\
Use a <style> block in the document head; do not link external \
stylesheets. Do not include <script> tags. Do not reference external \
fonts. Use only data: URIs for images. Do not use <iframe>, <video>, \
<audio>, <embed>, <object>, or <canvas>.\
";

/// Stable id for the segment. Matches the convention
/// `<plugin_id>:<segment_name>`.
pub const DEFAULT_PROMPT_ID: &str = "internal:html-canvas:default";
```

- [ ] **Step 3: Create the `Plugin` impl**

Create `crates/savvagent/src/plugin/builtin/html_canvas/plugin.rs`:

```rust
use async_trait::async_trait;
use savvagent_canvas::HtmlCanvas;
use savvagent_plugin::{
    Contributions, ContentBlockId, ContentRenderer, ContentRendererSpec,
    Manifest, Plugin, PluginError, PluginId, PluginKind, SystemPromptSegment,
};

use super::prompt_text::{DEFAULT_PROMPT_ID, DEFAULT_PROMPT_TEXT};

/// `internal:html-canvas` plugin. Constructed by `register_builtins`.
#[derive(Debug, Default)]
pub struct HtmlCanvasPlugin;

#[async_trait]
impl Plugin for HtmlCanvasPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: PluginId("internal:html-canvas".to_string()),
            name: "HTML canvas".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            description: "Renders model-authored HTML inline as a static canvas."
                .to_string(),
            kind: PluginKind::Optional,
            contributions: Contributions {
                content_renderers: vec![ContentRendererSpec {
                    block_type: "html".to_string(),
                    canonical: true,
                }],
                prompt_segments: vec![SystemPromptSegment {
                    id: DEFAULT_PROMPT_ID.to_string(),
                    text: DEFAULT_PROMPT_TEXT.to_string(),
                }],
                ..Contributions::default()
            },
        }
    }

    fn create_renderer(
        &self,
        block_type: &str,
        id: ContentBlockId,
        source: &str,
    ) -> Result<Box<dyn ContentRenderer>, PluginError> {
        match block_type {
            "html" => Ok(Box::new(HtmlCanvas::new(id, source))),
            other => Err(PluginError::ContentRendererNotFound(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_advertises_html_renderer_and_prompt_segment() {
        let p = HtmlCanvasPlugin;
        let m = p.manifest();
        assert_eq!(m.id, PluginId("internal:html-canvas".to_string()));
        assert_eq!(m.kind, PluginKind::Optional);
        assert_eq!(m.contributions.content_renderers.len(), 1);
        assert_eq!(m.contributions.content_renderers[0].block_type, "html");
        assert!(m.contributions.content_renderers[0].canonical);
        assert_eq!(m.contributions.prompt_segments.len(), 1);
        assert_eq!(
            m.contributions.prompt_segments[0].id,
            "internal:html-canvas:default"
        );
    }

    #[test]
    fn create_renderer_returns_canvas_for_html_block() {
        let p = HtmlCanvasPlugin;
        let r = p.create_renderer("html", ContentBlockId(0), "<p>x</p>");
        assert!(r.is_ok());
    }

    #[test]
    fn create_renderer_rejects_unknown_block_type() {
        let p = HtmlCanvasPlugin;
        let r = p.create_renderer("svg", ContentBlockId(0), "");
        assert!(matches!(
            r,
            Err(PluginError::ContentRendererNotFound(ref t)) if t == "svg"
        ));
    }
}
```

- [ ] **Step 4: Add to `builtin/mod.rs`**

In `crates/savvagent/src/plugin/builtin/mod.rs`, add:

```rust
pub mod html_canvas;
```

- [ ] **Step 5: Register in `register_builtins()`**

In `crates/savvagent/src/plugin/mod.rs`, find the `register_builtins()` function. Add an entry to its returned `BuiltinSet`:

```rust
use crate::plugin::builtin::html_canvas::HtmlCanvasPlugin;

pub(crate) fn register_builtins() -> BuiltinSet {
    BuiltinSet {
        // ... existing plugins ...
        html_canvas: Box::new(HtmlCanvasPlugin::default()),
    }
}
```

Add a corresponding field to `BuiltinSet`:

```rust
pub(crate) struct BuiltinSet {
    // ... existing fields ...
    pub html_canvas: Box<dyn Plugin>,
}
```

Update any pattern that iterates `BuiltinSet` (e.g., `into_iter` in `registry.rs`) to include `html_canvas` in the list.

- [ ] **Step 6: Run the plugin's tests**

```bash
cargo test -p savvagent plugin::builtin::html_canvas
```

Expected: PASS.

- [ ] **Step 7: Run the full workspace tests**

```bash
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent/
git commit -m "feat(plugin/html-canvas): internal:html-canvas built-in plugin"
```

---

## Task 14: Plugin registry — `content_renderers` index + prompt-segment gathering

**Files:**
- Modify: `crates/savvagent/src/plugin/registry.rs`
- Modify: `crates/savvagent/src/plugin/manifests.rs`
- Modify: `crates/savvagent/src/plugin/effects.rs`

Add the lookup index `block_type → PluginId` for content renderers, plus a helper that gathers all enabled plugins' `SystemPromptSegment`s in registration order. Also handle the new `Effect::OpenUrl` variant in the effects applier.

- [ ] **Step 1: Write the failing test**

In `crates/savvagent/src/plugin/registry.rs`, append a test (or extend the existing tests module):

```rust
    #[test]
    fn content_renderers_index_routes_block_type_to_plugin() {
        let registry = build_registry_with_html_canvas();
        let plugin = registry.content_renderer_for("html");
        assert_eq!(
            plugin.map(|p| p.0.clone()),
            Some("internal:html-canvas".to_string())
        );
    }

    #[test]
    fn unknown_block_type_returns_none() {
        let registry = build_registry_with_html_canvas();
        assert!(registry.content_renderer_for("xml").is_none());
    }

    #[test]
    fn active_prompt_segments_concatenates_enabled_plugins() {
        let registry = build_registry_with_html_canvas();
        let segments = registry.active_prompt_segments();
        assert!(segments.iter().any(|s| s.id == "internal:html-canvas:default"));
    }

    // Helper: build a PluginRegistry with html_canvas enabled.
    fn build_registry_with_html_canvas() -> PluginRegistry {
        // Match the existing helper pattern in this module; if none
        // exists, build minimally:
        //
        //   let mut r = PluginRegistry::new();
        //   r.add(Box::new(HtmlCanvasPlugin::default()), true);
        //   r.rebuild_indexes();
        //   r
        todo!("match the existing test helper shape");
    }
```

- [ ] **Step 2: Run the tests; verify they fail**

```bash
cargo test -p savvagent plugin::registry::tests::content_renderers_index_routes_block_type_to_plugin
```

Expected: FAIL.

- [ ] **Step 3: Extend `Indexes`**

In `crates/savvagent/src/plugin/manifests.rs`, find the `Indexes` struct (or its equivalent for `PluginRegistry`). Add the new index:

```rust
pub struct Indexes {
    // ... existing fields ...
    /// SPP content block type → owning plugin.
    pub content_renderers: HashMap<String, PluginId>,
}
```

When `Indexes` is built (during registry init / rebuild), populate the new field:

```rust
for plugin in enabled_plugins {
    let manifest = plugin.manifest();
    for spec in manifest.contributions.content_renderers {
        if !spec.canonical {
            continue;
        }
        match indexes.content_renderers.entry(spec.block_type.clone()) {
            Entry::Occupied(existing) => {
                return Err(IndexError::DuplicateCanonicalRenderer {
                    block_type: spec.block_type,
                    existing: existing.get().clone(),
                    duplicate: manifest.id.clone(),
                });
            }
            Entry::Vacant(slot) => {
                slot.insert(manifest.id.clone());
            }
        }
    }
}
```

Add the new error variant to whatever error enum `manifests.rs` uses for index-build errors.

- [ ] **Step 4: Add `PluginRegistry::content_renderer_for` and `active_prompt_segments`**

In `crates/savvagent/src/plugin/registry.rs`:

```rust
impl PluginRegistry {
    /// Look up the plugin id that owns the canonical content renderer
    /// for `block_type`.
    pub fn content_renderer_for(&self, block_type: &str) -> Option<&PluginId> {
        self.indexes.content_renderers.get(block_type)
    }

    /// All enabled plugins' system prompt segments in registration order.
    /// Used by the TUI to call `Host::set_prompt_segments` on startup
    /// and whenever the enabled set changes.
    pub fn active_prompt_segments(&self) -> Vec<SystemPromptSegment> {
        let mut out = Vec::new();
        for plugin_id in self.enabled_plugins_in_order() {
            if let Some(plugin) = self.get(plugin_id) {
                let m = plugin.manifest();
                out.extend(m.contributions.prompt_segments);
            }
        }
        out
    }
}
```

`enabled_plugins_in_order` and `get` are existing helpers — adapt to whatever they're already named.

- [ ] **Step 5: Handle `Effect::OpenUrl` in the effects applier**

In `crates/savvagent/src/plugin/effects.rs`, find the match expression that dispatches `Effect` variants. Add a branch:

```rust
        Effect::OpenUrl { url, target } => {
            apply_open_url(app, url, target)
        }
```

Define `apply_open_url`:

```rust
fn apply_open_url(app: &mut App, url: String, target: UrlTarget) {
    match target {
        UrlTarget::SystemBrowser => {
            // Shell to xdg-open (Linux) / open (macOS) / start (Windows).
            // Spawn detached and don't wait.
            let cmd = if cfg!(target_os = "macos") {
                "open"
            } else if cfg!(target_os = "windows") {
                "start"
            } else {
                "xdg-open"
            };
            if let Err(err) = std::process::Command::new(cmd)
                .arg(&url)
                .spawn()
            {
                tracing::warn!(?err, %url, "failed to open url");
                app.push_styled_note(/* styled-line: "Failed to open URL" */);
            }
        }
        UrlTarget::ContinueConversation => {
            // Send the URL as the next user prompt.
            app.submit_prompt(url);
        }
    }
}
```

Adapt `App::push_styled_note` / `submit_prompt` to whatever the existing API names are.

- [ ] **Step 6: Implement the test helper**

Replace the `todo!()` in the test helper from Step 1 with a real helper that constructs a `PluginRegistry` containing only the `HtmlCanvasPlugin`. Mirror an existing test helper if one exists.

- [ ] **Step 7: Run the tests; verify they pass**

```bash
cargo test -p savvagent plugin::registry
```

Expected: PASS.

```bash
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent/src/plugin/
git commit -m "feat(plugin): content_renderers index + prompt-segment gathering + OpenUrl"
```

---

## Task 15: TUI — `Entry::Canvas` variant + `CanvasRegistry`

**Files:**
- Modify: `crates/savvagent/src/app.rs`
- Modify: `crates/savvagent/Cargo.toml` — add `ratatui-image` dep + workspace dep entry
- Modify: `Cargo.toml` (workspace)

Extend the existing `Entry` enum with a `Canvas` variant carrying a `ContentBlockId` plus the source (so transcripts can persist the source and re-render on resume). Add a `CanvasRegistry` field to `App` that holds the live renderer instances keyed by `ContentBlockId`.

- [ ] **Step 1: Add `ratatui-image` workspace dep**

In the root `Cargo.toml`:

```toml
[workspace.dependencies]
# ... existing ...
ratatui-image = "<latest-stable-version>"
```

Choose the latest published version compatible with the ratatui version the workspace already uses (check `Cargo.lock` or `Cargo.toml` for the current `ratatui` pin). Document the version in the PR description.

In `crates/savvagent/Cargo.toml`:

```toml
[dependencies]
# ... existing ...
ratatui-image = { workspace = true }
```

- [ ] **Step 2: Write the failing test**

In `crates/savvagent/src/app.rs`, append to the existing test module:

```rust
    #[test]
    fn entry_carries_canvas_variant() {
        let e = Entry::Canvas {
            id: savvagent_plugin::ContentBlockId(7),
            source: "<p>hi</p>".into(),
            source_preview: None,
        };
        match e {
            Entry::Canvas { id, source, source_preview } => {
                assert_eq!(id, savvagent_plugin::ContentBlockId(7));
                assert_eq!(source, "<p>hi</p>");
                assert!(source_preview.is_none());
            }
            _ => panic!("expected Canvas"),
        }
    }
```

- [ ] **Step 3: Run the test; verify it fails**

```bash
cargo test -p savvagent app::tests::entry_carries_canvas_variant
```

Expected: FAIL.

- [ ] **Step 4: Add the variant**

Find the existing `Entry` enum in `crates/savvagent/src/app.rs` (line ~212). Add a new variant:

```rust
    /// A model-emitted HTML block to be rendered inline as a canvas.
    /// `source_preview` is `Some(...)` while the block is still
    /// streaming (each `HtmlSourceDelta` appends to it); on
    /// `ContentBlockStop` the host promotes the preview into `source`
    /// and sets `source_preview` back to `None`. The renderer instance
    /// lives in `App::canvas_registry`.
    Canvas {
        /// Host-assigned id, matching the renderer key in the
        /// canvas registry.
        id: savvagent_plugin::ContentBlockId,
        /// Final HTML source (after streaming completes).
        source: String,
        /// In-flight source buffer during streaming, swapped to
        /// `source` and reset to `None` on completion.
        source_preview: Option<String>,
    },
```

- [ ] **Step 5: Add `CanvasRegistry` to `App`**

Above the `App` struct definition in `app.rs`, add:

```rust
use std::collections::HashMap;
use savvagent_plugin::{ContentBlockId, ContentRenderer};

/// Lives inside `App`. Owns one renderer per live canvas block.
pub(crate) struct CanvasRegistry {
    next_id: u32,
    renderers: HashMap<ContentBlockId, Box<dyn ContentRenderer>>,
    image_picker: Option<ratatui_image::Picker>,
    image_states: HashMap<ContentBlockId, ratatui_image::protocol::StatefulProtocol>,
}

impl CanvasRegistry {
    pub fn new() -> Self {
        Self {
            next_id: 0,
            renderers: HashMap::new(),
            image_picker: ratatui_image::Picker::from_query_stdio().ok(),
            image_states: HashMap::new(),
        }
    }

    /// Allocate a fresh `ContentBlockId` for a newly-arrived canvas.
    pub fn allocate_id(&mut self) -> ContentBlockId {
        let id = ContentBlockId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Insert a renderer instance for an id.
    pub fn insert(
        &mut self,
        id: ContentBlockId,
        renderer: Box<dyn ContentRenderer>,
    ) {
        self.renderers.insert(id, renderer);
    }

    /// Look up the renderer for `id`.
    pub fn get_mut(
        &mut self,
        id: ContentBlockId,
    ) -> Option<&mut Box<dyn ContentRenderer>> {
        self.renderers.get_mut(&id)
    }

    /// `true` iff this terminal supports an image protocol.
    pub fn image_protocol_available(&self) -> bool {
        self.image_picker.is_some()
    }
}

impl std::fmt::Debug for CanvasRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasRegistry")
            .field("next_id", &self.next_id)
            .field("renderer_count", &self.renderers.len())
            .field("image_protocol", &self.image_protocol_available())
            .finish_non_exhaustive()
    }
}
```

Then add the field to the existing `App` struct:

```rust
pub struct App {
    // ... existing fields ...
    pub(crate) canvas_registry: CanvasRegistry,
}
```

And initialize in `App::new` (or wherever `App` is constructed):

```rust
impl App {
    pub fn new(/* existing args */) -> Self {
        Self {
            // ... existing ...
            canvas_registry: CanvasRegistry::new(),
        }
    }
}
```

- [ ] **Step 6: Persist `Canvas` entries in transcripts**

The transcript persistence path serializes `App.entries: Vec<Entry>` to JSON. Adding a new variant requires updating the on-disk serde shape. The new variant should serialize to:

```json
{"type":"canvas","id":7,"source":"<!doctype...","source_preview":null}
```

If `Entry` already derives `Serialize`/`Deserialize` with a `#[serde(tag = "type", rename_all = "snake_case")]`, the new variant just works. Verify by adding a serialization round-trip test:

```rust
    #[test]
    fn canvas_entry_persists_to_json() {
        let e = Entry::Canvas {
            id: ContentBlockId(3),
            source: "<p>x</p>".into(),
            source_preview: None,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "canvas");
        assert_eq!(v["source"], "<p>x</p>");
        let back: Entry = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
    }
```

If `Entry` doesn't already use derive_serde, the existing serialization path probably matches manually — adapt the new variant to that path. If `ContentBlockId` doesn't impl `Serialize`/`Deserialize`, derive them in `crates/savvagent-plugin/src/content.rs` (additive, WIT-portable as `u32`).

- [ ] **Step 7: Run the tests; verify they pass**

```bash
cargo test -p savvagent app
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/savvagent/
git commit -m "feat(tui): Entry::Canvas + CanvasRegistry holding renderers"
```

---

## Task 16: TUI — render `Entry::Canvas` via `ratatui-image`

**Files:**
- Modify: `crates/savvagent/src/ui.rs`

Walk the transcript items; for `Entry::Canvas`, ask the registry's renderer to produce a `Frame`, hand the pixels to `ratatui-image::protocol::StatefulProtocol`, and draw it at the computed cell rect. For terminals without an image protocol, render the source as a syntax-highlighted code block with a one-line banner.

- [ ] **Step 1: Add the canvas-rendering helper**

In `crates/savvagent/src/ui.rs`, find the function that walks `entries` and renders each. Add a new branch for `Entry::Canvas`:

```rust
use savvagent_plugin::PixelSize;

// In the entry-render loop:
Entry::Canvas { id, source, source_preview } => {
    if let Some(preview) = source_preview {
        // Streaming — render as source preview.
        render_source_preview(frame, area, preview);
    } else if app.canvas_registry.image_protocol_available() {
        render_canvas_image(frame, area, app, *id);
    } else {
        render_canvas_source_fallback(frame, area, source);
    }
}
```

Then add the three helper functions in the same file:

```rust
fn render_canvas_image(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    app: &mut App,
    id: ContentBlockId,
) {
    // Pixel width = cell width * the picker-reported pixels per cell.
    let picker = app
        .canvas_registry
        .image_picker
        .as_ref()
        .expect("checked image_protocol_available before calling");
    let (cell_w, _cell_h) = picker.font_size();
    let pixel_width = area.width as u32 * cell_w as u32;

    let frame_data = {
        let renderer = app
            .canvas_registry
            .renderers
            .get_mut(&id)
            .expect("Canvas entry without renderer");
        renderer.render(PixelSize {
            width: pixel_width,
            height: 0,
        })
    };

    // Convert Frame bytes -> image::RgbaImage -> ratatui-image protocol.
    let image = ratatui_image_image_from_frame(&frame_data);
    let mut protocol = app
        .canvas_registry
        .image_states
        .entry(id)
        .or_insert_with(|| {
            picker
                .new_protocol(image.clone(), area, ratatui_image::Resize::Fit(None))
                .expect("protocol from picker")
        });

    let widget = ratatui_image::StatefulImage::default();
    frame.render_stateful_widget(widget, area, &mut protocol);
}

fn render_canvas_source_fallback(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    source: &str,
) {
    // Render `source` as a code block + banner. Use the same code-block
    // syntax-highlighting helper the existing markdown renderer uses
    // (check ui.rs for the helper name; likely `render_code_block` or
    // similar).
    let banner = "Inline HTML rendering requires kitty / WezTerm / Ghostty / iTerm2.";
    let banner_widget = ratatui::widgets::Paragraph::new(banner)
        .style(ratatui::style::Style::default().fg(ratatui::style::Color::Yellow));
    let (banner_area, body_area) = split_top_one_line(area);
    frame.render_widget(banner_widget, banner_area);
    render_code_block(frame, body_area, source, "html");
}

fn render_source_preview(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    preview: &str,
) {
    // Streaming-in-progress: same code-block helper, plus an "..."
    // typing indicator on the right margin.
    render_code_block(frame, area, preview, "html");
}

fn ratatui_image_image_from_frame(
    frame: &savvagent_plugin::Frame,
) -> image::RgbaImage {
    image::RgbaImage::from_raw(frame.width, frame.height, frame.bytes.clone())
        .expect("Frame::bytes length must match Rgba8 width*height*4")
}

fn split_top_one_line(area: ratatui::layout::Rect) -> (ratatui::layout::Rect, ratatui::layout::Rect) {
    let banner = ratatui::layout::Rect { height: 1, ..area };
    let body = ratatui::layout::Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    (banner, body)
}
```

Adapt `render_code_block` to whatever the existing helper is in `ui.rs`. If none exists, add a minimal one:

```rust
fn render_code_block(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    source: &str,
    _language: &str,
) {
    let widget = ratatui::widgets::Paragraph::new(source)
        .block(
            ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::LEFT)
                .border_style(
                    ratatui::style::Style::default().fg(ratatui::style::Color::Blue),
                ),
        );
    frame.render_widget(widget, area);
}
```

- [ ] **Step 2: Add the `image` crate dep if needed**

`ratatui-image` typically pulls `image` transitively; confirm with `cargo tree`. If you need it directly for `RgbaImage::from_raw`, add to `crates/savvagent/Cargo.toml`:

```toml
[dependencies]
image = "0.25"
```

- [ ] **Step 3: Manual smoke**

There's no good unit-test path for visual ratatui rendering. Instead, add a `cargo run` smoke step you can drive manually:

```bash
cargo run -p savvagent
```

Then in a kitty / WezTerm / Ghostty terminal:

```
> Please respond with `<savvagent-test-canvas>` block.
```

(Or any prompt that exercises the canvas — the system prompt segment from Task 13 tells the model to use html-canvas blocks for structured docs.) Verify a rendered image appears inline. Switch to alacritty / xterm and verify the source-code fallback appears with the banner.

Document this in the PR description; do not block the commit on automated verification.

- [ ] **Step 4: Run the workspace tests**

```bash
cargo test --workspace
```

Expected: PASS (the rendering changes don't break test paths).

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/
git commit -m "feat(tui): render Entry::Canvas inline via ratatui-image"
```

---

## Task 17: TUI — streaming source preview + swap on `ContentBlockStop`

**Files:**
- Modify: `crates/savvagent/src/app.rs`
- Modify: `crates/savvagent/src/tui.rs` (or wherever stream events are consumed)

Hook the stream-event consumer to handle `Html` content blocks: on `ContentBlockStart { block: ContentBlock::Html }` push an `Entry::Canvas { source_preview: Some(String::new()), ... }`; on `ContentBlockDelta { delta: HtmlSourceDelta }` append; on `ContentBlockStop` move the preview into `source`, allocate a `ContentBlockId`, create a renderer, and store in the registry.

- [ ] **Step 1: Write the failing test**

In `crates/savvagent/src/app.rs`, append:

```rust
    #[test]
    fn streaming_html_block_transitions_to_canvas_on_stop() {
        let mut app = App::new(/* test args — match existing helpers */);
        let block_id = app.handle_html_block_start();
        app.handle_html_block_delta(block_id, "<!doctype");
        app.handle_html_block_delta(block_id, " html><body>hi</body>");

        // While streaming, the entry has source_preview = Some(...).
        let entry = app.last_entry().expect("entry pushed");
        match entry {
            Entry::Canvas { source_preview, .. } => {
                assert_eq!(
                    source_preview.as_deref(),
                    Some("<!doctype html><body>hi</body>"),
                );
            }
            _ => panic!("expected Canvas entry"),
        }

        app.handle_html_block_stop(block_id);

        // Post-stop: preview is None, source is set, renderer is in
        // the registry.
        let entry = app.last_entry().expect("entry");
        match entry {
            Entry::Canvas { id, source, source_preview } => {
                assert!(source_preview.is_none());
                assert_eq!(source, "<!doctype html><body>hi</body>");
                assert!(app.canvas_registry.renderers.contains_key(id));
            }
            _ => panic!("expected Canvas entry"),
        }
    }
```

- [ ] **Step 2: Run the test; verify it fails**

```bash
cargo test -p savvagent app::tests::streaming_html_block_transitions_to_canvas_on_stop
```

Expected: FAIL.

- [ ] **Step 3: Implement the three handlers**

In `crates/savvagent/src/app.rs`:

```rust
impl App {
    /// Called when a streaming `ContentBlockStart` event arrives for a
    /// `ContentBlock::Html`. Pushes a placeholder Canvas entry with an
    /// empty source_preview and returns the allocated block id.
    pub fn handle_html_block_start(&mut self) -> ContentBlockId {
        let id = self.canvas_registry.allocate_id();
        self.entries.push(Entry::Canvas {
            id,
            source: String::new(),
            source_preview: Some(String::new()),
        });
        id
    }

    /// Append a streaming fragment.
    pub fn handle_html_block_delta(&mut self, id: ContentBlockId, fragment: &str) {
        if let Some(Entry::Canvas { source_preview, id: entry_id, .. }) =
            self.entries.iter_mut().rfind(|e| matches!(e, Entry::Canvas { id: eid, .. } if *eid == id))
        {
            if let Some(buf) = source_preview {
                buf.push_str(fragment);
            }
            let _ = entry_id;
        }
    }

    /// Finalize a streaming HTML block: move the preview into `source`,
    /// create a renderer via the canvas plugin, store it in the
    /// registry.
    pub fn handle_html_block_stop(&mut self, id: ContentBlockId) {
        // Move preview -> source.
        if let Some(entry) = self
            .entries
            .iter_mut()
            .rfind(|e| matches!(e, Entry::Canvas { id: eid, .. } if *eid == id))
        {
            if let Entry::Canvas { source, source_preview, .. } = entry {
                if let Some(preview) = source_preview.take() {
                    *source = preview;
                }
            }
        }

        // Find the canonical renderer plugin for "html".
        let plugin_id = match self.plugin_registry.content_renderer_for("html") {
            Some(id) => id.clone(),
            None => {
                tracing::debug!("no html renderer registered; canvas stays as source");
                return;
            }
        };
        let plugin = match self.plugin_registry.get(&plugin_id) {
            Some(p) => p,
            None => return,
        };

        // Extract the source we just finalized.
        let source = match self
            .entries
            .iter()
            .rfind(|e| matches!(e, Entry::Canvas { id: eid, .. } if *eid == id))
        {
            Some(Entry::Canvas { source, .. }) => source.clone(),
            _ => return,
        };

        match plugin.create_renderer("html", id, &source) {
            Ok(renderer) => {
                self.canvas_registry.insert(id, renderer);
            }
            Err(err) => {
                tracing::warn!(?err, "create_renderer failed; canvas stays as source");
            }
        }
    }

    /// Helper for tests.
    #[cfg(test)]
    pub(crate) fn last_entry(&self) -> Option<&Entry> {
        self.entries.last()
    }
}
```

(`plugin_registry` is whatever field on `App` holds the registry — adapt to existing naming.)

- [ ] **Step 4: Wire the handlers into the stream consumer**

Find the stream-event consumer (likely in `crates/savvagent/src/tui.rs` — it spawns the streaming worker per `feedback_drive_pr_series_to_completion` and `project_tui_design`). Locate the existing `match stream_event` block. Add branches:

```rust
StreamEvent::ContentBlockStart { index, block: ContentBlock::Html { .. } } => {
    let id = app.handle_html_block_start();
    // Track index -> id mapping so subsequent deltas can route.
    app.html_block_index_to_id.insert(index, id);
}
StreamEvent::ContentBlockDelta { index, delta: BlockDelta::HtmlSourceDelta { source } } => {
    if let Some(&id) = app.html_block_index_to_id.get(&index) {
        app.handle_html_block_delta(id, &source);
    }
}
StreamEvent::ContentBlockStop { index } => {
    // The existing handler probably already does its work for text/tool blocks;
    // add the html-specific finalization first:
    if let Some(&id) = app.html_block_index_to_id.get(&index) {
        app.handle_html_block_stop(id);
        app.html_block_index_to_id.remove(&index);
    }
    // ... continue with existing ContentBlockStop handling ...
}
```

Add `html_block_index_to_id: HashMap<u32, ContentBlockId>` to `App` and initialize.

- [ ] **Step 5: Run the test; verify it passes**

```bash
cargo test -p savvagent app::tests::streaming_html_block_transitions_to_canvas_on_stop
```

Expected: PASS.

- [ ] **Step 6: Run the workspace tests**

```bash
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/savvagent/
git commit -m "feat(tui): streaming HTML source preview + swap on block stop"
```

---

## Task 18: Auto-export to `~/.savvagent/canvases/`

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/html_canvas/auto_export.rs`
- Modify: `crates/savvagent/src/plugin/builtin/html_canvas/mod.rs`
- Modify: `crates/savvagent/src/plugin/builtin/html_canvas/plugin.rs`

Each finalized HTML block is written to `~/.savvagent/canvases/<unix>-<turn>-<block>.html`. Mode: 0o600; directory: 0o700 (matches the existing `~/.savvagent/credentials` pattern). Default on; disable via a `plugins.toml` flag.

The auto-export is triggered from `App::handle_html_block_stop` (Task 17) by emitting an effect-like signal the plugin acts on, OR by directly calling a function exposed by this task. Since the existing plugin architecture routes effects via `apply_effects`, and we don't want to introduce a new `Effect::AutoExportCanvas` variant just for this (it's an internal concern), we use a direct call from the App layer to a helper this task ships.

- [ ] **Step 1: Write the failing test**

In a new file `crates/savvagent/src/plugin/builtin/html_canvas/auto_export.rs`:

```rust
//! Auto-export each finalized HTML canvas to
//! `~/.savvagent/canvases/<unix>-<turn>-<block>.html`.

use std::path::{Path, PathBuf};

use savvagent_plugin::ContentBlockId;

/// Result of an auto-export attempt.
#[derive(Debug)]
pub enum AutoExportOutcome {
    /// Wrote the canvas to disk.
    Written { path: PathBuf },
    /// Auto-export is disabled via plugins.toml.
    Disabled,
    /// Write attempted and failed; we log and continue.
    Failed { err: std::io::Error },
}

/// Compute the auto-export path under `base_dir` for the given
/// (turn_id, block_id) at the given unix timestamp.
pub fn auto_export_path(
    base_dir: &Path,
    unix_ts: u64,
    turn_id: u32,
    block_id: ContentBlockId,
) -> PathBuf {
    base_dir.join(format!("{unix_ts:010}-{turn_id:06}-{}.html", block_id.0))
}

/// Write `source` to `path` with 0o600 permissions, creating `parent`
/// with 0o700 if it doesn't exist. Returns the outcome.
pub fn write_canvas(path: &Path, source: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(parent)?.permissions();
            perm.set_mode(0o700);
            std::fs::set_permissions(parent, perm)?;
        }
    }
    std::fs::write(path, source)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(path)?.permissions();
        perm.set_mode(0o600);
        std::fs::set_permissions(path, perm)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_export_path_format() {
        let p = auto_export_path(
            Path::new("/tmp/canvases"),
            1_716_300_000,
            12,
            ContentBlockId(3),
        );
        assert_eq!(
            p.to_string_lossy(),
            "/tmp/canvases/1716300000-000012-3.html"
        );
    }

    #[test]
    fn write_canvas_creates_file_with_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("subdir").join("x.html");
        write_canvas(&path, "<p>hi</p>").unwrap();
        let read = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read, "<p>hi</p>");
    }

    #[cfg(unix)]
    #[test]
    fn write_canvas_sets_secure_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("dir").join("y.html");
        write_canvas(&path, "<p>x</p>").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "file mode must be 0o600");
        let dir_mode = std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "dir mode must be 0o700");
    }
}
```

(`tempfile` is a common dev-dep across this workspace; verify it's already in `crates/savvagent/Cargo.toml` `[dev-dependencies]` and add if not.)

- [ ] **Step 2: Run the tests; verify they pass**

```bash
cargo test -p savvagent plugin::builtin::html_canvas::auto_export
```

Expected: PASS (this is a self-contained module).

- [ ] **Step 3: Wire `mod.rs`**

Append to `crates/savvagent/src/plugin/builtin/html_canvas/mod.rs`:

```rust
pub mod auto_export;
```

- [ ] **Step 4: Drive auto-export from `handle_html_block_stop`**

In `crates/savvagent/src/app.rs`, extend `handle_html_block_stop` (from Task 17) to trigger auto-export after the renderer is created. Add at the end:

```rust
        // Auto-export: write the canvas to ~/.savvagent/canvases/
        if self.html_canvas_auto_export_enabled() {
            use crate::plugin::builtin::html_canvas::auto_export::{
                auto_export_path, write_canvas,
            };
            let base = match home_dir() {
                Some(home) => home.join(".savvagent").join("canvases"),
                None => return,
            };
            let unix_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let path = auto_export_path(&base, unix_ts, self.current_turn_id(), id);
            if let Err(err) = write_canvas(&path, &source) {
                tracing::warn!(?err, ?path, "auto-export of canvas failed");
            }
        }
```

Add helper methods:

```rust
impl App {
    fn html_canvas_auto_export_enabled(&self) -> bool {
        // Read the plugins.toml entry for "internal:html-canvas":
        //   [plugins."internal:html-canvas"]
        //   auto_export = false  # disable; default is true
        // Adapt to the existing plugins.toml reader (in plugin/registry.rs
        // or similar).
        self.plugin_config
            .as_ref()
            .and_then(|cfg| cfg.option_for("internal:html-canvas", "auto_export"))
            .map(|v| v.as_bool().unwrap_or(true))
            .unwrap_or(true)
    }

    fn current_turn_id(&self) -> u32 {
        // App already tracks the current turn id for transcript persistence;
        // use that. If no such field exists, add it.
        self.current_turn_id_field
    }
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}
```

(Adapt `plugin_config` and `current_turn_id_field` to existing names. The `plugins.toml` reader is part of v0.9.0 plugin system.)

- [ ] **Step 5: Add a turn-id field if `App` doesn't have one**

If `App` doesn't already track the active turn id, add:

```rust
pub struct App {
    // ... existing ...
    current_turn_id_field: u32,
}
```

And increment it where each new turn starts (likely in `submit_prompt` or in the streaming worker).

- [ ] **Step 6: Add an integration test**

Append to `crates/savvagent/src/app.rs` tests:

```rust
    #[test]
    fn auto_export_writes_file_on_block_stop() {
        // Set HOME to a tempdir, drive a streaming html block, assert
        // the file exists. Use the existing HOME_LOCK pattern (per
        // memory: rust_i18n locale leaks between parallel tests —
        // HOME_LOCK serializes HOME-changing tests).
        let _lock = test_helpers::HOME_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HOME", tmp.path());

        let mut app = App::new(/* test args */);
        let id = app.handle_html_block_start();
        app.handle_html_block_delta(id, "<p>hi</p>");
        app.handle_html_block_stop(id);

        let canvases_dir = tmp.path().join(".savvagent").join("canvases");
        let entries: Vec<_> = std::fs::read_dir(&canvases_dir)
            .expect("canvases dir exists")
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        let content = std::fs::read_to_string(entries[0].path()).unwrap();
        assert_eq!(content, "<p>hi</p>");
    }
```

(Use the existing `test_helpers::HOME_LOCK` per the memory note about test locale isolation.)

- [ ] **Step 7: Run the workspace tests**

```bash
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent/
git commit -m "feat(canvas): auto-export each html block to ~/.savvagent/canvases"
```

---

## Task 19: `/save-canvas` slash command

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/html_canvas/slash.rs`
- Modify: `crates/savvagent/src/plugin/builtin/html_canvas/mod.rs`
- Modify: `crates/savvagent/src/plugin/builtin/html_canvas/plugin.rs`

Adds the `SlashSpec` and `Plugin::handle_slash` branch for `/save-canvas`. Args:

- `/save-canvas` → writes the most recent canvas to a default path under cwd.
- `/save-canvas ./my-spec.html` → writes to the explicit path.
- `/save-canvas ./spec.html --block 3` → writes the canvas with `ContentBlockId(3)`.
- Optional `--open` flag → emits `Effect::OpenUrl` for the `file://` URL after writing.

The plugin needs read access to the App's canvases (or, equivalently, the slash handler receives the canvas source via context). The current `Plugin::handle_slash` signature takes only `&mut self` + `name` + `args`, so the plugin can't read the App's state. Two options:

1. Plumb the canvas source list through `handle_slash` (changes the trait signature, undesirable).
2. The TUI's slash dispatcher intercepts `/save-canvas`, builds a context object, and calls a helper on the plugin.

Option 2 keeps the trait stable. The slash dispatch in `crates/savvagent/src/plugin/slash.rs` already does some per-slash work; add a special-case for `internal:html-canvas:save-canvas` that bypasses the trait and calls into a non-trait helper.

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent/src/plugin/builtin/html_canvas/slash.rs`:

```rust
//! Logic for the /save-canvas slash command.
//!
//! The slash dispatcher in `crates/savvagent/src/plugin/slash.rs` calls
//! [`dispatch`] directly (bypassing the Plugin trait) because the
//! command needs access to App-owned canvas state.

use std::path::{Path, PathBuf};

use savvagent_plugin::{ContentBlockId, Effect, UrlTarget};

use crate::plugin::builtin::html_canvas::auto_export::write_canvas;

/// Parsed args for /save-canvas.
#[derive(Debug, PartialEq, Eq)]
pub struct SaveCanvasArgs {
    /// Output path, or None to derive from cwd.
    pub path: Option<PathBuf>,
    /// Specific block id to save, or None to save the most recent.
    pub block: Option<ContentBlockId>,
    /// Whether to open the file after writing.
    pub open: bool,
}

/// Parse args from the slash invocation. `args` is everything after
/// the command name on the input line, already tokenised.
pub fn parse_args(args: &[String]) -> Result<SaveCanvasArgs, String> {
    let mut path: Option<PathBuf> = None;
    let mut block: Option<ContentBlockId> = None;
    let mut open = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--block" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--block requires an argument")?;
                let n: u32 = v.parse().map_err(|_| format!("invalid block id: {v}"))?;
                block = Some(ContentBlockId(n));
                i += 2;
            }
            "--open" => {
                open = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if path.is_some() {
                    return Err(format!("unexpected argument: {other}"));
                }
                path = Some(PathBuf::from(other));
                i += 1;
            }
        }
    }

    Ok(SaveCanvasArgs { path, block, open })
}

/// Dispatch the slash. `canvases` is the App-supplied set of currently
/// known canvases in transcript order; `cwd` is the working directory.
pub fn dispatch(
    args: SaveCanvasArgs,
    canvases: &[(ContentBlockId, String)],
    cwd: &Path,
) -> Result<DispatchResult, String> {
    let (id, source) = match args.block {
        Some(id) => canvases
            .iter()
            .find(|(eid, _)| *eid == id)
            .ok_or_else(|| format!("no canvas with id {}", id.0))?
            .clone(),
        None => canvases
            .last()
            .ok_or("no canvas in transcript yet")?
            .clone(),
    };

    let path = args.path.unwrap_or_else(|| {
        cwd.join(format!("savvagent-canvas-{}.html", id.0))
    });
    write_canvas(&path, &source).map_err(|e| format!("write failed: {e}"))?;

    let mut effects = Vec::new();
    if args.open {
        effects.push(Effect::OpenUrl {
            url: format!("file://{}", path.display()),
            target: UrlTarget::SystemBrowser,
        });
    }

    Ok(DispatchResult { path, effects })
}

#[derive(Debug)]
pub struct DispatchResult {
    pub path: PathBuf,
    pub effects: Vec<Effect>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_no_args_returns_defaults() {
        let a = parse_args(&[]).unwrap();
        assert_eq!(a, SaveCanvasArgs { path: None, block: None, open: false });
    }

    #[test]
    fn parse_explicit_path() {
        let a = parse_args(&["./out.html".into()]).unwrap();
        assert_eq!(a.path.as_deref(), Some(Path::new("./out.html")));
    }

    #[test]
    fn parse_block_flag() {
        let a = parse_args(&["--block".into(), "7".into()]).unwrap();
        assert_eq!(a.block, Some(ContentBlockId(7)));
    }

    #[test]
    fn parse_open_flag() {
        let a = parse_args(&["--open".into()]).unwrap();
        assert!(a.open);
    }

    #[test]
    fn parse_combined() {
        let a = parse_args(&[
            "./x.html".into(),
            "--block".into(),
            "2".into(),
            "--open".into(),
        ])
        .unwrap();
        assert_eq!(a.path.as_deref(), Some(Path::new("./x.html")));
        assert_eq!(a.block, Some(ContentBlockId(2)));
        assert!(a.open);
    }

    #[test]
    fn parse_unknown_flag_errors() {
        let e = parse_args(&["--bogus".into()]).unwrap_err();
        assert!(e.contains("unknown flag"));
    }

    #[test]
    fn dispatch_writes_file_and_emits_open_effect() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let canvases = vec![(ContentBlockId(0), "<p>hi</p>".to_string())];
        let r = dispatch(
            SaveCanvasArgs { path: None, block: None, open: true },
            &canvases,
            cwd,
        )
        .unwrap();
        assert!(r.path.exists());
        assert_eq!(r.effects.len(), 1);
        assert!(matches!(&r.effects[0], Effect::OpenUrl { target, .. } if *target == UrlTarget::SystemBrowser));
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p savvagent plugin::builtin::html_canvas::slash
```

Expected: PASS.

- [ ] **Step 3: Add the SlashSpec to the plugin manifest**

In `crates/savvagent/src/plugin/builtin/html_canvas/plugin.rs`, extend the `Contributions` in the manifest:

```rust
            contributions: Contributions {
                content_renderers: vec![/* ... */],
                prompt_segments: vec![/* ... */],
                slash_commands: vec![SlashSpec {
                    name: "save-canvas".to_string(),
                    summary: "Save the most recent HTML canvas to a file".to_string(),
                    args_hint: Some("[path] [--block N] [--open]".to_string()),
                    requires_arg: false,
                    suppress_prompt_segments: vec![],
                }],
                ..Contributions::default()
            },
```

- [ ] **Step 4: Intercept `/save-canvas` in the slash dispatcher**

In `crates/savvagent/src/plugin/slash.rs`, find the `dispatch` function. Before the standard `plugin.handle_slash(...)` path, add a special-case:

```rust
if name == "save-canvas" {
    use crate::plugin::builtin::html_canvas::slash::{dispatch, parse_args};
    let parsed = parse_args(&args).map_err(|e| {
        // Convert to whatever error type the dispatcher returns;
        // most paths log + return Ok(vec![note]).
        // Pseudocode:
        return Ok(vec![Effect::PushNote {
            line: error_styled_line(format!("/save-canvas: {e}")),
        }]);
    })?;
    let canvases = app.canvas_sources_in_order();   // see Step 5
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    return match dispatch(parsed, &canvases, &cwd) {
        Ok(result) => {
            let mut effs = vec![Effect::PushNote {
                line: success_styled_line(format!("Canvas saved to {}", result.path.display())),
            }];
            effs.extend(result.effects);
            Ok(effs)
        }
        Err(e) => Ok(vec![Effect::PushNote {
            line: error_styled_line(format!("/save-canvas: {e}")),
        }]),
    };
}
```

(`error_styled_line` / `success_styled_line` adapt to existing helpers.)

- [ ] **Step 5: Add `App::canvas_sources_in_order`**

In `crates/savvagent/src/app.rs`:

```rust
impl App {
    /// Return all finalized canvases (not in-flight previews) in
    /// transcript order. Used by /save-canvas.
    pub fn canvas_sources_in_order(&self) -> Vec<(ContentBlockId, String)> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Canvas { id, source, source_preview, .. }
                    if source_preview.is_none() =>
                {
                    Some((*id, source.clone()))
                }
                _ => None,
            })
            .collect()
    }
}
```

- [ ] **Step 6: Run the workspace tests**

```bash
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 7: Manual smoke**

```bash
cargo run -p savvagent
# In the TUI, after the model produces a canvas:
/save-canvas
/save-canvas ./my-spec.html
/save-canvas --open
```

Verify the files appear and `--open` shells to the system browser.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent/
git commit -m "feat(canvas): /save-canvas slash command"
```

---

## Task 20: Wire `Host::set_prompt_segments` from the TUI startup

**Files:**
- Modify: `crates/savvagent/src/main.rs` (or wherever the TUI builds the host)

The host has `set_prompt_segments` (Task 10). The plugin registry has `active_prompt_segments` (Task 14). The TUI just needs to call them — at startup and any time the enabled-plugin set changes.

- [ ] **Step 1: Write the failing test**

Append to `crates/savvagent/src/app.rs` tests:

```rust
    #[tokio::test]
    async fn startup_pushes_html_canvas_segment_to_host() {
        let _lock = test_helpers::HOME_LOCK.lock().unwrap();
        // Build an App with default plugins enabled; assert the host
        // received the html-canvas segment.
        let app = App::new_for_test(/* tokio-friendly test ctor */).await;
        let segments = app.host_prompt_segments_snapshot();
        assert!(segments.iter().any(|s| s.id == "internal:html-canvas:default"));
    }
```

(`new_for_test` and `host_prompt_segments_snapshot` are test-only helpers; add them in Step 3 if absent.)

- [ ] **Step 2: Run the test; verify it fails**

```bash
cargo test -p savvagent app::tests::startup_pushes_html_canvas_segment_to_host
```

Expected: FAIL.

- [ ] **Step 3: Call `set_prompt_segments` after registry build**

In the startup path (`main.rs` or `App::new`), after `register_builtins()` runs and the registry is constructed, push segments to the host:

```rust
let segments = registry.active_prompt_segments();
host.set_prompt_segments(segments);
```

Add the test-only snapshot helper in `App`:

```rust
#[cfg(test)]
impl App {
    pub(crate) fn host_prompt_segments_snapshot(&self) -> Vec<SystemPromptSegment> {
        // Adapt to how the host exposes its current segments — add an
        // accessor if needed.
        self.host.active_prompt_segments()
    }
}
```

If you added the helper to `Host::active_prompt_segments` in Task 10 as `pub(crate)`, promote it to `pub` here or use a `pub(crate) fn snapshot_for_tests()` instead.

- [ ] **Step 4: Wire suppression list when a slash is dispatched**

In `crates/savvagent/src/plugin/slash.rs::dispatch`, before invoking `host.run_turn_streaming` for the slash, fetch the `SlashSpec.suppress_prompt_segments` and call `host.set_turn_suppression`:

```rust
let suppress = registry
    .slash_spec(&name)
    .map(|spec| spec.suppress_prompt_segments.clone())
    .unwrap_or_default();
host.set_turn_suppression(suppress);
// ... existing turn dispatch ...
```

`registry.slash_spec(name)` is an accessor over the slash index — add if missing.

- [ ] **Step 5: Run the test; verify it passes**

```bash
cargo test -p savvagent app::tests::startup_pushes_html_canvas_segment_to_host
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/
git commit -m "feat(tui): push plugin prompt segments to host at startup"
```

---

## Task 21: README + tmux passthrough docs

**Files:**
- Modify: `README.md`
- Create: `docs/canvas-terminal-compat.md`

User-facing docs for the new feature: terminal compatibility matrix, tmux passthrough caveat, `/save-canvas` command, `auto_export` flag.

- [ ] **Step 1: Add a top-level feature blurb to README**

In `README.md`, after the existing feature list or in a logical place, add:

```markdown
### Inline HTML rendering

Savvagent renders model-emitted HTML inline in the chat transcript when
your terminal supports an image protocol (Kitty / iTerm2 / WezTerm /
Ghostty / sixel). Models are prompted to wrap structured documents
(plans, specs, status updates, reviews) in ```html-canvas fenced
blocks; savvagent renders them as static images for now and Phase 2
will add mouse + keyboard interaction.

Every rendered canvas is also auto-exported to
`~/.savvagent/canvases/<unix>-<turn>-<block>.html` for opening in a
real browser or for sharing. Use `/save-canvas [path] [--open]` to
write to an explicit location.

Disable inline rendering by toggling the `internal:html-canvas` plugin
off in `~/.savvagent/plugins.toml`:

\`\`\`toml
[plugins."internal:html-canvas"]
enabled = false
\`\`\`

Or disable auto-export only:

\`\`\`toml
[plugins."internal:html-canvas"]
enabled = true
auto_export = false
\`\`\`

See `docs/canvas-terminal-compat.md` for the supported-terminal matrix
and tmux passthrough setup.
```

- [ ] **Step 2: Create `docs/canvas-terminal-compat.md`**

```markdown
# Inline HTML canvas — terminal compatibility

The canvas feature places rendered HTML in the conversation transcript
via your terminal's image protocol. Detection runs once at startup;
terminals without a supported protocol fall back to a syntax-highlighted
source-code view with a one-line banner.

## Supported

| Terminal | Protocol | Notes |
|---|---|---|
| Kitty | kitty graphics | First-class. Incremental frame updates. |
| Ghostty | kitty graphics | First-class. |
| WezTerm | iTerm2 inline | First-class. |
| iTerm2 (macOS) | iTerm2 inline | First-class. |
| Anything with sixel | sixel | Best-effort; full-frame updates. |

## Tmux

Tmux eats terminal escape sequences by default, breaking image
protocols. Enable passthrough:

\`\`\`bash
# In your ~/.tmux.conf
set -g allow-passthrough on
\`\`\`

If you see corrupted output or no images inside tmux, this is the most
likely cause.

## Unsupported

Alacritty, plain xterm, terminals without image-protocol support: the
canvas falls back to a syntax-highlighted source-code view with a
banner noting the limitation. Functionality is preserved — content is
still readable.

## SSH

Image protocols work over SSH when the local terminal supports them
and the connection forwards escape sequences (the default for `ssh`).
No special setup needed.

## Troubleshooting

- **Source-code fallback appears but you expect images:** Your terminal
  doesn't have a supported protocol detected. Re-check the list above.
- **Images render glitched/torn:** You're likely inside tmux without
  passthrough; see above. Outside tmux, file a bug with terminal
  name + version.
- **Render is blocky / wrong colours:** Sixel terminals have variable
  colour-depth support. Try a kitty/iTerm2-protocol terminal.
```

- [ ] **Step 3: Commit**

```bash
git add README.md docs/canvas-terminal-compat.md
git commit -m "docs(canvas): terminal compatibility matrix + tmux passthrough"
```

---

## Task 22: CHANGELOG entry + version bump + release scaffolding commit

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `Cargo.toml` (workspace)
- Modify: each crate's `Cargo.toml` literal if it doesn't inherit `version.workspace = true`

Bump the workspace version from `0.16.1` → `0.17.0`. Per the project memory (`feedback_phase_release_rollup`), this is a *scaffolding* release commit — no git tag is pushed. The actual v0.18.0 tag goes up only after Phase 2 also ships.

- [ ] **Step 1: Bump the workspace version**

In the root `Cargo.toml`:

```toml
[workspace.package]
version = "0.17.0"
```

If individual crates carry literal versions in their own `[package]` blocks (rather than `version.workspace = true`), bump those too. Audit with:

```bash
grep -rn '^version = ' crates/*/Cargo.toml | grep -v workspace
```

Update any matches to `0.17.0`.

Also update workspace dependency literals to `0.17.0` for the in-repo crates:

```bash
grep -n 'savvagent-.*version = "0\.15\.0"' Cargo.toml
```

For each matching workspace dependency entry, change the version to `0.17.0`.

- [ ] **Step 2: Update CHANGELOG**

In `CHANGELOG.md`, add a new section at the top (above the most recent existing entry):

```markdown
## [0.17.0] - unreleased

> This release is part of the inline HTML canvas initiative. Per the
> repo's multi-phase release convention, **no git tag is pushed for
> 0.17.0** — the final tag (v0.18.0) goes up after Phase 2 (mouse +
> keyboard interaction) lands. See
> `docs/superpowers/specs/2026-05-21-inline-html-canvas-design.md`.

### Added

- SPP v0.2.0: new `ContentBlock::Html { source }` content block and
  `BlockDelta::HtmlSourceDelta { source }` stream delta (additive;
  v0.1.0 conformance is preserved).
- `savvagent-fence` crate: streaming parser that extracts
  ` ```html-canvas ` fenced blocks from model text output. Wired into
  all four providers (`provider-anthropic`, `provider-gemini`,
  `provider-openai`, `provider-local`).
- `savvagent-canvas` crate: Blitz-backed `HtmlCanvas` implementing
  `ContentRenderer` (static rendering only; eventing in Phase 2).
- Plugin trait surface: `ContentRenderer` trait + supporting types
  (`Frame`, `PixelSize`, `PixelFormat`, `ContentBlockId`, …);
  `Plugin::create_renderer` factory method; `Contributions::content_renderers`;
  `Contributions::prompt_segments`; `SlashSpec::suppress_prompt_segments`;
  `Effect::OpenUrl { url, target }` + `UrlTarget` enum.
- Host: `Host::set_prompt_segments` + `Host::set_turn_suppression`
  compose plugin-contributed `SystemPromptSegment`s into the system
  prompt; per-slash suppression drops segments for a single turn
  (e.g. `/commit` can suppress the html-canvas segment).
- `internal:html-canvas` built-in plugin: claims SPP `"html"` blocks as
  the canonical renderer; contributes a default system prompt segment;
  ships the `/save-canvas` slash command.
- Auto-export: every finalized HTML canvas is written to
  `~/.savvagent/canvases/<unix>-<turn>-<block>.html` (mode 0o600,
  directory 0o700). Disable via
  `[plugins."internal:html-canvas"] auto_export = false`.
- TUI: streaming HTML blocks show a typewriter-style source preview
  during the stream and swap to the rendered canvas on
  `ContentBlockStop`. Terminals without an image protocol render the
  source as a syntax-highlighted code block with a banner.
- Docs: `docs/canvas-terminal-compat.md` (supported terminals + tmux
  passthrough).

### Changed

- Workspace dependency: `ratatui-image` added.
- Workspace dependency: `blitz` added (version pinned by the Phase 0
  spike; see `docs/superpowers/notes/2026-05-21-blitz-spike.md`).
```

- [ ] **Step 3: Build the workspace to confirm versions match**

```bash
cargo build --workspace
```

Expected: PASS. If anything fails because a dep literal didn't match the workspace version, fix and rebuild.

- [ ] **Step 4: Run the full test suite**

```bash
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 5: Format + clippy with the CI stable toolchain**

Per the memory note `feedback_match_ci_toolchain_locally`, run with the same stable Rust the CI uses:

```bash
rustup run stable cargo fmt --all -- --check
rustup run stable cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS. If clippy flags anything, fix and re-run.

- [ ] **Step 6: Commit the release scaffolding**

```bash
git add Cargo.toml CHANGELOG.md crates/*/Cargo.toml
git commit -m "release(0.17.0): inline HTML canvas Phase 1"
```

**Do not push.** Per the memory note `feedback_cargo_dist_release` and `feedback_phase_release_rollup`, no git tag is created here — Phase 2 ships the final tag.

---

## Acceptance check (run after every PR / before final release commit)

These map 1:1 to the spec's acceptance criteria (§ Acceptance criteria, items 1–17 in the spec).

- [ ] `cargo test --workspace` is green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is green.
- [ ] `cargo fmt --all -- --check` is green.
- [ ] SPP `Html { source }` block and `HtmlSourceDelta` round-trip JSON (Tasks 2, 3).
- [ ] All four providers emit `Html` blocks when their model output contains a `html-canvas` fence (Task 6 tests).
- [ ] `savvagent-canvas::HtmlCanvas::render` produces an Rgba8 frame of the requested width whose bytes match `w*h*4` (Task 11 test).
- [ ] Subset validator emits expected warnings (Task 12 tests).
- [ ] `HtmlCanvasPlugin` manifest declares one canonical content renderer for `"html"` and one prompt segment (Task 13 tests).
- [ ] Plugin registry routes `"html"` block type to `internal:html-canvas` (Task 14 tests).
- [ ] `App` Entry has a `Canvas` variant with serde round-trip (Task 15 tests).
- [ ] Streaming HTML blocks transition preview → rendered on `ContentBlockStop` (Task 17 test).
- [ ] Auto-export writes a file at the expected path with 0o600 mode (Task 18 tests).
- [ ] `/save-canvas` parses flags correctly and emits an `OpenUrl` effect when `--open` is set (Task 19 tests).
- [ ] Startup pushes `internal:html-canvas:default` segment to the host (Task 20 test).
- [ ] Manual cross-terminal smoke: render a canvas in kitty (success), in alacritty (source fallback with banner).
- [ ] `CHANGELOG.md` has a `[0.17.0] - unreleased` section.
- [ ] `Cargo.toml` workspace version is `0.17.0`.
- [ ] `Cargo.lock` updated and committed (run `cargo update -p savvagent` if any in-repo version literal moved).
- [ ] No commits push to remote unless explicitly authorized.

---

## Phase 2 carryover (NOT part of this plan)

The following items are deliberately out of scope for Phase 1 and ship in the separate Phase 2 plan:

- `ContentRenderer::dispatch`, `freeze`, `thaw`, `focusable_elements`,
  `set_focus`, `focused_index` implementations on `HtmlCanvas`.
- `AppFocus::Canvas`; canvas focus state in `App`.
- Mouse + keyboard event routing into focused canvases.
- Tab / Shift-Tab traversal of focusable elements within a canvas.
- Ctrl-J / Ctrl-K traversal between canvases in the transcript.
- Escape to unfocus.
- `<details>` expand/collapse, link follow, form input interactions.
- `KeyScope::OnFocusedCanvas` keybinding scope.
- Ctrl-O "open in browser" keybinding.
- Soft-freeze / thaw lifecycle.
- Focus chrome (border) around the focused canvas.
- `release(0.18.0)` scaffolding commit and v0.18.0 git tag.
