# Inline HTML canvas — design

Date: 2026-05-21 (Phase 1 + 2 design); Phase 2 amendment 2026-05-23
Status: Phase 1 shipped (PR #97); Phase 2 amendment approved 2026-05-23
Supersedes: nothing
Related: v0.9.0 plugin system (`2026-05-12-v0.9.0-plugin-system-design.md`); SPP wire format (`crates/savvagent-protocol/SPEC.md`)
Inspiration: ["How I AI: HTML is the new markdown"](https://www.lennysnewsletter.com/p/how-i-ai-html-is-the-new-markdown)

## Phase 2 amendment (2026-05-23)

This spec was originally written for both phases of the inline HTML
canvas initiative. Phase 1 (static rendering + export) shipped via the
22-task plan at
`docs/superpowers/plans/2026-05-21-inline-html-canvas-phase-1.md`. This
amendment closes the spec for Phase 2 implementation by:

1. **Promoting** the spec's existing Phase 2 subsections — `Focus
   model`, `Keyboard routing`, `Mouse routing`, `Focus chrome`,
   `Lifecycle and soft freeze`, and the `ContentRenderer` event-surface
   methods (`dispatch`, `freeze`, `thaw`, `focusable_elements`,
   `set_focus`, `focused_index`) — from "deferred to Phase 2" to
   "shipping in Phase 2." Their as-written design is unchanged; only
   the disposition moves.
2. **Pinning** Blitz at the same versions Phase 1 ships
   (`blitz-* = "=0.3.0-alpha.4"`). A 2026-05-23 re-check of crates.io
   confirmed no alpha.5 has been published; the Phase 0 spike findings
   continue to apply, including the requirement that Phase 2 ship a
   **host-side default-action router** (links, `<summary>`, form
   submit) inside `HtmlCanvas::dispatch`.
3. **Adding** three capability areas not in the original spec:
   - **Tool-emitted HTML** (new § *Tool-emitted HTML*): MCP tool
     results can carry `{ "type": "html", "source": "..." }` content
     items, which `ToolRegistry` translates to SPP `ContentBlock::Html`
     blocks. Bypasses the fence parser (the content is already typed).
   - **Persistence of interactive state** (updates § *Persistence*):
     two new `ContentRenderer` trait methods, `snapshot_state` and
     `restore_state`, carry an opaque renderer-defined byte blob in
     the transcript JSON. `HtmlCanvas` serializes form values, scroll
     offsets, expanded-details set, and the focused-element id; on
     `/resume` the bytes are handed back via `restore_state`.
   - **Sub-agent prompt firmness** (updates § *Agent contention*): the
     "future" hedge is replaced by a concrete contract — sub-agent
     `CompleteRequest::system` REPLACES the host's composed prompt by
     default; plugin `SystemPromptSegment`s do NOT leak in; sub-agent
     manifests can opt in per-segment via `inherit_segments:
     Vec<String>`. Code change deferred to whenever sub-agents land;
     this spec section is the contract that future code must honor.
4. **Resolving** the original open question on default `UrlTarget` for
   `<a href>` follows (see § *Open questions*): **absolute URLs →
   `SystemBrowser`; relative paths → `ContinueConversation`**. Decided
   here so the Phase 2 plan can codify it.
5. **Documenting** two design refinements surfaced during Phase 2
   scoping:
   - The default-action interceptor lives **inside
     `savvagent-canvas::HtmlCanvas::dispatch`**, returning
     `Effect::OpenUrl` via `InputOutcome::effects` so the host still
     mediates the actual shell-out (see § *Architecture overview*).
   - **Built-in canvas keys take precedence over plugin keybindings
     in `KeyScope::OnFocusedCanvas`** (Tab, Shift-Tab, Esc, Ctrl-J,
     Ctrl-K, Ctrl-O; see § *Keyboard routing*). Plugin authors register
     non-conflicting bindings only.

After Phase 2 lands, the version bump from `release(0.17.0)` (already
in master from Phase 1's scaffolding commit) is followed by the actual
`v0.17.0` git tag push — `cargo-dist` owns the release artifact build
from there.

The rest of this document is the original 2026-05-21 design with
in-place amendments at the affected sections. Where a section's text
was updated, the change is in-line; where new sections were added,
they appear at the natural location in the document.

## Problem

Markdown is the default medium for model output and humans struggle with
it. Plans grow to thousands of lines, specs interleave dense code blocks
with prose, review docs have structure that markdown can hint at but not
*render*. The reader's failure mode is not "can't decode the syntax" —
it's "loses the thread." Information density without visual hierarchy
turns into a wall of text that gets skimmed instead of read.

The article cited above argues for **HTML as the medium of AI output**.
Interactive, scrollable plans. Status updates that get opened instead of
ignored. Specs with collapsible sections, side-by-side diffs, callouts
with semantic emphasis. The model still does the cognition; HTML carries
the result in a shape humans actually engage with.

Savvagent today renders model output as plain text into a ratatui
`Paragraph`. There is no path for the model to express structure beyond
ASCII art. This spec adds one: the model emits HTML in a recognized
content block, savvagent renders it **inline in the conversation
transcript** with full mouse and keyboard interaction, without becoming
a GUI app and without executing model-authored Rust or JavaScript.

## Goals

1. **Inline in the chat transcript.** Rendered HTML appears mixed with
   text turns in the conversation log, like images in a chat UI. Not in
   a side pane, not a separate window.
2. **Real in-pane interaction.** Mouse-click focuses a block; mouse-move
   produces hover; scroll wheel scrolls overflow containers; keyboard
   Tab/Shift-Tab walks focusable elements within the focused block;
   dedicated chord (Ctrl-J/Ctrl-K) jumps between blocks. No
   model-in-the-loop round-trips for interaction.
3. **Subset of HTML/CSS.** Pin a concrete subset (§ "HTML+CSS subset")
   that the model is prompted to use. The subset is the contract; the
   renderer is permitted to be lenient outside it but the prompt does
   not advertise it.
4. **Soft freeze on focus loss.** When focus leaves a block, the
   renderer stops dispatching events and stops re-rendering but retains
   DOM state. Refocus thaws losslessly.
5. **Pragmatic streaming.** While the HTML is streaming from the model,
   show the source typewriter-style (syntax-highlighted code preview)
   inline where the rendered block will appear. On stream complete,
   swap the source preview for the rendered canvas in place.
6. **Stay a TUI app.** No new window, no GUI shell. The terminal is
   still the only chrome.
7. **No WASM, no runtime rustc.** The model emits HTML, not Rust or
   JS. The renderer is an in-process Rust library compiled into the
   savvagent binary.

## Non-goals

- **Become a GUI application.** Even hybrid in-window/in-terminal modes
  are out of scope.
- **Run JavaScript.** No `<script>` execution, no DOM events from JS, no
  network access from rendered docs.
- **Render arbitrary internet HTML.** The subset is what the model is
  prompted to emit and what we promise to render. Pasting a random web
  page is undefined behavior.
- **Pixel-perfect parity with a real browser.** We render via Blitz on
  a constrained subset; visual fidelity is "good enough that humans
  prefer it to markdown," not "matches Chrome exactly."
- **Terminal-widget fallback rendering** (mapping HTML to ratatui
  widgets). v1 requires a terminal with an image protocol; degraded
  terminals show source code with a banner. Path A from brainstorming
  is deferred to a future spec if there is demand.
- **Streaming layout.** Re-laying-out the doc on every chunk is
  expensive and visually jumpy; we use the source-preview-then-swap
  pattern instead (§ Streaming).
- **Cross-block focus persistence after restart.** Interactive state
  (form values, scroll position, expanded `<details>`) lives in
  memory; saved transcripts re-render from source on load with no
  interactive state.

## Approach

A new content block type `Html { source }` in SPP. Providers extract it
from a sentinel-fenced code block in the model's text response. A new
in-process crate `savvagent-canvas` wraps [Blitz] (Servo-derived layout
+ Stylo + Markup5ever) and exposes a WIT-portable interface for
"render HTML to pixel buffer + dispatch events + report focus state."
The conversation log gains a new item variant for HTML blocks; rendered
output goes inline via [`ratatui-image`] (Kitty / iTerm2 / WezTerm /
Ghostty / sixel). The v0.9.0 plugin trait surface is extended with a
`ContentRenderer` contribution kind; the HTML canvas ships as the
first built-in plugin against that surface (`internal:html-canvas`).

[Blitz]: https://github.com/DioxusLabs/blitz
[`ratatui-image`]: https://crates.io/crates/ratatui-image

Approach risks called out up front:

- **Blitz's event-dispatch surface is the load-bearing assumption.**
  Layout + paint are well-supported in Blitz's headless mode; the
  "send a click at (x,y) and get back the updated DOM" path is less
  battle-tested for embedders outside Dioxus. Phase 0 of the
  implementation plan is a spike that proves out the eventing path
  against a pinned Blitz version.
  **Spike outcome (2026-05-21, blitz-* 0.3.0-alpha.4):** synthetic
  events *are* accepted by `BaseDocument::handle_dom_event` and
  `Document::handle_ui_event` without error, but Blitz's published
  headless API does NOT run the browser's default actions (a
  synthetic click on `<summary>` does not toggle the parent
  `<details>`'s `open` attribute, neither immediately nor after a
  subsequent `resolve()`). Phase 2 will therefore ship with a
  *host-side event router* inside `savvagent-canvas::HtmlCanvas::dispatch`
  that maps clicks-on-`<summary>` to manual `open`-attribute flipping,
  clicks-on-`<a href>` to `Effect::OpenUrl`, and form submission to a
  synthesized `Effect::OpenUrl`. The `ContentRenderer` trait surface
  defined in this spec is unaffected by the router; it lives inside one
  method's implementation. See
  `docs/superpowers/notes/2026-05-21-blitz-spike.md` for details.
- **`<details>` body painted regardless of `open` state.** Independent
  of events, the spike found that Blitz 0.3.0-alpha.4's headless paint
  renders the `<details>` body even when the `open` attribute is
  absent. For Phase 1 (no interaction yet), the system-prompt segment
  steers the model away from `<details>` (or uses it only for
  always-visible disclosures). For Phase 2, the host router's
  `open`-attribute-flip + re-render path naturally drives the correct
  paint output, since paint just renders whatever the current attribute
  says — the bug only bites when the attribute is *off* and Blitz
  paints as if it were *on*. Mitigation in v1 prompts: prefer
  semantically-flat structure to `<details>`. Subset validator warns
  for `<details>` in the document until the upstream paint bug is
  fixed.
- **`StyleThreading::Sequential` required.** `HtmlCanvas` constructs Blitz with
  `StyleThreading::Sequential` because Blitz's default `Parallel` threading panics
  under concurrent renders (upstream Blitz issue #430). This serializes style recalc
  to the calling thread; acceptable for headless single-turn rendering, but worth
  revisiting if Blitz upstream resolves the panic before Phase 2.
- **Image-protocol bandwidth.** Hover means re-uploading the frame on
  mouse-move. Mitigated by re-render only on state change and by
  Kitty's incremental frame update support; spec calls out tmux as a
  known degraded case.

## Architecture overview

```
┌─────────────────────────────────────────────────────────────────────┐
│ Provider (anthropic/openai/gemini/local)                            │
│   - Parses model text response for ```html-canvas``` fenced blocks  │
│   - Emits SPP `Html { source }` content blocks alongside `Text`     │
└────────────────────────────┬────────────────────────────────────────┘
                             │ SPP CompleteResponse / StreamEvent
                             ▼
┌─────────────────────────────────────────────────────────────────────┐
│ savvagent-host                                                       │
│   - Forwards content blocks to TUI without inspecting Html source    │
│   - Persists `Html { source }` blocks in transcript JSON unchanged   │
└────────────────────────────┬────────────────────────────────────────┘
                             │ StreamEvent / TurnComplete
                             ▼
┌─────────────────────────────────────────────────────────────────────┐
│ savvagent (TUI)                                                      │
│  ┌──────────────────────────────────────────────────────────────┐    │
│  │ Conversation log                                             │    │
│  │   Vec<LogItem { Text | ToolCall | Canvas(BlockId) }>          │    │
│  └──────────────────────────────────────────────────────────────┘    │
│  ┌──────────────────────────────────────────────────────────────┐    │
│  │ CanvasRegistry: HashMap<BlockId, Box<dyn ContentRenderer>>   │    │
│  │   - Soft-freeze state (Active / Frozen)                      │    │
│  │   - Image protocol cache IDs                                 │    │
│  └──────────────────────────────────────────────────────────────┘    │
│  ┌──────────────────────────────────────────────────────────────┐    │
│  │ FocusManager:                                                │    │
│  │   enum AppFocus { ChatInput | ScreenStack | Canvas(BlockId) }│    │
│  └──────────────────────────────────────────────────────────────┘    │
│  ┌──────────────────────────────────────────────────────────────┐    │
│  │ savvagent-canvas crate (ContentRenderer impl)                │    │
│  │   - Owns Blitz instance per BlockId                          │    │
│  │   - render() → Frame { width, height, format, bytes }        │    │
│  │   - dispatch(InputEvent) → InputOutcome { effects, dirty }   │    │
│  │   - freeze() / thaw()                                         │    │
│  └──────────────────────────────────────────────────────────────┘    │
│                                                                      │
│  ratatui-image places Frame buffers into the transcript cells via    │
│  the terminal's image protocol (Kitty/iTerm2/WezTerm/Ghostty/sixel). │
└──────────────────────────────────────────────────────────────────────┘
```

## SPP wire protocol changes

Add a new content block variant:

```jsonc
{ "type": "html", "source": "<!doctype html>...</html>" }
```

Acceptable in `messages[].content[]` (request — for round-tripping a
prior turn's HTML back into context) and in `CompleteResponse.content[]`
(response — primary case).

Mirroring streaming, add to `StreamEvent`:

```text
content_block_start { index: 2, block: html("") }
content_block_delta { index: 2, delta: html_source_delta("<!doctype html>") }
content_block_delta { index: 2, delta: html_source_delta("<head>...") }
content_block_stop  { index: 2 }
```

`html_source_delta` carries raw HTML text fragments. The TUI's streaming
preview reassembles these into the visible source-code-preview shown in
place of the eventual rendered canvas.

SPP version bump to v0.2.0. Providers that do not produce HTML blocks
remain conformant — the field is additive.

### Provider-side extraction

Models are prompted to emit HTML inside a sentinel-fenced block in their
text output:

````
Here's the plan you asked for:

```html-canvas
<!doctype html>
<html>
  <head><style>...</style></head>
  <body>...</body>
</html>
```

That should give you a structured view.
````

The provider crate (each of `provider-anthropic`, `provider-openai`,
`provider-gemini`, `provider-local`) parses the streaming text for
` ```html-canvas` opening fences and ` ``` ` closing fences, splitting
the streamed content into `Text` and `Html` content blocks in order.

Sentinel choice rationale: `html-canvas` is unambiguous, won't collide
with code samples about HTML, and survives intact through providers
that emit code blocks natively. The same provider parses other
existing fence languages (`rust`, `python`, etc.) without ambiguity.

The extraction logic lives in a shared crate (`savvagent-fence`,
created in this work) so all four providers reuse the same parser
state machine. Providers that gain "native" HTML blocks (multi-modal
output) in the future can bypass the parser and emit `Html` blocks
directly.

## Tool-emitted HTML

> *Phase 2 amendment. Not in the original 2026-05-21 spec.*

A second producer of `ContentBlock::Html` is MCP tools.

> **Implementation amendment (2026-05-25):** The original spec assumed a
> `{"type":"html","source":"..."}` content item. The pinned `rmcp`
> (1.6.0) `RawContent` enum has no `html` variant (`Text`, `Image`,
> `Resource`, `Audio`, `ResourceLink` only), so that shape is not
> representable without forking rmcp. Tool-emitted HTML instead rides
> the standard MCP **embedded resource** primitive with
> `mimeType: "text/html"` — an idiomatic, fork-free carrier. A tool
> emits:
>
> ```json
> {
>   "content": [
>     {"type": "text", "text": "Wrote 3 files. Diff:"},
>     {"type": "resource", "resource": {
>        "uri": "canvas://tool-output",
>        "mimeType": "text/html",
>        "text": "<!doctype html><html><body>…</body></html>"
>     }}
>   ]
> }
> ```
>
> The host detects `Resource` content items whose
> `TextResourceContents.mime_type == Some("text/html")` and turns each
> into a `ContentBlock::Html { source: <text> }`. Non-html resources
> and unknown content types fall through to the existing text-flatten
> path. The conceptual contract below ("html items each become their
> own `ContentBlock::Html`") is unchanged; only the wire detection
> differs.

Conceptually a tool's result content array maps to a sequence of
content blocks:

```json
{
  "content": [
    {"type": "text", "text": "Wrote 3 files. Diff:"},
    {"type": "resource", "resource": {"mimeType": "text/html", "text": "<!doctype html>…", "uri": "canvas://x"}}
  ]
}
```

`ToolRegistry::call` (in `savvagent-host`) walks the returned
content array. Today it concatenates `text` items into a single
`ContentBlock::Text`. Phase 2 extends this:

- `text` items continue to concatenate into a `Text` block.
- `html` items each become their own `ContentBlock::Html { source }`
  block, preserving the per-item HTML source verbatim. **The host
  assigns the `ContentBlockId`** — tools do not (and cannot) know
  about it. The id is allocated from the same monotonic counter the
  host uses for model-emitted blocks, so ids stay unique within a
  turn regardless of who produced the block.
- Block order in the output matches the order the tool emitted them
  (text-then-html, text-html-text, etc.).
- Unknown content types are stringified into a fallback `Text` block
  with a one-line warning, matching today's "ignore unknown" behavior.

This bypasses the fence parser entirely — the tool's HTML is already
typed; no sentinel scanning is needed. The host treats the source
identically to model-emitted HTML once it's in `ContentBlock::Html`
form: the registered `internal:html-canvas` renderer takes over from
there. **Subset enforcement is also identical** — the same advisory
validator runs and emits the same `tracing::warn!` lines for tool
HTML as for model HTML. We do NOT escalate to errors for tool HTML
even though "tools should know better": uniformity beats discipline
here, and tool bugs are fixable. The warnings give tool authors the
same diagnostic signal model prompt-tuners get.

### Tool author contract

A tool that emits HTML promises:

- The source is a complete document. Partial fragments are not
  supported; the renderer parses fresh on each `restore_state`
  cycle.
- The HTML stays within the documented subset (§ *HTML+CSS subset*).
  Subset violations get a `tracing::warn!` from the renderer (same
  treatment as model-emitted HTML); they do not fail the tool call.
- No network resources (`http://`, `https://`, `file://`). Inline
  styles and `data:` URIs only.
- The tool does NOT include `\`\`\`html-canvas` fences inside the
  source — those are a *streaming-text* convention for model output;
  tool HTML is already structurally typed.

### Streaming

Tool calls return synchronously today; there is no streaming-tool-
result delivery path. When that lands (separate feature), each
`html` content item maps to a sequence of `HtmlSourceDelta`s
identically to model-emitted streaming HTML. The renderer is unaware
of the source.

## Plugin trait extension

The v0.9.0 plugin trait surface (`savvagent-plugin`) is extended with a
new contribution kind: **content renderers**. This is the *only* shape
plugins can use to render non-text content into the conversation log.
The HTML canvas is the first built-in plugin against this kind.

### New types in `savvagent-plugin`

```rust
// crates/savvagent-plugin/src/content.rs (new)

/// A WIT-portable image frame produced by a ContentRenderer.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub bytes: Vec<u8>,
}

pub enum PixelFormat { Rgba8, Bgra8 }

pub struct PixelSize { pub width: u32, pub height: u32 }

pub struct ContentBlockId(pub u32);

pub enum InputEvent {
    Key(KeyEventPortable),
    Mouse(MouseEventPortable),
    Focus(FocusKind),
}

pub enum FocusKind { Gained, Lost }

pub struct MouseEventPortable {
    pub kind: MouseEventKind,
    pub button: Option<MouseButton>,
    pub x_pixel: u32,    // pixel offset within the rendered frame
    pub y_pixel: u32,
    pub modifiers: KeyMods,
}

pub enum MouseEventKind { Press, Release, Move, ScrollUp, ScrollDown }
pub enum MouseButton { Left, Middle, Right }

/// Outcome of dispatching an input event to a ContentRenderer.
pub struct InputOutcome {
    /// Effects the host should apply (e.g., `OpenUrl` when a link is
    /// clicked, `PromptSend` if the model offered a "send this back"
    /// affordance).
    pub effects: Vec<Effect>,
    /// True iff the renderer's frame needs re-rendering.
    pub dirty: bool,
}

pub struct FocusableElement {
    /// Opaque plugin-defined identifier (e.g., DOM node ID).
    pub id: String,
    /// Bounding box within the rendered frame, for the host to draw
    /// focus chrome if desired.
    pub bounds: Rect,
}

pub struct Rect { pub x: u32, pub y: u32, pub width: u32, pub height: u32 }

/// New `Effect` variants needed for canvas interactions:
//   Effect::OpenUrl { url: String, target: UrlTarget }
pub enum UrlTarget {
    /// Hand off to the user's system browser via `open` / `xdg-open`.
    SystemBrowser,
    /// Send the URL to the model as a new prompt (e.g., "open this
    /// file in savvagent: <url>").
    ContinueConversation,
}
```

### New trait

```rust
// crates/savvagent-plugin/src/content.rs (continued)

#[async_trait::async_trait]
pub trait ContentRenderer: Send {
    /// Stable identifier for this renderer instance (matches the
    /// `ContentBlockId` the host assigned).
    fn id(&self) -> ContentBlockId;

    /// Render the current state at the given size. Returns the
    /// content's natural height in pixels at the requested width.
    fn render(&mut self, size: PixelSize) -> Frame;

    /// Dispatch an input event. Returns side effects + dirty bit.
    async fn dispatch(
        &mut self,
        event: InputEvent,
    ) -> Result<InputOutcome, PluginError>;

    /// Soft-freeze: stop dispatching events, keep DOM state.
    fn freeze(&mut self);

    /// Resume from soft freeze. After thaw, the next `render` call
    /// should produce a frame consistent with the pre-freeze state.
    fn thaw(&mut self);

    /// Current focusable elements, in tab order. Host uses this for
    /// focus traversal (Tab / Shift-Tab) and for drawing focus
    /// indicators around the active element.
    fn focusable_elements(&self) -> Vec<FocusableElement>;

    /// Current focused element index into `focusable_elements()`, or
    /// `None` if nothing is focused.
    fn focused_index(&self) -> Option<u32>;

    /// Move focus to the element at the given index. The host calls
    /// this when the user Tabs through elements.
    fn set_focus(&mut self, index: Option<u32>);

    /// **Phase 2 amendment.** Serialize the renderer's interactive
    /// state (form values, scroll offsets, expanded `<details>` set,
    /// focused-element id) to an opaque byte blob. Returns `None`
    /// when there is nothing recoverable, including:
    /// - The document has no focusable or stateful elements.
    /// - All persistable state is at its initial value (every form
    ///   field empty, no `<details>` open, nothing focused, scroll
    ///   at the origin). A `None` return is cheaper than serializing
    ///   an empty-everything blob and equivalent under restore.
    /// - The canvas is still streaming its source (the host should
    ///   not call `snapshot_state` on a streaming canvas; the
    ///   renderer returns `None` defensively if it does).
    ///
    /// The bytes are persisted in the transcript JSON alongside the
    /// source.
    ///
    /// Default returns `None` so plugins authored against the
    /// Phase 1 trait surface compile against the Phase 2 trait
    /// without code change.
    fn snapshot_state(&self) -> Option<Vec<u8>> { None }

    /// **Phase 2 amendment.** Restore renderer state previously
    /// produced by `snapshot_state`. Called by the host after
    /// constructing the renderer from source on `/resume`. The
    /// renderer is free to interpret the bytes however it likes;
    /// the host treats them as opaque.
    ///
    /// Returns [`PluginError::StateRestoreFailed`] if the bytes are
    /// corrupt or schema-incompatible. The host falls back to "no
    /// restored state" and logs a warning; the renderer proceeds as
    /// if newly constructed (i.e. with whatever defaults `new(source)`
    /// produced).
    ///
    /// Default returns `Ok(())` (no-op) so plugins authored against
    /// the Phase 1 trait surface compile against the Phase 2 trait
    /// without code change.
    fn restore_state(&mut self, _bytes: &[u8]) -> Result<(), PluginError> {
        Ok(())
    }
}
```

#### New `PluginError` variant

```rust
// crates/savvagent-plugin/src/error.rs
pub enum PluginError {
    // ... existing variants ...

    /// `ContentRenderer::restore_state` could not interpret the
    /// supplied bytes (corrupt, schema-incompatible, or the renderer's
    /// own decoder returned an error). The host treats this as a
    /// soft failure: log a warning, drop the bytes, continue rendering
    /// from defaults.
    StateRestoreFailed(String),
}
```

The `String` is a free-form renderer-supplied reason ("expected JSON,
got binary"; "schema v2 not understood by this build"; etc.) included
in the warning log to aid debugging.

### Plugin manifest extension

```rust
// crates/savvagent-plugin/src/manifest.rs

pub struct Contributions {
    pub slash_commands: Vec<SlashSpec>,
    pub screens:        Vec<ScreenSpec>,
    pub themes:         Vec<ThemeEntry>,
    pub providers:      Vec<ProviderSpec>,
    pub hooks:          Vec<HookKind>,
    pub slots:          Vec<SlotSpec>,
    pub keybindings:    Vec<KeybindingSpec>,
    pub content_renderers: Vec<ContentRendererSpec>,     // NEW
    pub prompt_segments:   Vec<SystemPromptSegment>,     // NEW (see § Prompt contention)
}

pub struct ContentRendererSpec {
    /// Content block type tag this renderer handles, e.g., "html".
    /// Matches the SPP `type` discriminator.
    pub block_type: String,
    /// Whether this renderer is the canonical handler for the block
    /// type. Two plugins both claiming canonical is a startup error.
    pub canonical: bool,
}

pub struct SystemPromptSegment {
    /// Stable identifier of the form `<plugin_id>:<segment_name>`,
    /// used by `SlashSpec::suppress_prompt_segments` to drop the
    /// segment for a specific slash command's turn.
    pub id: String,
    /// The prompt text. Concatenated with other segments + the host
    /// default in the order plugins were registered.
    pub text: String,
}

// SlashSpec gains an optional suppression list:
pub struct SlashSpec {
    pub name: String,
    pub summary: String,
    pub args_hint: Option<String>,
    /// Prompt segment IDs to drop from the system prompt when this
    /// slash is invoked. Empty by default.
    pub suppress_prompt_segments: Vec<String>,
}
```

### Plugin trait additions

```rust
// crates/savvagent-plugin/src/plugin.rs

#[async_trait::async_trait]
pub trait Plugin: Send + Sync {
    // ... existing methods unchanged ...

    /// Factory for a content renderer instance. Called when the
    /// conversation log encounters a content block whose `type`
    /// matches one of this plugin's `ContentRendererSpec`s.
    fn create_renderer(
        &self,
        block_type: &str,
        id: ContentBlockId,
        source: &str,
    ) -> Result<Box<dyn ContentRenderer>, PluginError> {
        let _ = (block_type, id, source);
        Err(PluginError::ContentRendererNotFound(block_type.to_string()))
    }
}
```

### WIT portability

All new types follow v0.9.0's rules: owned data only, explicit-width
numerics, closed enums, async restricted to `async_trait`. The `Frame`
buffer is `Vec<u8>` which is WIT-portable as `list<u8>` (the v1.0 WIT
port will accept the copy cost; for the in-process path in v0.X.0
there's no copy).

`savvagent-plugin/Cargo.toml` continues to omit ratatui/crossterm/
tokio-runtime/anyhow.

## savvagent-canvas crate

```
crates/savvagent-canvas/
    Cargo.toml          # blitz; ratatui-image NOT here (TUI owns that)
    src/
        lib.rs
        canvas.rs       # HtmlCanvas impl ContentRenderer
        plugin.rs       # HtmlCanvasPlugin impl Plugin
        subset.rs       # Subset validator + lint warnings
        coords.rs       # Cell ↔ pixel coordinate translation helpers
```

Dependencies:

```toml
[dependencies]
savvagent-plugin = { workspace = true }
blitz = { workspace = true }                # version pin TBD by spike
async-trait = { workspace = true }
tracing = { workspace = true }
```

`HtmlCanvas` owns a single Blitz instance for one HTML doc. Its state:

```rust
pub struct HtmlCanvas {
    id: ContentBlockId,
    source: String,
    renderer: blitz::Renderer,         // exact API TBD by spike
    dom_state: blitz::DocumentState,
    frozen: bool,
    last_frame_size: Option<PixelSize>,
    focused_node: Option<NodeId>,
}
```

`HtmlCanvasPlugin` is a stateless factory exposing the `Plugin` trait:

```rust
pub struct HtmlCanvasPlugin;

#[async_trait::async_trait]
impl Plugin for HtmlCanvasPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: PluginId("internal:html-canvas".to_string()),
            name: "HTML canvas".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            description: "Renders model-authored HTML inline.".to_string(),
            kind: PluginKind::Optional,
            contributions: Contributions {
                content_renderers: vec![ContentRendererSpec {
                    block_type: "html".to_string(),
                    canonical: true,
                }],
                ..Default::default()
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
            "html" => Ok(Box::new(HtmlCanvas::new(id, source)?)),
            other => Err(PluginError::ContentRendererNotFound(other.to_string())),
        }
    }
}
```

## TUI integration

### Conversation log items

```rust
// crates/savvagent/src/app.rs (modified)

pub enum LogItem {
    Text(StyledText),
    ToolCall(ToolCallView),
    Canvas {
        id: ContentBlockId,
        source_preview: Option<String>,  // Some() while streaming
    },
}
```

The existing `App` owns a `Vec<LogItem>` (currently a `Vec<StyledText>`).
The `CanvasRegistry` lives alongside:

```rust
pub struct CanvasRegistry {
    renderers: HashMap<ContentBlockId, CanvasEntry>,
    next_id: u32,
    image_picker: Option<ratatui_image::Picker>,
}

pub struct CanvasEntry {
    renderer: Box<dyn ContentRenderer>,
    image: Option<ratatui_image::StatefulImage>,
    last_size: PixelSize,
    state: CanvasState,
}

pub enum CanvasState { Streaming, Active, Frozen }
```

### Image protocol emission

The conversation log render path:

1. Walk visible `LogItem`s.
2. For `LogItem::Text` / `LogItem::ToolCall` — render as today.
3. For `LogItem::Canvas { id, source_preview }`:
   - If `source_preview.is_some()`: render as a syntax-highlighted code
     block (re-use the existing markdown renderer's code-block path).
   - Else: look up `canvas_registry.renderers[id]`. If absent
     (renderer dropped or disabled), render the source as code.
     If present, ask the renderer for a `Frame` at the current width
     (cell-width × cell-pixel-width), upload to `ratatui-image`, and
     render the `StatefulImage` at the computed cell region.

`ratatui-image::Picker::from_query_stdio()` is invoked once at startup
to detect protocol support. Without a working protocol, `image_picker`
is `None` and all canvases render as source code with a one-line
warning banner.

### Focus model

```rust
pub enum AppFocus {
    ChatInput,
    ScreenStack,                  // existing v0.9.0 screens take input
    Canvas(ContentBlockId),
}
```

Transitions:

| From → To | Trigger |
|-----------|---------|
| ChatInput → Canvas | Mouse click within a canvas region |
| ChatInput → Canvas | Ctrl-J (jump to next canvas in log) |
| Canvas(a) → Canvas(b) | Mouse click in another canvas; Ctrl-J / Ctrl-K |
| Canvas → ChatInput | Esc |
| Any → ScreenStack | `Effect::OpenScreen` |
| ScreenStack → previous | `Effect::CloseScreen` |

On `Canvas(a) → *` the registry calls `renderer.freeze()` on `a`. On
`* → Canvas(b)` it calls `thaw()` if previously frozen.

### Keyboard routing

When `AppFocus == Canvas(id)`:

- `Tab` / `Shift-Tab` → `renderer.set_focus(next/prev index of focusable_elements())`.
- `Enter` / `Space` on a focused link/button → dispatch as
  `InputEvent::Key`, plugin returns `Effect::OpenUrl` etc. if a link.
- `Arrow keys` → `InputEvent::Key`; renderer scrolls overflow
  containers or moves caret in inputs.
- `Esc` → unfocus (`AppFocus = ChatInput`).
- `Ctrl-J` / `Ctrl-K` → jump to next/prev canvas (host-level, not
  routed into the renderer).
- `Ctrl-O` → "open in browser" (Phase 2 — writes the canvas to a
  temp file and shells `xdg-open` / `open` / `start`).

#### Precedence with `KeyScope::OnFocusedCanvas`

> *Phase 2 amendment. The original spec defined the scope but didn't
> spell out the precedence rule.*

Plugins can register keybindings scoped to `OnFocusedCanvas` via the
existing `KeybindingSpec` mechanism. Built-in canvas keys (`Tab`,
`Shift-Tab`, `Esc`, `Ctrl-J`, `Ctrl-K`, `Ctrl-O`) **take precedence**
over any plugin-registered binding in this scope. The host runs the
built-in matcher first; only on a miss does it look at plugin-
contributed bindings.

Plugin authors should register non-conflicting bindings only. Plugin
registration of a conflicting binding is not a startup error (would
break composability across plugins users mix-and-match) but it has no
effect, and the host emits a debug-level log on first conflict.

### Mouse routing

Crossterm mouse mode is enabled at startup (already set up for
ratatui-image's mouse interaction in v0.9.0+). On each mouse event:

1. Cell coordinates from crossterm.
2. The TUI knows the cell rect each visible canvas occupies (computed
   during render). If the cell is inside a canvas:
   - Translate cell-rel coord → pixel-rel coord using the canvas's
     `last_size` and the cell-pixel ratio reported by
     `ratatui-image::Picker`.
   - If the event is a `Press` and the canvas is not focused, transition
     `AppFocus = Canvas(id)`.
   - Dispatch `InputEvent::Mouse(MouseEventPortable { ... })`.
   - If `InputOutcome::dirty`, re-render the frame and re-emit via
     `ratatui-image` (Kitty's image-replace path keeps the image ID).
3. If not in any canvas: pass through to chat input / transcript scroll.

### Focus chrome

The host draws a 1-cell-wide border around the focused canvas using
ratatui. The renderer's frame stays unchanged; the chrome is painted by
the TUI in the cells immediately surrounding the canvas. This avoids
re-rendering the frame just for focus state.

Within the canvas, the renderer draws focus on the focused element
itself (via `:focus` CSS), as a browser would.

## HTML+CSS subset

The model is prompted to emit HTML using *only* the elements and
properties in this list. The renderer is lenient — out-of-subset
content may render correctly or may render degraded, but the prompt
contract does not advertise it.

### Document structure

`<!doctype html>`, `<html>`, `<head>`, `<title>`, `<meta charset>`,
`<style>`, `<body>`.

### Block-level elements

`<h1>`–`<h6>`, `<p>`, `<blockquote>`, `<pre>`, `<hr>`, `<div>`,
`<section>`, `<article>`, `<header>`, `<footer>`, `<main>`, `<nav>`,
`<aside>`, `<figure>`, `<figcaption>`.

### Inline elements

`<span>`, `<a href>`, `<em>`, `<strong>`, `<code>`, `<kbd>`, `<mark>`,
`<small>`, `<sub>`, `<sup>`, `<br>`, `<time>`, `<abbr>`, `<dfn>`,
`<q>`, `<cite>`.

### Lists & tables

`<ul>`, `<ol>`, `<li>`; `<dl>`, `<dt>`, `<dd>`; `<table>`, `<thead>`,
`<tbody>`, `<tfoot>`, `<tr>`, `<th>`, `<td>`, `<caption>`,
`<colgroup>`, `<col>`.

### Interactive elements

`<a href>` — link follow via `Effect::OpenUrl` (target chosen at
host-config time; default `SystemBrowser`).

`<details>` / `<summary>` — expand/collapse via Enter or click.
*(Phase 1 quirk: Blitz 0.3.0-alpha.4 paints the `<details>` body regardless
of the `open` attribute; see "Approach risks" for context and mitigation.)*

`<button>` — focusable; click dispatches `Effect::OpenUrl` only if its
`data-href` attribute is present (the "actionable button" pattern). All
other buttons are visual.

`<form>`, `<input>` (text/number/email/url/password/checkbox/radio),
`<textarea>`, `<select>` / `<option>`, `<label>`, `<fieldset>`,
`<legend>`. Form submission emits an `Effect::OpenUrl` with a
synthesized URL embedding form values if the form has an `action`
attribute; otherwise form state is local-only.

### Media

`<img src>` — `data:` URIs only in v1. Network URIs are *rejected*
(the renderer paints a placeholder with the URL printed inside);
local file URIs may be supported in a follow-on spec.

`<svg>` — supported as inline SVG via Blitz's existing SVG path.

### Styling

`<style>` blocks within `<head>`; inline `style="..."` attributes on
any element. **No `<link rel="stylesheet">`** — external stylesheets
are not loaded.

CSS properties: whatever Blitz handles in its current release. The
prompt advertises a conservative subset (display, flex/grid,
padding/margin, color, background, border, font-*, line-height,
overflow:auto, border-radius, box-shadow, opacity, position:relative,
transform: translate/scale, transition on `:hover`/`:focus`/`:active`,
the four pseudo-classes, `::before`/`::after`). Media queries are not
in scope in v1 (canvas size is fixed at terminal width).

**White-background default.** Model-authored canvases without an explicit
`body { background: … }` rule render with a white background, which contrasts
poorly on dark TUI themes; authors should style their documents appropriately
(e.g. `body { background: #1e1e1e; color: #cdd6f4; }`).

### Excluded

`<script>`, `<iframe>`, `<object>`, `<embed>`, `<video>`, `<audio>`,
`<canvas>` (the element — not to be confused with this feature's
"canvas"), `<link rel="stylesheet">`, `<style>` with `@import`,
network-fetched fonts (`@font-face` with remote `src`). Anything
requiring JavaScript execution.

### Subset validator (advisory)

`savvagent-canvas::subset` walks the parsed DOM and emits warnings
(via `tracing::warn!`) for out-of-subset elements/attributes/properties.
Not a render error — the canvas still draws. Warnings surface in the
log for developers debugging model output.

## Streaming

While the model is streaming the HTML source:

1. On `content_block_start { type: "html" }`: TUI appends a
   `LogItem::Canvas { id, source_preview: Some(String::new()) }`. No
   renderer is created yet.
2. On each `content_block_delta { html_source_delta(s) }`: append `s` to
   `source_preview`. The TUI re-renders the source-preview block: a
   monospace code block with syntax highlighting (re-using the
   existing markdown code-fence path), styled with a colored left
   border to signal "rendering pending."
3. On `content_block_stop`:
   - The TUI moves the accumulated `source_preview` text into the
     persistent transcript as the canvas's `source`.
   - It calls `canvas_registry.create(id, source)` which asks the
     `internal:html-canvas` plugin to construct a renderer.
   - `LogItem::Canvas { id, source_preview: None }` replaces the entry.
   - Next render frame swaps the source-preview code block for the
     rendered image.

This means time-to-first-pixel is `time_to_first_token` (the user sees
the source appearing token-by-token). Time-to-rendered-canvas is
`time_to_stream_complete + render_latency`. Render latency is the
single Blitz layout+paint pass for the finished doc.

Future work (not v1): mid-stream layout passes on a debounce.

## Lifecycle and soft freeze

- **Creation:** `content_block_stop` on a streaming HTML block.
- **Active:** receives all input events; re-renders on dirty.
- **Frozen:** `AppFocus` left this canvas. Renderer stops dispatching
  events. The image stays in the terminal's image protocol cache
  unchanged. DOM state (form values, scroll offsets, expanded
  `<details>`, etc.) is retained in the Blitz instance.
- **Active (resumed):** `AppFocus` returned. `renderer.thaw()` is
  called; events resume.
- **Memory note:** a frozen canvas holds its Blitz instance — DOM,
  style tree, layout tree, paint commands. For a typical spec/plan
  doc this is on the order of single-digit MB. Hard cap deferred to
  observation; if memory becomes an issue, a future patch can drop
  the rendered paint surface (keep DOM) and re-paint on thaw.

### Cross-restart behavior

Transcripts persist `Html { source }` blocks verbatim in the on-disk
JSON. On `/resume`, the TUI re-creates canvases from source — DOM
state from the prior session is *not* restored. This is acceptable
because interactive state (forms, scroll positions) was ephemeral
anyway; nobody expects "what I typed into the form three days ago" to
survive a restart.

## Terminal compatibility

At startup, `ratatui-image::Picker::from_query_stdio()` runs a probe
against the terminal. Outcomes:

| Outcome | Behavior |
|---------|----------|
| Kitty graphics protocol available | Use Kitty path; image-replace updates |
| iTerm2 inline images available | Use iTerm2 path |
| sixel available | Use sixel path; full-frame updates |
| None of the above | All canvases render as source code with one-line banner: "Inline rendering requires kitty / WezTerm / Ghostty / iTerm2." |

Known degraded cases:

- **tmux without `set -g allow-passthrough on`:** image escape
  sequences are eaten by tmux. We do not auto-detect this; the user
  sees rendering glitches. Documented as a known caveat in
  `README.md`; configuration snippet provided.
- **SSH sessions:** image protocols work when the local terminal
  supports them and the connection forwards escape sequences. No
  special handling.

## Provider prompting

The `internal:html-canvas` plugin contributes a `SystemPromptSegment`
(id `internal:html-canvas:default`) that is composed into the host's
system prompt when the plugin is enabled:

```
When responding to the user with a structured document — plan, spec,
status update, design review, comparison table, anything where visual
hierarchy and scannability matter — prefer HTML over markdown. Wrap
the HTML in a ```html-canvas fenced block. The user's terminal
renders it inline as an interactive document with mouse and keyboard
support.

For code samples, terse replies, error messages, or output that is
destined for another system (commit messages, PR comments, files on
disk), use plain text or markdown — those are not rendered as
canvases.

Supported tags and styles: <subset description>.
Do not include <script> tags. Do not reference external stylesheets
or fonts. Use only data: URIs for images.
```

The subset description in the prompt is a curated subset of the full
subset (§ HTML+CSS subset) — favors the elements models reliably
produce well. The full subset is the renderer's compatibility surface;
the prompt advertises a tighter circle.

The precise scoping language ("structured document … *vs.* output
destined for another system") is the first line of defense against
contention with slash commands and agents whose work product is
markdown by nature. The per-slash suppression mechanism (§ Prompt
contention) is the explicit second line.

## Reviewing existing artifacts

This feature renders HTML the *model emits*, not HTML it reads from
disk. There is no automatic "render any markdown file as HTML" path —
that would be a separate plugin (deferred; see § Out of scope).

For reviewing file-based artifacts (specs, plans, code, ADRs) the
intended flow is:

1. User asks "review `docs/superpowers/specs/2026-05-21-foo.md`".
2. Model reads the file with the `read_file` tool.
3. Model emits the *review* as an HTML canvas — sections by severity,
   quoted snippets with `<pre><code>` blocks, side-by-side comparisons
   via flex/grid, callouts for blocking issues. The model is *not*
   asked to re-render the source file; it produces a review document
   that happens to be HTML.
4. User reads the review inline. The original file on disk is
   unchanged.

For *writing* new specs/plans where both a chat-time canvas view and
an on-disk markdown file are wanted, the model uses both surfaces:
emit the markdown via the `write_file` tool (canonical artifact on
disk) *and* render an HTML canvas view inline for the user to grok in
the conversation. The system-prompt segment can include guidance to
this effect; the model is otherwise free to decide.

A future `/view-as-html <path>` slash command (a separate plugin)
would let users open arbitrary markdown files in a canvas without an
LLM round-trip. Out of scope for this spec.

## Viewing canvases outside the TUI

Three escape hatches let canvases leave the TUI for the user's real
browser or for sharing:

### Auto-export (Phase 1)

By default, every rendered HTML block is also written to disk at:

```
~/.savvagent/canvases/<unix-ts>-<turn_id>-<block_id>.html
```

The file is self-contained: it includes the HTML source verbatim with
no rewriting. Auto-export is **on by default**. To disable it, set
`enabled = false` for `internal:html-canvas` in `plugins.toml`:

```toml
[plugins."internal:html-canvas"]
enabled = false
```

v0.17.0 ships with no separate `auto_export` toggle — the plugin's
auto-export and rendering are bundled. Disabling the plugin suppresses
both. A dedicated per-feature flag may be added in a future release if
there is demand.

The transcript JSON remains the source of truth — the on-disk
`.html` is a convenience copy. Deleting the files does not corrupt
the transcript; re-opening the transcript via `/resume` re-creates
the files (if the plugin is enabled).

The directory is created with `0o700`, files `0o600`, matching the
existing `~/.savvagent/` permission discipline.

### `/save-canvas <path>` (Phase 1)

The `internal:html-canvas` plugin contributes a `save-canvas` slash
command. Arguments:

```
/save-canvas                              # saves the most recent canvas to a default path
/save-canvas ./my-spec.html               # explicit path
/save-canvas ./spec.html --block 3        # specific canvas by index in current transcript
```

Emits `Effect::PushNote` confirming the path, plus an `Effect::OpenUrl
{ url: "file://...", target: SystemBrowser }` if the user adds
`--open`.

### Open-in-browser keybinding (Phase 2)

When focus is on a canvas, **Ctrl-O** writes the canvas to a temp
file (`/tmp/savvagent-canvas-<id>.html`) and shells out to
`xdg-open` / `open` / Windows `start`. The user's actual browser
renders it with full fidelity — useful for sharing, printing, or for
canvases that hit the edges of Blitz's subset support.

The keybinding is contributed via `KeybindingSpec` with
`KeyScope::OnFocusedCanvas` (a new scope variant; see § Plugin trait
extension).

## Prompt contention and suppression

Different parts of savvagent will reasonably want different output
formats. The HTML-canvas system prompt is opt-in by plugin enable
state and *scoped* by per-slash suppression.

### How segments compose

When constructing the system prompt for a turn, the host:

1. Starts with the default savvagent system prompt.
2. Appends project context (`SAVVAGENT.md` if present).
3. Iterates enabled plugins in registration order. For each, appends
   any contributed `SystemPromptSegment.text`.
4. If the turn is the dispatch of a `SlashSpec` and the spec lists
   `suppress_prompt_segments`, those segment IDs are filtered out.
5. The result is the final `system` field in the `CompleteRequest`.

This means:

- **Globally turning off canvas guidance:** disable
  `internal:html-canvas` in `plugins.toml`.
- **Turning off canvas guidance for one slash command:** that slash's
  `SlashSpec::suppress_prompt_segments` includes
  `"internal:html-canvas:default"`.
- **Mixed conversations:** a user can chat normally (HTML canvas on),
  then invoke `/review-pr` (canvas suppressed; model emits markdown
  for GitHub comments), then resume chatting (canvas back on for the
  next user turn).

### Built-in markdown-required slashes

The spec assumes a few built-in slash commands will suppress the
canvas segment because their output flows to systems that consume
markdown verbatim:

- `/commit` — commit messages
- `/pr` — PR titles and bodies
- (future) `/review-pr` — GitHub PR comment bodies

These are *not* shipped as part of this spec; if a slash command
already exists that conflicts, its `SlashSpec` is updated to include
the suppression in the same PR that introduces this feature.

### Sub-agent contract

> *Phase 2 amendment. The original "Agent contention (future)" section
> hedged this; Phase 2 commits to a concrete contract that future
> sub-agent code must honor. No sub-agent code ships in Phase 2; this
> section is the design.*

When savvagent grows a sub-agent concept, each sub-agent's
`CompleteRequest::system` **fully replaces** the host's composed
prompt for the duration of the sub-agent's turn. Plugin-contributed
`SystemPromptSegment`s do NOT leak into sub-agent prompts by default.

A sub-agent manifest may opt into specific segments via:

```rust
pub struct SubAgentManifest {
    // ... existing fields ...
    /// Plugin SystemPromptSegment ids to compose into THIS sub-agent's
    /// system prompt. Each item is a fully-qualified segment id in
    /// the form `"<plugin_id>:<segment_name>"` — the same string
    /// shape `SystemPromptSegment::id` uses and the same shape
    /// `SlashSpec::suppress_prompt_segments` filters on. Items that
    /// don't match any registered segment are silently ignored
    /// (logged at `debug` level for plugin authors to spot typos).
    /// Composition order: sub-agent's own system field first, then
    /// each segment in this list joined by blank lines.
    /// Empty by default.
    pub inherit_segments: Vec<String>,
}
```

The mechanism resembles the per-slash suppression (§ *Prompt
contention and suppression*) but inverted: slashes opt *out* of host
defaults; sub-agents opt *in* to specific segments.

**Why opt-in for sub-agents, opt-out for slashes**: a slash command
runs inside the same agent loop as normal chat — it inherits the
host's full prompt by default and surgically removes pieces.
A sub-agent is a different agent with its own identity, prompt, and
purpose — leaking host segments would frequently produce nonsense
(`"prefer HTML canvas output"` makes no sense when the sub-agent's
job is to emit a JSON-only summary). Default-deny is the safe
posture; inheritance is a deliberate borrow.

**Composition order is consistent with slash composition**, not
asymmetric:

| Surface | Order |
|---|---|
| Normal chat / slash | host default → project context (`SAVVAGENT.md`) → plugin segments (in registration order) → suppression filter |
| Sub-agent | sub-agent's own system field → inherited plugin segments (in `inherit_segments` order) |

Both surfaces put the "root" prompt first and append segments. The
sub-agent simply lacks the host-default + project-context layers
because it is its own root. Plugin segments always append, never
prepend.

Phase 2 ships only the spec; the code change lands with the sub-
agent feature itself. When that PR is written, it MUST honor this
contract — no leak by default, opt-in via `inherit_segments`, qualified
id strings, append-order composition.

### Multiple prompt-contributing plugins

If two plugins both contribute segments and one's instructions
contradict the other, behavior is undefined — the host concatenates
them and trusts the model. Plugin authors are expected to scope their
language precisely. Conflict detection is out of scope; this is a
"plugins-are-trust" situation in v0.9.0+ generally.

## Error handling

| Failure | Behavior |
|---------|----------|
| HTML doesn't parse (truly malformed) | html5ever auto-recovers; we render best-effort. Subset validator logs warnings. |
| Unsupported element / property | Blitz renders what it can; subset validator logs warnings. |
| `<img src="https://...">` (network URI) | Renderer paints a placeholder showing the URL in monospace. |
| Image protocol failure (terminal disconnects, IO error) | Canvas falls back to rendering source as code with a one-line error banner. Failure logged. |
| Renderer panic | Caught at the canvas boundary. Affected canvas converts to source-code fallback. Other canvases unaffected. Bug-style `tracing::error!` with stack. |
| Blitz API change at compile time | Pinned Blitz version in `[workspace.dependencies]`. Upgrades go through the same review cycle as any dep bump. |

## Persistence

### Transcript schema versions

| Version | Introduced by | What's new |
|---|---|---|
| **v1** | pre-canvas | Baseline: text, tool-call, tool-result Entries only. |
| **v2** | Phase 1 (PR #97, 2026-05-23) | Adds `Html { source }` content block; adds `Canvas` Entry variant. |
| **v3** | Phase 2 | Adds optional `state` field on `Canvas` Entries (interactive-state persistence). |

The transcript header carries an integer `schema_version` field. Older
builds reading newer transcripts must degrade gracefully (§
*Cross-build compatibility matrix* below).

### Phase 1 (v2)

- Transcripts JSON gains the new `Html` content block type. The block
  carries `{ type: "html", source: "..." }`. Existing transcripts
  load unchanged (no `Html` blocks present).
- The on-disk schema version moves from v1 → v2 to signal the new
  block + Entry types. Older builds loading newer transcripts log a
  warning and render `Html` blocks as raw source.

### Interactive-state persistence (v3)

> *Phase 2 amendment. The original spec said interactive state is NOT
> persisted; Phase 2 adds it.*

The `Canvas` Entry variant in the transcript JSON gains an optional
opaque `state` field:

```json
{
  "type": "canvas",
  "id": 42,
  "source": "<!doctype html>...",
  "state": "<base64-encoded opaque blob>"
}
```

- The blob is produced by `ContentRenderer::snapshot_state()` (see
  § *New trait*) at transcript save time, and consumed by
  `restore_state()` at `/resume` time.
- The host treats the bytes as opaque. The encoding inside is the
  renderer's choice; `HtmlCanvas` serializes a `serde_json`-shaped
  struct of `{ form_values: Map<NodeId, FormValue>, scroll: Map<NodeId,
  (u32, u32)>, open_details: Set<NodeId>, focused: Option<NodeId> }`.
- **NodeId stability is a load-bearing assumption that this spec
  does not yet prove.** The expectation is that Blitz (via html5ever)
  assigns node ids deterministically — the same source parses to the
  same ids across processes — so a snapshot taken in session A can
  be restored in session B after re-parsing the same source.
  html5ever is a tree builder, so the assumption is plausible, but
  Blitz's specific id-assignment scheme (`NodeId(u32)` from a
  monotonically-incremented counter inside `BaseDocument`) was not
  verified to be process-deterministic during the Phase 0 spike.
  **The Phase 2 plan MUST include a verification task** — a short
  mini-spike that parses the same HTML in two processes and asserts
  byte-equal NodeId-to-element mapping. If the assumption fails,
  `HtmlCanvas` falls back to keying on `(tag, nth-of-type-among-
  siblings)` or a CSS-selector-style path instead of NodeId. If
  the source changes between save and resume (it shouldn't — the
  source is in the same JSON record), `restore_state` best-effort
  applies what still matches and logs a warning for the rest.
- The transcript schema version bumps v2 → v3. Older builds (v1, v2)
  loading v3 transcripts ignore `state` and render the source fresh
  — graceful fallback, no data loss except interactive state.
- A snapshot is taken at: `TurnComplete`, before manual `/save`,
  and at clean TUI shutdown. A snapshot is NOT taken on every input
  event (too expensive — events are frequent and most don't change
  persistable state).
- A snapshot is NOT taken for canvases whose source is still
  streaming (`source_preview.is_some()`). The host skips them; their
  `Canvas` Entry serializes with `state` absent.

#### State-loss tradeoff

The snapshot triggers above mean any state change that happens *between*
the last TurnComplete (or `/save`) and an unclean exit (kill -9,
SIGSEGV, power loss) is lost. Concretely: a user expands a
`<details>` three turns after the canvas was created, then the TUI
crashes before the next TurnComplete — that expansion is gone on
`/resume`.

This is an accepted tradeoff. The alternatives — snapshot on every
input event (expensive: input events fire at ~60 Hz from mouse-move
debouncing) or snapshot on a debounce timer (added complexity for
limited recovery benefit) — both pay regular cost for a rare loss.
Real-world interactive state is also re-creatable: a `<details>` re-
expansion is one click. If a user reports losing meaningful state to
a crash, we revisit; for v1 we accept the gap.

### Cross-build compatibility matrix

| Build → reading transcript ↓ | Pre-canvas (v1) | Phase 1 (v2) | Phase 2 (v3) |
|---|---|---|---|
| Pre-canvas build | works | **load error unless `serde(other)` fallback present — see below** | same error mode |
| Phase 1 build    | works (older format) | works | works; silently ignores `state` field (no consumer) |
| Phase 2 build    | works | works (no `state` to restore — initial defaults) | works (full restore) |

#### Pre-canvas compatibility requires a serde fallback

A pre-canvas-build's `Entry` enum has variants like `Text`,
`ToolCall`, `ToolResult` — but no `Canvas`. Default `serde_json`
deserialization of an externally-tagged enum **fails** when it
encounters an unknown tag. So a pre-canvas build loading a v2 or v3
transcript would `Err` on the first `Canvas` Entry, not "warn and
render as source" as I initially wrote.

For graceful pre-canvas degradation, the `Entry` enum needs a
`#[serde(other)]` (or equivalent) variant that absorbs unknown
tags and renders them as a degraded "[unknown entry type]" placeholder
in the transcript view.

**This is a Phase 1 followup, not Phase 2 work** — but it must be
noted here because the pre-canvas row of the matrix above can only
be honest about "graceful degradation" if the fallback ships. Without
it, pre-canvas users can't load any post-canvas transcript without an
error. If we don't backport the fallback to a Phase-1 dot release, the
matrix above must drop the "graceful" claim and the pre-canvas row
must say "load error."

The Phase 2 plan will:

1. Add `#[serde(other)] Unknown` (or similar — pick the cleanest
   serde idiom for tagged enums) to `Entry` in `savvagent-protocol`.
2. Cut a Phase-1 dot release that includes only that backport
   (matches the v0.16.x line) so existing v0.16.x users get
   graceful loading before Phase 2's v0.17.0 hits.

## Testing strategy

### Unit tests

In `crates/savvagent-canvas/src/`:

- `subset` validator: known good docs produce zero warnings; known bad
  docs produce expected warnings.
- Coordinate translation (`coords.rs`): cell-pixel ratio computations;
  edge cases at canvas boundary.
- `HtmlCanvas::dispatch` for synthetic events: focus traversal Tab/
  Shift-Tab walks `focusable_elements` in order; `set_focus` updates
  `focused_index`; mouse-press at a known pixel coord lands on the
  expected element.

In `crates/savvagent-plugin/src/`:

- New types compile under WIT portability rules (no ratatui/crossterm
  imports added).
- `Plugin::create_renderer` default impl returns the expected error
  variant.

In `crates/savvagent/src/`:

- `CanvasRegistry`: create → freeze → thaw → drop lifecycle.
- `AppFocus` transitions: mouse click in canvas region focuses;
  Esc unfocuses; Ctrl-J/Ctrl-K jumps in correct order.
- Streaming: `content_block_start` → deltas → `content_block_stop`
  produces a `Canvas` LogItem with the rendered source.

### Integration tests

- **Headless render snapshot.** A small set of canonical HTML docs
  (one spec-shaped doc, one diff view, one expandable plan, one form,
  one with `<details>`) renders to a frame; the frame is captured
  and snapshot-tested. Run on CI with a feature flag that enables a
  software rasterizer for Blitz to keep results deterministic.
- **Mouse round-trip.** Drive a fake terminal: receive a mouse press
  at cell coords, assert the right canvas was focused, assert
  `InputEvent::Mouse` reached the renderer with the right pixel
  coords. (No actual image-protocol emission; the bytes are
  swallowed.)

### Manual cross-terminal verification

A test matrix run before each release:

| Terminal | Protocol | Expected |
|----------|----------|----------|
| Kitty | kitty graphics | full render + interaction |
| WezTerm | iTerm2 protocol | full render + interaction |
| iTerm2 | iTerm2 protocol | full render + interaction |
| Ghostty | kitty graphics | full render + interaction |
| WezTerm under tmux + `allow-passthrough on` | kitty graphics | full render + interaction |
| Alacritty | none | source-code fallback with banner |
| Tmux without passthrough | varies | known-degraded; banner suggests config |

Documented in `docs/canvas-terminal-compat.md`.

## Acceptance criteria

**SPP & host.**

1. SPP v0.2.0: `Html { source }` content block defined; `html_source_delta` stream delta defined.
2. All four provider crates (`provider-anthropic`, `provider-openai`, `provider-gemini`, `provider-local`) recognize and extract `html-canvas` fenced blocks during streaming.
3. `savvagent-host` forwards `Html` blocks through `run_turn_streaming` unchanged.
4. Transcripts JSON round-trips `Html` blocks losslessly.

**Renderer.**

5. `savvagent-canvas` crate builds in the workspace; depends on `savvagent-plugin` + Blitz.
5b. `savvagent-canvas`'s Blitz dependency bumps the workspace `rust-version` from 1.85 to
    1.89; CHANGELOG calls this out explicitly.
6. `HtmlCanvas` implements `ContentRenderer`: renders the spec's HTML+CSS subset to a pixel buffer; dispatches mouse and keyboard events; soft-freezes losslessly.

**Plugin integration.**

7. `savvagent-plugin` gains `ContentRenderer` trait, `ContentRendererSpec`, `Frame`, `InputEvent`, `MouseEventPortable`, `UrlTarget`, `Effect::OpenUrl`, `SystemPromptSegment`, `SlashSpec::suppress_prompt_segments`. All new types pass the existing CI WIT-portability grep.
8. `internal:html-canvas` plugin is registered in `register_builtins()` as Optional (toggleable via `plugins.toml`).
9. Disabling the plugin: HTML blocks render as source code; no system prompt segment composed; no Blitz instance created; no auto-export.
10. Enabled plugin's `SystemPromptSegment` (`internal:html-canvas:default`) is composed into the system prompt by the host's prompt composition step.
11. A `SlashSpec` listing `internal:html-canvas:default` in `suppress_prompt_segments` invokes the model without that segment for that turn.

**Persistence & export.**

12. Transcript JSON round-trips `Html { source }` blocks losslessly.
13. Auto-export on by default: each rendered HTML block writes to `~/.savvagent/canvases/<unix-ts>-<turn>-<block>.html` with `0o600`. The directory is created with `0o700` if missing.
14. Disabling the plugin (`enabled = false` in `plugins.toml`) suppresses both rendering and auto-export; v0.17.0 has no separate `auto_export` toggle.
15. `/save-canvas` writes the chosen canvas to a user-specified path; `--open` opens it in the system browser.
16. Phase 2: Ctrl-O while focused on a canvas opens it in the system browser via `xdg-open` / `open`.

**TUI.**

10. Conversation log renders mixed `Text` / `ToolCall` / `Canvas` items. Canvases appear inline at their conversation position.
11. Mouse click inside a canvas region focuses it; subsequent mouse events route to the renderer with correctly-translated pixel coordinates.
12. Ctrl-J / Ctrl-K jumps between visible canvases. Esc returns focus to chat input.
13. Tab / Shift-Tab traverses focusable elements within the focused canvas.
14. Streaming: the source appears typewriter-style during stream; on `content_block_stop`, the source is replaced by the rendered canvas in place.
15. Soft freeze: focus leaving a canvas pauses it; refocus resumes form values, scroll positions, expanded `<details>`.

**Terminal compat.**

16. Image-protocol detection at startup chooses Kitty / iTerm2 / WezTerm / Ghostty / sixel based on the terminal probe.
17. In unsupported terminals, HTML blocks render as source code with a banner; no rendering errors propagate to the user.

## Phasing

Two shippable releases.

### Phase 1 — Static rendering + export (one release)

- SPP v0.2.0 wire changes.
- Provider-side fence extraction (`savvagent-fence` crate).
- `savvagent-plugin` trait extensions: `ContentRenderer` trait, `Frame`, `ContentRendererSpec`, `SystemPromptSegment`, `SlashSpec::suppress_prompt_segments`, `Effect::OpenUrl`.
- Host prompt composition step that gathers `SystemPromptSegment`s from enabled plugins and honors per-slash suppression.
- `savvagent-canvas` crate with Blitz integration; `HtmlCanvas` renders to pixel buffer (no event dispatch yet).
- `internal:html-canvas` plugin registered with its default prompt segment.
- TUI: `LogItem::Canvas`, conversation log inline rendering via `ratatui-image`, source-code fallback for unsupported terminals.
- Streaming source preview → swap on stream complete.
- Auto-export to `~/.savvagent/canvases/`, configurable via `plugins.toml`.
- `/save-canvas` slash command.
- Cross-terminal test matrix run.

After Phase 1 the user gets: rich rendered HTML inline in the
transcript, with every canvas also available as a standalone `.html`
file in `~/.savvagent/canvases/` for opening in a real browser or
sharing. No interaction inside the TUI yet. This alone solves a big
chunk of the "I don't read markdown plans" problem.

### Phase 2 — Interaction + tool HTML + state persistence (one release)

> *Phase 2 amendment. Items marked "(new)" were not in the original
> spec and are added by the 2026-05-23 amendment above.*

- `InputEvent`, `MouseEventPortable`, `InputOutcome`, `FocusableElement` types. *(Phase 1 added these as Phase-2-ready stubs; Phase 2 wires them.)*
- `ContentRenderer::dispatch`, `freeze`, `thaw`, `focusable_elements`,
  `set_focus`, `focused_index` — promoted from no-op defaults to real
  implementations on `HtmlCanvas`.
- `ContentRenderer::snapshot_state` and `restore_state` — **(new)**
  two methods added to the trait surface, default to `None` /
  `Ok(())` so Phase 1 plugins continue to compile.
- `HtmlCanvas` implements the eventing surface against Blitz, with a
  **renderer-side event router** inside `dispatch` that runs the
  browser default actions Blitz's headless API does not: clicks on
  `<summary>` flip the parent `<details>`'s `open` attribute and re-
  resolve; clicks on `<a href>` emit `Effect::OpenUrl` via
  `InputOutcome::effects` rather than propagating to Blitz; form
  submission synthesizes an `Effect::OpenUrl`. If a later Blitz
  version implements default actions natively, the router shrinks to
  a pass-through; the surface contract doesn't change.
- TUI: `AppFocus::Canvas`, mouse routing, keyboard routing, Ctrl-J/K
  block traversal, Tab/Shift-Tab element traversal, Esc to unfocus,
  focus chrome.
- New keybinding scope `KeyScope::OnFocusedCanvas` for canvas-
  specific shortcuts, with built-in keys taking precedence (§
  *Keyboard routing*).
- Ctrl-O "open in browser" keybinding while focused on a canvas.
- Soft freeze on focus loss; thaw on refocus.
- Link follow via `Effect::OpenUrl`. Default `UrlTarget` is
  `SystemBrowser` for absolute URLs, `ContinueConversation` for
  relative paths (§ *Open questions* resolution).
- `<details>` expand/collapse interaction (driven by the renderer
  router, not Blitz's default action).
- Form input (text/checkbox/radio/select).
- **Tool-emitted HTML (new):** `ToolRegistry::call` translates
  MCP tool-result content items of type `html` into
  `ContentBlock::Html` blocks. See § *Tool-emitted HTML*.
- **Persistence of interactive state (new):** transcript JSON gains
  the optional `state` field on the `Canvas` Entry. `HtmlCanvas`
  serializes form values, scroll offsets, expanded-details set, and
  focused-element id. `/resume` restores state via
  `restore_state`. See § *Interactive-state persistence*.
- **Sub-agent prompt contract (new — spec-only):** firms up the
  segment-leak rules for sub-agents (§ *Sub-agent contract*). No
  code change in Phase 2; future sub-agent PR must honor.
- `release(0.17.0)` rollup commit (Phase 1's commit bumped the
  version; Phase 2's commit consolidates CHANGELOG, README,
  spec-doc cross-references).
- **Push the `v0.17.0` git tag.** cargo-dist's Release workflow
  takes over from here. Per `feedback_phase_release_rollup`, this
  is the first tag push for the inline-canvas initiative.

#### Windows CI carries forward Phase 1's exclusion

Phase 1 excluded `savvagent-canvas` and `savvagent` from the
`test (windows-latest)` CI job because Blitz's static init hangs
on the GitHub-hosted windows-latest runner image (root cause is
font enumeration via DirectWrite; the runner image lacks fonts
Blitz expects, or its enumeration path blocks). Phase 2 grows both
crates substantially but does **not** address this — the hang is in
Blitz/upstream, not in our code. The Phase 2 PR keeps the same
exclusion in `.github/workflows/ci.yml` and CHANGELOG calls it out.

A separate follow-up investigation (out of scope for Phase 2) will
attempt one of: (a) install fonts into the runner image via apt-
equivalent / vcpkg, (b) shim Blitz's font discovery to a fixed
bundled font, (c) wait for an upstream Blitz fix and re-evaluate.
Until that resolves, Linux + macOS coverage carries the Blitz-
linking test surface; local Windows dev runs continue to exercise
canvas code normally.

### Phase 0 (spike, before Phase 1 implementation begins)

- Pin a Blitz version, build a minimal example that:
  - Parses an HTML doc.
  - Lays it out at a fixed size.
  - Paints to an RGBA buffer.
  - Dispatches a synthetic click at pixel coords.
  - Reports which element was hit and any DOM state change.
- Document the actual Blitz API and any deltas from this spec's
  assumed surface. If material divergence, revise this spec before
  Phase 1 planning.

## Risk register

- **Blitz event dispatch path.** Mitigated by Phase 0 spike (run
  2026-05-21 against `blitz-* 0.3.0-alpha.4`, findings at
  `docs/superpowers/notes/2026-05-21-blitz-spike.md`). Outcome:
  synthetic events accepted, default actions not run; Phase 2 ships a
  host-side router for `<summary>`/`<a>`/form-submit.
- **Blitz MSRV bump (1.85 → 1.89).** `blitz-* 0.3.0-alpha.4` declares
  `rust-version = "1.89.0"`. Adopting it in Phase 1 forces a workspace
  MSRV bump from 1.85 to 1.89. Mitigation: call out in the Phase 1
  CHANGELOG entry; CI's `rustup show` already runs on a newer stable.
- **Image-protocol bandwidth on rapid mouse-move events.** Mitigated by
  re-render only on `InputOutcome::dirty`, by Kitty's image-replace
  semantics, and by debouncing mouse-move events at the TUI layer
  (16 ms / ~60 Hz).
- **Provider parser ambiguity.** A model emitting `\`\`\`html` (without
  `-canvas`) when it means a code sample, vs.  `\`\`\`html-canvas`
  when it means a rendered doc. Mitigation: only the explicit
  `html-canvas` sentinel triggers extraction; plain `html` stays a
  code sample.
- **WIT portability of `Frame::bytes`.** A multi-MB `Vec<u8>` crossing
  the plugin boundary is fine in-process but lossy under WIT. v0.X.0
  ships in-process; the WIT port is the v1.0 problem. Spec calls this
  out so future-us doesn't claim WIT-portability without nuance.
- **Memory growth from frozen canvases.** Acknowledged; observation-
  driven cap deferred to a follow-on patch if it becomes an issue.
- **Tmux passthrough discoverability.** Many users will hit tmux
  passthrough as a footgun. Mitigation: README section + a startup
  hint when we detect tmux but no passthrough.
- **Prompt contention across plugins.** Multiple plugins with
  contradictory `SystemPromptSegment`s could confuse the model. v1
  trusts plugin authors to scope language precisely and provides no
  static analysis or runtime conflict detection. Mitigation: the
  built-in segment's wording is precise about *when* HTML applies vs.
  doesn't ("destined for another system → plain text/markdown"),
  reducing the conflict surface for the cases we control.

## Out of scope (deferred)

> *Phase 2 amendment: two bullets removed — "Cross-restart interactive
> state" and "Sub-agent prompt-segment inheritance" moved into Phase 2
> scope.*

- Terminal-widget rendering fallback (path A from brainstorming).
  Future spec if there's demand for low-fidelity-but-universal mode.
- Streaming layout (mid-stream re-render).
- Script execution.
- Network resources (external stylesheets, fonts, images via
  http/https URIs).
- Multi-page / paginated HTML docs.
- WASM-loaded `ContentRenderer` plugins. The trait surface is
  WIT-portable in shape; the loader is the v1.0+ problem.
- Themes that style the canvas chrome differently per app theme.
  v1 uses one focus-chrome style.
- Deterministic markdown-file viewer (`/view-as-html <path>`). A
  separate plugin spec; would let users render existing `.md` files
  as canvases without an LLM round-trip.
- Cross-plugin prompt-segment conflict detection. Plugin authors are
  expected to scope their language precisely; the host concatenates
  and trusts the model.
- Streaming tool-result delivery. Today tool calls return
  synchronously; if/when streaming tool results land, tool-emitted
  HTML naturally extends to `HtmlSourceDelta` (§ *Tool-emitted HTML*
  → *Streaming*).

## Open questions

- **Crate version of Blitz** to pin. **Resolved by Phase 0 spike
  (2026-05-21):** `blitz-dom = "=0.3.0-alpha.4"`,
  `blitz-html = "=0.3.0-alpha.4"`, `blitz-paint = "=0.3.0-alpha.4"`,
  `blitz-traits = "=0.3.0-alpha.4"`, plus `anyrender = "0.10"`,
  `anyrender_vello_cpu = "0.12"`, `peniko = "0.6"`. Spike notes at
  `docs/superpowers/notes/2026-05-21-blitz-spike.md`.
- **Default `UrlTarget`** for `<a href>` follow: **Resolved by Phase 2
  amendment (2026-05-23).** The router classifies the href by URL
  scheme and routes accordingly:

  | href shape | Disposition | Rationale |
  |---|---|---|
  | `http://...`, `https://...` | `Effect::OpenUrl { target: SystemBrowser }` | Standard web link; user expects browser open. |
  | `mailto:...` | `Effect::OpenUrl { target: SystemBrowser }` | `xdg-open`/`open` route to the default mail client. |
  | `tel:...`, `sms:...` | `Effect::OpenUrl { target: SystemBrowser }` | Same path; OS handlers route to the right app (or fail gracefully if none registered). |
  | `data:...` | **No effect emitted; log at `debug`.** | Data URLs encode content inline; shelling out is meaningless. The user clicking is almost always a misunderstanding of what the link does. |
  | `javascript:...` | **Blocked; log at `warn`.** | XSS-adjacent. The renderer never emits an effect for these even if Blitz somehow surfaces the click. |
  | `file://...` | **No effect emitted; log at `debug`.** | The subset (§ *HTML+CSS subset*) excludes file:// resources, so this should not appear; if it does, treat as a subset violation. |
  | bare path (no scheme, no `//`) — e.g. `./foo.md`, `docs/spec.md`, `foo.rs` | `Effect::OpenUrl { target: ContinueConversation }` | Model probably means "look at this file in the project." Routing it as a new user prompt is safer than blindly shelling `xdg-open` on a path that may not exist or that the user did not consent to open externally. |
  | anything else (unknown scheme) | **No effect emitted; log at `warn`.** | Conservative default: model bug or malformed URL. |
- **Whether `internal:html-canvas` is Core or Optional.** Lean:
  Optional in v1 (allow users to turn it off if Blitz misbehaves on
  their setup); promote to Core in a later release once stable.
- **System prompt subset advertisement** — how tight to make it. Lean:
  one paragraph + a short tag list, not the full subset table. Final
  wording decided when writing prompt copy.

## Implementation order (high-level — full plan in writing-plans output)

Phase 0 spike (2026-05-21, done) → Phase 1 (SPP, provider extract,
canvas crate static render, plugin trait extension, TUI integration,
streaming preview — shipped as PR #97, merged 2026-05-23) → **Phase 2**
(eventing, freeze/thaw, focus, mouse/kb routing, tool-emitted HTML,
interactive-state persistence, sub-agent contract). Each phase ships
as its own release with notes, README/CHANGELOG update, and GitHub
issue closure per the project's existing release discipline; per
`feedback_phase_release_rollup`, only the final phase (Phase 2)
pushes the `v0.17.0` git tag.
