# Inline HTML canvas — Phase 2 implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add mouse + keyboard interaction to the inline HTML canvas, soft-freeze/thaw on focus, persistence of interactive state across `/save`+`/resume`, tool-emitted HTML translation, and ship v0.17.0.

**Architecture:** Promote the Phase-2 trait stubs on `HtmlCanvas` to real implementations against Blitz alpha.4's headless eventing API, with a **renderer-side default-action interceptor** (links → `Effect::OpenUrl` via `InputOutcome::effects`; `<details>` → toggle `open` attr; form submit → effect). Add two new trait methods (`snapshot_state`, `restore_state`) with no-op defaults. TUI gains `AppFocus::Canvas { id, element_idx }`, 1-cell focus chrome, mouse hit-testing against canvas image regions, and keyboard routing where built-in canvas keys (Tab/Shift-Tab/Esc/Ctrl-J/Ctrl-K/Ctrl-O) take precedence over plugin-registered `KeyScope::OnFocusedCanvas` bindings. `ToolRegistry::call` translates MCP `{"type":"html","source":"..."}` content items into `ContentBlock::Html` blocks (host assigns `ContentBlockId`). Transcripts gain an optional opaque `state` blob on `Canvas` Entries (schema v3), captured at `TurnComplete`+`/save`+clean shutdown.

**Tech Stack:** Rust 2024, `blitz-* = "=0.3.0-alpha.4"` (unchanged from Phase 1), `tokio` (existing), `serde`+`serde_json` (existing), `crossterm` mouse mode (already enabled by ratatui-image in Phase 1), `base64` (transcript blob encoding, may already be transitive).

**Spec:** `docs/superpowers/specs/2026-05-21-inline-html-canvas-design.md` (Phase 2 amendment dated 2026-05-23). Read the Phase 2 amendment header at the top of the spec before starting; it summarizes every section this plan implements.

**Phase 1 baseline:** Phase 1 shipped via PR #97 at branch `worktree-inline-html-canvas-phase-1`. This plan continues on the same branch. The branch is currently merge-clean against `origin/master`. Phase 1 left the Phase-2 methods on `ContentRenderer` as no-op defaults; this plan promotes them to real impls.

**Release discipline:** Per `feedback_phase_release_rollup`, Phase 2 is the **final phase** of the inline-canvas initiative. After all tasks land, the `release(0.17.0)` rollup commit goes on master and the `v0.17.0` git tag IS pushed (Phase 1's commit already bumped versions; this phase just consolidates CHANGELOG + README + spec cross-refs and pushes the tag). cargo-dist's Release workflow takes over from there.

**Spec drift carryover:** Entry lives at `crates/savvagent/src/app.rs` (not in `savvagent-protocol` as the spec implies). All `Entry` references in this plan target the savvagent crate.

---

## File structure

**New files:**
- `crates/savvagent-canvas/src/coords.rs` — pixel ↔ terminal-cell coordinate helpers.
- `crates/savvagent-canvas/src/focus.rs` — focusable-element traversal of Blitz DOM.
- `crates/savvagent-canvas/src/events.rs` — synthetic event dispatch wrapping Blitz's `BaseDocument::handle_*_event`.
- `crates/savvagent-canvas/src/interceptor.rs` — default-action interceptor (links, `<details>`, forms).
- `crates/savvagent-canvas/src/state.rs` — `CanvasState` struct + serde wire format for snapshot/restore.
- `crates/savvagent/src/plugin/builtin/html_canvas/open_in_browser.rs` — Ctrl-O temp-file + shell-out implementation.
- `docs/superpowers/notes/2026-05-23-blitz-nodeid-stability.md` — mini-spike findings for NodeId stability (Task 1 output).

**Modified files:**
- `crates/savvagent-plugin/src/error.rs` — add `PluginError::StateRestoreFailed(String)`.
- `crates/savvagent-plugin/src/content.rs` — add `ContentRenderer::snapshot_state` + `restore_state` methods with defaults.
- `crates/savvagent-plugin/src/manifest.rs` — add `KeyScope::OnFocusedCanvas` variant.
- `crates/savvagent-plugin/src/lib.rs` — re-export changes if any.
- `crates/savvagent-canvas/Cargo.toml` — add deps (`base64` for state, any new transitive Blitz crates).
- `crates/savvagent-canvas/src/lib.rs` — wire new modules.
- `crates/savvagent-canvas/src/canvas.rs` — promote all Phase-2 stub methods to real impls; thread CanvasState; integrate interceptor.
- `crates/savvagent-host/src/session.rs` — extend `ToolRegistry::call` (or its callsite) to translate `html` content items; add snapshot triggers; thread restored-state into renderer instantiation.
- `crates/savvagent/src/app.rs` — `Entry::Canvas` gains `state: Option<Vec<u8>>`; add `Entry::Unknown` via `#[serde(other)]`; `AppFocus::Canvas { id, element_idx }`; `App.canvas_focus` state.
- `crates/savvagent/src/ui.rs` — focus chrome rendering; mouse hit-testing produces canvas-relative pixel coords.
- `crates/savvagent/src/tui.rs` — mouse + keyboard routing for canvas focus; KeyScope::OnFocusedCanvas precedence.
- `crates/savvagent/src/plugin/builtin/html_canvas/mod.rs` — pass restored state to new renderer instances on `/resume`.
- `crates/savvagent/src/plugin/builtin/html_canvas/slash.rs` — `/save-canvas` already exists; no changes here.
- `CHANGELOG.md` — Phase 2 entry under `## [0.17.0] - unreleased`.
- `README.md` — interaction blurb (Ctrl-J/Ctrl-K/Tab/Ctrl-O).
- `.github/workflows/ci.yml` — no change (Phase 1's exclusion carries forward; CHANGELOG notes it).

---

## Task 1: Mini-spike — Blitz NodeId stability

**Files:**
- Create: `docs/superpowers/notes/2026-05-23-blitz-nodeid-stability.md`
- Create: throwaway crate `crates/_nodeid-spike/` (deleted at end of task)
- Modify: `Cargo.toml` (workspace) — add throwaway crate to `members` temporarily

The Phase 2 spec's interactive-state persistence assumes `HtmlCanvas` can serialize state keyed on Blitz NodeId and restore it deterministically in a new process. If NodeId is a monotonic counter that depends on parse-time ordering only, and html5ever's tree builder produces the same tree from identical input, the assumption holds. If NodeId depends on a process-global allocator, address-of-something, or random init, the assumption fails. Verify before implementing snapshot/restore against NodeId. If it fails, downgrade snapshot/restore to key on `(tag, nth-of-type-among-siblings)` paths instead.

- [ ] **Step 1: Create the throwaway crate skeleton**

```bash
mkdir -p crates/_nodeid-spike/src
```

Create `crates/_nodeid-spike/Cargo.toml`:

```toml
[package]
name = "_nodeid-spike"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
blitz-dom = { workspace = true }
blitz-html = { workspace = true }
blitz-traits = { workspace = true }
```

Add to workspace `members` in root `Cargo.toml`:

```toml
[workspace]
members = [
    # ... existing entries ...
    "crates/_nodeid-spike",
]
```

- [ ] **Step 2: Write the spike**

Create `crates/_nodeid-spike/src/main.rs`:

```rust
//! Mini-spike: verify Blitz NodeId stability across processes for
//! identical HTML input.
//!
//! Runs the parse pipeline twice in this single process AND outputs
//! enough info that the user can run two processes manually and diff.
//!
//! Findings recorded in
//! docs/superpowers/notes/2026-05-23-blitz-nodeid-stability.md.

use blitz_dom::{BaseDocument, DocumentConfig, StyleThreading};
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport};

const SAMPLE_HTML: &str = r#"<!doctype html>
<html><body>
  <h1 id="title">Hi</h1>
  <details><summary>more</summary><p>hidden</p></details>
  <form>
    <input type="text" name="a">
    <input type="text" name="b">
    <button type="submit">go</button>
  </form>
  <ul><li>1</li><li>2</li><li>3</li></ul>
</body></html>"#;

fn parse_and_dump(label: &str) -> Vec<(u32, String)> {
    let document = HtmlDocument::from_html(
        SAMPLE_HTML,
        DocumentConfig {
            base_url: None,
            net_provider: None,
            style_threading: StyleThreading::Sequential,
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            ..Default::default()
        },
    );
    let base: &BaseDocument = document.as_ref();
    let mut out = Vec::new();
    // Walk the DOM in document order, recording (node_id, node_kind/name).
    for (id, node) in base.nodes.iter().enumerate() {
        out.push((id as u32, format!("{:?}", node.data)));
    }
    println!("=== {label} ===");
    for (id, kind) in &out {
        println!("  {id:4} {kind}");
    }
    out
}

fn main() {
    let first = parse_and_dump("first parse");
    let second = parse_and_dump("second parse");

    if first == second {
        println!("\nVERDICT: NodeId is stable across two parses in the same process.");
        println!("Run this binary twice with diff to verify cross-process stability:");
        println!("  cargo run -p _nodeid-spike > /tmp/a.txt");
        println!("  cargo run -p _nodeid-spike > /tmp/b.txt");
        println!("  diff /tmp/a.txt /tmp/b.txt");
    } else {
        println!("\nVERDICT: NodeId is NOT stable even within one process.");
        println!("snapshot/restore on NodeId is not viable; key on (tag,nth-of-type) paths instead.");
    }
}
```

- [ ] **Step 3: Run the spike and the cross-process check**

```bash
cargo run -p _nodeid-spike > /tmp/spike-a.txt
cargo run -p _nodeid-spike > /tmp/spike-b.txt
diff /tmp/spike-a.txt /tmp/spike-b.txt
```

Expected: in-process verdict line says "stable"; `diff` either prints nothing (cross-process stable) or shows differences.

- [ ] **Step 4: Write findings**

Create `docs/superpowers/notes/2026-05-23-blitz-nodeid-stability.md`:

```markdown
# Blitz NodeId stability mini-spike

Date: 2026-05-23
Blitz version: =0.3.0-alpha.4 (same as Phase 1)
Question: is Blitz NodeId stable across parses of identical HTML in
different processes? Phase 2's snapshot_state/restore_state design
keys persisted state on NodeId; if NodeId is non-deterministic across
processes, the design must fall back to (tag, nth-of-type-path) keys.

## In-process stability

[Paste / summarize: did the two parses in one process produce
identical (id, kind) lists? Yes / no.]

## Cross-process stability

[Paste / summarize: did `diff /tmp/spike-a.txt /tmp/spike-b.txt`
exit with no differences? Yes / no.]

## Decision

[One of:]
- **Confirmed.** NodeId is deterministic across processes. `HtmlCanvas`
  serializes state keyed by NodeId(u32). Spec's persistence section
  stays as written.
- **Confirmed for in-process only; cross-process drifts.** snapshot/
  restore in the same session works; `/resume` after restart needs
  the (tag, nth-of-type-path) fallback. Tasks 14-16 implement
  the path-based keys.
- **Not stable at all.** Either re-design (path keys only) or
  document that interactive state persistence is best-effort and
  often resets.

## Spec amendment (if needed)

[If decision is "not as written," cite the spec lines that need updating.]
```

- [ ] **Step 5: Tear down the throwaway crate**

```bash
rm -rf crates/_nodeid-spike
```

Remove the `"crates/_nodeid-spike"` line from workspace `members`.

```bash
cargo build --workspace
```

Expected: success.

- [ ] **Step 6: If the spec needs amending, amend it**

If NodeId is not cross-process stable, edit `docs/superpowers/specs/2026-05-21-inline-html-canvas-design.md`:

- Update the "NodeId stability" bullet in the Interactive-state persistence section to reflect what actually works.
- If the fix is to key on (tag, nth-of-type-path), document the encoding shape.

If no amendment needed, note it in the spike doc.

- [ ] **Step 7: Commit**

```bash
git add docs/superpowers/notes/2026-05-23-blitz-nodeid-stability.md Cargo.toml
# If the spec was amended:
git add docs/superpowers/specs/2026-05-21-inline-html-canvas-design.md
git commit -m "docs(spike): blitz nodeid stability for state persistence"
```

---

## Task 2: `PluginError::StateRestoreFailed` variant

**Files:**
- Modify: `crates/savvagent-plugin/src/error.rs`

Add the variant the Phase 2 spec defined for `restore_state` soft-failure signaling.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` in `crates/savvagent-plugin/src/error.rs`:

```rust
    #[test]
    fn state_restore_failed_display() {
        let e = PluginError::StateRestoreFailed("schema v2 not understood".to_string());
        assert_eq!(
            format!("{e}"),
            "state restore failed: schema v2 not understood",
        );
    }
```

- [ ] **Step 2: Run; verify it fails**

```bash
cargo test -p savvagent-plugin error::tests::state_restore_failed_display
```

Expected: FAIL with `no variant or associated item named 'StateRestoreFailed' found`.

- [ ] **Step 3: Add the variant**

In `crates/savvagent-plugin/src/error.rs`, extend the enum (insert after the last existing variant):

```rust
    /// `ContentRenderer::restore_state` could not interpret the
    /// supplied bytes (corrupt, schema-incompatible, or the renderer's
    /// own decoder returned an error). The host treats this as a
    /// soft failure: log a warning, drop the bytes, continue rendering
    /// from defaults.
    StateRestoreFailed(String),
```

And add a Display branch:

```rust
            Self::StateRestoreFailed(msg) => write!(f, "state restore failed: {msg}"),
```

- [ ] **Step 4: Run; verify it passes**

```bash
cargo test -p savvagent-plugin
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-plugin/src/error.rs
git commit -m "feat(plugin): PluginError::StateRestoreFailed variant"
```

---

## Task 3: `ContentRenderer::snapshot_state` + `restore_state`

**Files:**
- Modify: `crates/savvagent-plugin/src/content.rs`

Add the two methods with no-op defaults so the Phase 1 `HtmlCanvas` impl continues to compile until Task 14 wires real bodies.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod trait_smoke` in `crates/savvagent-plugin/src/lib.rs` (the existing module from Phase 1):

```rust
    #[tokio::test]
    async fn default_snapshot_returns_none_and_restore_is_ok() {
        use crate::content::{ContentBlockId, ContentRenderer, Frame, PixelFormat, PixelSize};

        struct Stub;
        #[async_trait::async_trait]
        impl ContentRenderer for Stub {
            fn id(&self) -> ContentBlockId { ContentBlockId(0) }
            fn render(&mut self, _: PixelSize) -> Frame {
                Frame {
                    width: 1, height: 1, format: PixelFormat::Rgba8,
                    bytes: vec![0, 0, 0, 0],
                }
            }
        }

        let mut s = Stub;
        assert!(s.snapshot_state().is_none());
        assert!(s.restore_state(b"anything").is_ok());
    }
```

- [ ] **Step 2: Run; verify it fails**

```bash
cargo test -p savvagent-plugin trait_smoke::default_snapshot_returns_none_and_restore_is_ok
```

Expected: FAIL with `no method named 'snapshot_state' found`.

- [ ] **Step 3: Add the trait methods**

In `crates/savvagent-plugin/src/content.rs`, add to the `ContentRenderer` trait (after `set_focus`):

```rust
    /// Serialize the renderer's interactive state to an opaque byte
    /// blob. Returns `None` when there is nothing recoverable: the
    /// document has no stateful elements, all state is at defaults,
    /// or (for streaming renderers) the source isn't complete yet.
    ///
    /// Default returns `None`. Renderers that opt into persistence
    /// override this method.
    fn snapshot_state(&self) -> Option<Vec<u8>> {
        None
    }

    /// Restore renderer state previously produced by `snapshot_state`.
    /// Returns [`PluginError::StateRestoreFailed`] if the bytes are
    /// corrupt or schema-incompatible; the host then falls back to
    /// "no restored state" and logs a warning.
    ///
    /// Default returns `Ok(())` (no-op) so renderers that don't opt
    /// into persistence compile against the Phase 2 trait without
    /// code change.
    fn restore_state(&mut self, _bytes: &[u8]) -> Result<(), PluginError> {
        Ok(())
    }
```

- [ ] **Step 4: Run; verify it passes**

```bash
cargo test -p savvagent-plugin
```

Expected: PASS. Phase 1 `HtmlCanvas` should still compile because both new methods have defaults.

- [ ] **Step 5: Verify the full workspace builds**

```bash
cargo build --workspace
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-plugin/src/content.rs crates/savvagent-plugin/src/lib.rs
git commit -m "feat(plugin): ContentRenderer::snapshot_state + restore_state"
```

---

## Task 4: `KeyScope::OnFocusedCanvas` variant

**Files:**
- Modify: `crates/savvagent-plugin/src/manifest.rs`

The Phase 1 spec promised this scope; Phase 2 ships it. Plugins can register key bindings that fire only when `AppFocus == Canvas(id)`. Built-in canvas keys take precedence (Task 21 enforces).

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` in `crates/savvagent-plugin/src/manifest.rs`:

```rust
    #[test]
    fn key_scope_on_focused_canvas() {
        let scope = KeyScope::OnFocusedCanvas;
        // Round-trip through serde to confirm the variant is named correctly
        // for any future on-disk keybinding config.
        let s = serde_json::to_string(&scope).unwrap();
        assert_eq!(s, "\"on_focused_canvas\"");
        let back: KeyScope = serde_json::from_str(&s).unwrap();
        assert_eq!(back, scope);
    }
```

- [ ] **Step 2: Run; verify it fails**

```bash
cargo test -p savvagent-plugin manifest::tests::key_scope_on_focused_canvas
```

Expected: FAIL with `no variant or associated item named 'OnFocusedCanvas'`.

- [ ] **Step 3: Add the variant**

In `crates/savvagent-plugin/src/manifest.rs`, extend the `KeyScope` enum (insert after the last existing variant):

```rust
    /// Active iff `AppFocus == Canvas(_)`. Built-in canvas keys
    /// (Tab, Shift-Tab, Esc, Ctrl-J, Ctrl-K, Ctrl-O) take precedence;
    /// plugin bindings in this scope fire only on a built-in miss.
    OnFocusedCanvas,
```

(If the enum has `#[derive(Serialize, Deserialize)]` with `#[serde(rename_all = "snake_case")]`, no further work is needed. If it uses a different rename style, match the style of the other variants.)

- [ ] **Step 4: Run; verify it passes**

```bash
cargo test -p savvagent-plugin
```

Expected: PASS.

- [ ] **Step 5: Build the workspace to check for non-exhaustive match warnings**

```bash
cargo build --workspace 2>&1 | grep -E "non-exhaustive|missing match arm" | head -10
```

If any matches surface, follow up: extend each `match` to handle `OnFocusedCanvas`. For Phase 2 they almost certainly want a fall-through to "scope inactive when AppFocus isn't Canvas" — the actual precedence logic lands in Task 21.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-plugin/src/manifest.rs
git commit -m "feat(plugin): KeyScope::OnFocusedCanvas variant"
```

---

## Task 5: `savvagent-canvas::coords` — pixel ↔ cell helpers

**Files:**
- Create: `crates/savvagent-canvas/src/coords.rs`
- Modify: `crates/savvagent-canvas/src/lib.rs`

The TUI receives mouse events in terminal cell coordinates; the renderer needs frame-relative pixel coordinates. The renderer also owns the "I rendered at width W cells" knowledge (from the most recent `render` call). Put the translation in a small WIT-safe helper.

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent-canvas/src/coords.rs`:

```rust
//! Cell ↔ pixel coordinate translation for canvas mouse events.
//!
//! Terminal mouse events arrive in cells (row, column). The renderer
//! needs frame-relative pixel coords for synthetic event dispatch.
//! Translation depends on the (cell_width_px, cell_height_px) reported
//! by `ratatui-image::Picker` at startup and the canvas's render rect
//! (top-left cell + size in cells).
//!
//! Pure functions; no Blitz dependency. Tested in isolation.

#![warn(missing_docs)]

/// Pixel dimensions of one terminal cell, as reported by
/// `ratatui-image::Picker::from_query_stdio` at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellPixelSize {
    /// Pixel width of one cell. Common values: 8, 10, 12.
    pub width: u16,
    /// Pixel height of one cell. Common values: 16, 20.
    pub height: u16,
}

/// Cell-coordinate rect occupied by a rendered canvas on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRect {
    /// Column of the top-left cell.
    pub col: u16,
    /// Row of the top-left cell.
    pub row: u16,
    /// Width in cells.
    pub width: u16,
    /// Height in cells.
    pub height: u16,
}

/// Translate a terminal-cell mouse event to a frame-relative pixel
/// coordinate. Returns `None` if the cell is outside `rect`.
pub fn cell_to_pixel(
    rect: CellRect,
    cell_size: CellPixelSize,
    event_col: u16,
    event_row: u16,
) -> Option<(u32, u32)> {
    if event_col < rect.col
        || event_row < rect.row
        || event_col >= rect.col + rect.width
        || event_row >= rect.row + rect.height
    {
        return None;
    }
    let dx_cells = event_col - rect.col;
    let dy_cells = event_row - rect.row;
    let x_px = u32::from(dx_cells) * u32::from(cell_size.width);
    let y_px = u32::from(dy_cells) * u32::from(cell_size.height);
    Some((x_px, y_px))
}

/// Does the given cell-coord pair land inside `rect`?
pub fn contains_cell(rect: CellRect, col: u16, row: u16) -> bool {
    col >= rect.col
        && row >= rect.row
        && col < rect.col + rect.width
        && row < rect.row + rect.height
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(col: u16, row: u16, w: u16, h: u16) -> CellRect {
        CellRect { col, row, width: w, height: h }
    }

    fn cs(w: u16, h: u16) -> CellPixelSize {
        CellPixelSize { width: w, height: h }
    }

    #[test]
    fn inside_returns_pixel_offset() {
        let rect = r(10, 5, 40, 12);
        let cell = cs(8, 16);
        // Click on the cell at (col=12, row=7): 2 cells right, 2 cells down.
        assert_eq!(cell_to_pixel(rect, cell, 12, 7), Some((16, 32)));
    }

    #[test]
    fn outside_returns_none() {
        let rect = r(10, 5, 40, 12);
        let cell = cs(8, 16);
        assert!(cell_to_pixel(rect, cell, 9, 5).is_none());      // left of
        assert!(cell_to_pixel(rect, cell, 50, 5).is_none());     // right of
        assert!(cell_to_pixel(rect, cell, 10, 4).is_none());     // above
        assert!(cell_to_pixel(rect, cell, 10, 17).is_none());    // below
    }

    #[test]
    fn top_left_cell_maps_to_origin() {
        let rect = r(10, 5, 40, 12);
        let cell = cs(8, 16);
        assert_eq!(cell_to_pixel(rect, cell, 10, 5), Some((0, 0)));
    }

    #[test]
    fn contains_cell_matches_bounds() {
        let rect = r(10, 5, 40, 12);
        assert!(contains_cell(rect, 10, 5));
        assert!(contains_cell(rect, 49, 16));
        assert!(!contains_cell(rect, 9, 5));
        assert!(!contains_cell(rect, 50, 5));
    }
}
```

- [ ] **Step 2: Wire the module**

In `crates/savvagent-canvas/src/lib.rs`, add:

```rust
/// Cell ↔ pixel coordinate translation helpers.
pub mod coords;
pub use coords::{cell_to_pixel, contains_cell, CellPixelSize, CellRect};
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent-canvas coords::
```

Expected: all four tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent-canvas/src/coords.rs crates/savvagent-canvas/src/lib.rs
git commit -m "feat(canvas): coords helpers for cell↔pixel translation"
```

---

## Task 6: `savvagent-canvas::focus` — focusable element traversal

**Files:**
- Create: `crates/savvagent-canvas/src/focus.rs`
- Modify: `crates/savvagent-canvas/src/lib.rs`

Walk the Blitz DOM in document order and return every focusable element with its bounding box. Focusable = `<a href>`, `<button>`, `<input>`, `<select>`, `<textarea>`, `<details><summary>`, anything with `tabindex` ≥ 0.

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent-canvas/src/focus.rs`:

```rust
//! DOM traversal that produces the ordered list of focusable elements
//! for a Blitz `BaseDocument`. Used by `HtmlCanvas::focusable_elements`.

#![warn(missing_docs)]

use blitz_dom::BaseDocument;
use savvagent_plugin::{FocusableElement, Rect};

/// Walk `base` in document order, returning every focusable element's
/// `(node_id, FocusableElement)` pair. Caller stores the `node_id` for
/// later `set_focus` dispatch (to look up the Blitz node by id).
pub fn collect(base: &BaseDocument) -> Vec<(u32, FocusableElement)> {
    let mut out = Vec::new();
    // Document order = depth-first preorder traversal from the root.
    // Blitz's `BaseDocument` exposes nodes in a flat Vec; traversal
    // via `node.children` is correct.
    walk(base, base.root_element().id, &mut out);
    out
}

fn walk(
    base: &BaseDocument,
    node_id: u32,
    out: &mut Vec<(u32, FocusableElement)>,
) {
    let node = match base.try_node_by_id(node_id) {
        Some(n) => n,
        None => return,
    };
    if is_focusable(node) {
        let rect = bounding_rect(node);
        out.push((
            node_id,
            FocusableElement {
                id: format!("{node_id}"),
                bounds: rect,
            },
        ));
    }
    for child in node.children.iter().copied() {
        walk(base, child, out);
    }
}

fn is_focusable(node: &blitz_dom::Node) -> bool {
    // Element-only check.
    let element = match node.data.element_data() {
        Some(e) => e,
        None => return false,
    };
    let local = element.name.local.as_ref();
    match local {
        "a" => element.attr(blitz_dom::local_name!("href")).is_some(),
        "button" | "input" | "select" | "textarea" => true,
        "summary" => true, // any <summary> (Phase 2 scope; <details>/<summary> pairing)
        _ => {
            // [tabindex] attribute opt-in.
            element
                .attr(blitz_dom::local_name!("tabindex"))
                .and_then(|v| v.parse::<i32>().ok())
                .map(|n| n >= 0)
                .unwrap_or(false)
        }
    }
}

fn bounding_rect(node: &blitz_dom::Node) -> Rect {
    // `node.final_layout` is the resolved layout; `location` and `size`
    // are f32 pixels. Round; clamp to u32 (canvases are small).
    let l = &node.final_layout;
    let x = l.location.x.round().max(0.0) as u32;
    let y = l.location.y.round().max(0.0) as u32;
    let width = l.size.width.round().max(0.0) as u32;
    let height = l.size.height.round().max(0.0) as u32;
    Rect { x, y, width, height }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blitz_dom::{DocumentConfig, StyleThreading};
    use blitz_html::HtmlDocument;
    use blitz_traits::shell::{ColorScheme, Viewport};

    const SAMPLE: &str = r#"<!doctype html>
<html><body>
  <p>not focusable</p>
  <a href="https://example.com">link 1</a>
  <a>link without href — not focusable</a>
  <button>button</button>
  <details><summary>summary</summary><p>body</p></details>
  <input type="text">
  <div tabindex="0">tabbable div</div>
  <div tabindex="-1">explicitly skipped div</div>
</body></html>"#;

    fn parse() -> HtmlDocument {
        HtmlDocument::from_html(
            SAMPLE,
            DocumentConfig {
                base_url: None,
                net_provider: None,
                style_threading: StyleThreading::Sequential,
                viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
                ..Default::default()
            },
        )
    }

    #[test]
    fn focusable_elements_in_document_order() {
        let mut doc = parse();
        {
            let base: &mut BaseDocument = doc.as_mut();
            base.resolve(0.0);
        }
        let base: &BaseDocument = doc.as_ref();
        let elements = collect(base);
        // Expected (in order): link 1 (a with href), button, summary,
        // input, tabbable div. NOT: anchor-without-href, tabindex=-1 div.
        assert_eq!(elements.len(), 5, "got: {elements:#?}");
    }
}
```

(If `local_name!` macro is not the exact form Blitz uses, swap for the actual call shape — the implementer's job during Task 6 execution to adapt to the pinned alpha.4 API discovered during the spike.)

- [ ] **Step 2: Wire the module**

In `crates/savvagent-canvas/src/lib.rs`, add:

```rust
mod focus;
```

(Not `pub`; the canvas internals use it.)

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent-canvas focus::
```

Expected: `focusable_elements_in_document_order` passes. If the test fails because Blitz's actual API differs from the assumed `attr`, `final_layout`, etc., adjust to match the real API — the test is the contract; the impl should match.

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent-canvas/src/focus.rs crates/savvagent-canvas/src/lib.rs
git commit -m "feat(canvas): focus.rs traversal for focusable elements"
```

---

## Task 7: Implement `HtmlCanvas::{focusable_elements, focused_index, set_focus}`

**Files:**
- Modify: `crates/savvagent-canvas/src/canvas.rs`

Wire the focus.rs traversal output into the `ContentRenderer` trait methods on `HtmlCanvas`. Store the focused node id on the canvas; expose the index into `focusable_elements()` as `focused_index()`.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` in `crates/savvagent-canvas/src/canvas.rs`:

```rust
    #[test]
    fn focusable_elements_returns_walk_results() {
        let mut c = HtmlCanvas::new(
            ContentBlockId(1),
            "<!doctype html><body><a href='x'>link</a><button>b</button></body>",
        );
        // Need a render pass to populate layout before traversal.
        c.render(PixelSize { width: 200, height: 0 });
        let elements = c.focusable_elements();
        assert_eq!(elements.len(), 2, "got {elements:#?}");
        // First should be the link (document order).
        assert!(elements[0].id != elements[1].id);
    }

    #[test]
    fn set_focus_updates_focused_index() {
        let mut c = HtmlCanvas::new(
            ContentBlockId(2),
            "<!doctype html><body><a href='x'>l1</a><a href='y'>l2</a></body>",
        );
        c.render(PixelSize { width: 200, height: 0 });
        assert_eq!(c.focused_index(), None);
        c.set_focus(Some(1));
        assert_eq!(c.focused_index(), Some(1));
        c.set_focus(None);
        assert_eq!(c.focused_index(), None);
    }
```

- [ ] **Step 2: Run; verify they fail**

```bash
cargo test -p savvagent-canvas canvas::tests
```

Expected: FAIL (today's `focused_index` default returns `None` always; `set_focus` does nothing).

- [ ] **Step 3: Add state fields**

In `crates/savvagent-canvas/src/canvas.rs`, extend `HtmlCanvas`:

```rust
pub struct HtmlCanvas {
    id: ContentBlockId,
    source: String,
    // NEW Phase 2 state:
    /// Cached focusable-element list from the most recent render. `None`
    /// before the first render. Tuple is (node_id, exposed element).
    focusable_cache: Option<Vec<(u32, FocusableElement)>>,
    /// Index into `focusable_cache` that's currently focused, if any.
    focused: Option<u32>,
    /// Document state set by `freeze`; events are dropped while frozen.
    frozen: bool,
    /// Owned Blitz document. Phase 1 reparsed on every render; Phase 2
    /// keeps it across renders so DOM state (form values, scroll,
    /// expanded details) survives between calls.
    document: Option<blitz_html::HtmlDocument>,
}
```

Adjust `HtmlCanvas::new` to initialize:

```rust
pub fn new(id: ContentBlockId, source: &str) -> Self {
    crate::subset::validate(source);
    Self {
        id,
        source: source.to_string(),
        focusable_cache: None,
        focused: None,
        frozen: false,
        document: None,
    }
}
```

(Make sure to add the `use` for `FocusableElement` at the top: `use savvagent_plugin::{..., FocusableElement, ...};`.)

- [ ] **Step 4: Implement the methods**

Replace the trait method bodies on `HtmlCanvas`:

```rust
fn focusable_elements(&self) -> Vec<FocusableElement> {
    self.focusable_cache
        .as_ref()
        .map(|v| v.iter().map(|(_, fe)| fe.clone()).collect())
        .unwrap_or_default()
}

fn focused_index(&self) -> Option<u32> {
    self.focused
}

fn set_focus(&mut self, index: Option<u32>) {
    if let Some(i) = index {
        let len = self
            .focusable_cache
            .as_ref()
            .map(|v| v.len() as u32)
            .unwrap_or(0);
        if i >= len {
            self.focused = None;
            return;
        }
    }
    self.focused = index;
}
```

- [ ] **Step 5: Populate the cache during render**

In `render_html_to_rgba` (or in `HtmlCanvas::render` directly — refactor as needed), after the final layout resolve and before returning the frame, call:

```rust
self.focusable_cache = Some(crate::focus::collect(document.as_ref()));
```

This requires `render` to own the document (Step 3 added the `document` field). Refactor `render_html_to_rgba` to take `&mut HtmlCanvas` or fold it directly into `render`.

- [ ] **Step 6: Run; verify the tests pass**

```bash
cargo test -p savvagent-canvas canvas::tests
```

Expected: both `focusable_elements_returns_walk_results` and `set_focus_updates_focused_index` pass.

- [ ] **Step 7: Run the existing canvas tests too — they must still pass**

```bash
cargo test -p savvagent-canvas
```

Expected: `canvas_renders_at_requested_width` and `canvas_id_round_trips` still pass.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent-canvas/src/canvas.rs
git commit -m "feat(canvas): wire focusable_elements + focused_index + set_focus"
```

---

## Task 8: Implement `HtmlCanvas::{freeze, thaw}`

**Files:**
- Modify: `crates/savvagent-canvas/src/canvas.rs`

Frozen canvases drop input events but otherwise behave normally. Thaw re-enables dispatch. No re-layout, no re-paint — soft freeze.

- [ ] **Step 1: Write the failing test**

Append to `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn freeze_and_thaw_flip_internal_flag() {
        let mut c = HtmlCanvas::new(
            ContentBlockId(3),
            "<!doctype html><body><a href='x'>l</a></body>",
        );
        c.render(PixelSize { width: 100, height: 0 });
        c.freeze();
        // dispatch tests come in Task 9; here we just verify the flag.
        // Expose a test-only getter via #[cfg(test)] to assert.
        assert!(c.is_frozen());
        c.thaw();
        assert!(!c.is_frozen());
    }
```

Add the test-only getter to the impl:

```rust
#[cfg(test)]
impl HtmlCanvas {
    fn is_frozen(&self) -> bool {
        self.frozen
    }
}
```

- [ ] **Step 2: Run; verify it fails**

```bash
cargo test -p savvagent-canvas canvas::tests::freeze_and_thaw_flip_internal_flag
```

Expected: FAIL (the existing default impls do nothing).

- [ ] **Step 3: Implement**

Replace the no-op default `freeze` and `thaw` on `HtmlCanvas`:

```rust
fn freeze(&mut self) {
    self.frozen = true;
}

fn thaw(&mut self) {
    self.frozen = false;
}
```

- [ ] **Step 4: Run; verify it passes**

```bash
cargo test -p savvagent-canvas
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-canvas/src/canvas.rs
git commit -m "feat(canvas): freeze/thaw soft-freeze flag"
```

---

## Task 9: `savvagent-canvas::events` — synthetic event dispatch core

**Files:**
- Create: `crates/savvagent-canvas/src/events.rs`
- Modify: `crates/savvagent-canvas/src/canvas.rs`
- Modify: `crates/savvagent-canvas/src/lib.rs`

Phase 0 spike confirmed Blitz alpha.4 accepts synthetic events via `BaseDocument::handle_dom_event` / `Document::handle_ui_event`. This task wraps them. The wrapper takes an `InputEvent` from `savvagent-plugin`, converts to the Blitz event shape, dispatches, and returns whether the DOM is dirty (event landed on a node that changed state). Default-action interception lands in Tasks 10-12.

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent-canvas/src/events.rs`:

```rust
//! Synthetic event dispatch into Blitz. Translates portable
//! `InputEvent` values into Blitz's UI/DOM event shape and routes
//! them through `BaseDocument::handle_dom_event` /
//! `Document::handle_ui_event`.
//!
//! Default actions (link follow, <details> toggle, form submit) are
//! NOT done here — they live in `interceptor::run_default_actions`
//! and are called from `HtmlCanvas::dispatch` AFTER this raw dispatch.

#![warn(missing_docs)]

use blitz_dom::BaseDocument;
use savvagent_plugin::{InputEvent, MouseEventKind, MouseEventPortable};

/// Outcome of raw dispatch: which node the event landed on (if any) and
/// whether the DOM was marked dirty by Blitz.
#[derive(Debug, Clone)]
pub struct RawDispatch {
    /// Node id that received the event, if any. `None` if the event
    /// fell outside any element.
    pub target_node: Option<u32>,
    /// Whether Blitz reports the DOM changed.
    pub dirty: bool,
}

/// Dispatch `event` against `base`. The caller is responsible for
/// calling `base.resolve(0.0)` afterwards if `dirty` is true.
pub fn dispatch_raw(base: &mut BaseDocument, event: &InputEvent) -> RawDispatch {
    match event {
        InputEvent::Mouse(m) => dispatch_mouse(base, m),
        InputEvent::Key(_) => RawDispatch { target_node: None, dirty: false }, // Phase 2 task: see Task 18
        InputEvent::Focus(_) => RawDispatch { target_node: None, dirty: false },
    }
}

fn dispatch_mouse(base: &mut BaseDocument, m: &MouseEventPortable) -> RawDispatch {
    // Hit-test the document at the event's pixel position.
    let target = base.hit_test(m.x_pixel as f32, m.y_pixel as f32);
    match m.kind {
        MouseEventKind::Press => {
            // Construct a Blitz UI event and dispatch.
            // (The exact `UiEvent::*` enum the alpha.4 API exposes is
            // discovered in the Phase 0 spike notes. Use whichever
            // constructor maps to "mouse press at point.")
            let _ = base; let _ = target;
            todo!("call Blitz handle_ui_event with PointerDown");
        }
        MouseEventKind::Release => {
            let _ = base; let _ = target;
            todo!("call Blitz handle_ui_event with PointerUp");
        }
        MouseEventKind::Move => {
            let _ = base; let _ = target;
            todo!("call Blitz handle_ui_event with PointerMove");
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            // No scroll handling in Phase 2.0; overflow containers don't
            // exist in the subset that ships. Track in plan addendum
            // if user feedback wants scroll.
            RawDispatch { target_node: target, dirty: false }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blitz_dom::{DocumentConfig, StyleThreading};
    use blitz_html::HtmlDocument;
    use blitz_traits::shell::{ColorScheme, Viewport};
    use savvagent_plugin::{KeyMods, MouseButton, MouseEventKind, MouseEventPortable};

    fn doc() -> HtmlDocument {
        HtmlDocument::from_html(
            "<!doctype html><body><a id='target' href='https://example.com'>link</a></body>",
            DocumentConfig {
                base_url: None,
                net_provider: None,
                style_threading: StyleThreading::Sequential,
                viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
                ..Default::default()
            },
        )
    }

    #[test]
    fn mouse_press_on_link_targets_link_node() {
        let mut d = doc();
        {
            let base: &mut BaseDocument = d.as_mut();
            base.resolve(0.0);
        }
        let base: &mut BaseDocument = d.as_mut();
        // The link is the only inline content near the top-left of body.
        // Click somewhere in the body padding/margin first character area.
        let ev = InputEvent::Mouse(MouseEventPortable {
            kind: MouseEventKind::Press,
            button: Some(MouseButton::Left),
            x_pixel: 16,
            y_pixel: 24,
            modifiers: KeyMods::default(),
        });
        let out = dispatch_raw(base, &ev);
        // The hit-test should land on *some* element; if it lands on
        // the <a>, even better. We assert it's at least non-None.
        assert!(out.target_node.is_some(), "expected hit-test to find a node");
    }
}
```

(The `todo!()` blocks in `dispatch_mouse` are filled in during Step 3 once the exact Blitz API is in hand. Step 2 confirms the test compiles but the dispatch path panics on the `todo!`.)

- [ ] **Step 2: Wire the module + verify the test compiles**

In `crates/savvagent-canvas/src/lib.rs`, add:

```rust
mod events;
```

Then:

```bash
cargo build -p savvagent-canvas
```

Expected: builds. (Tests don't run yet because of the `todo!`.)

- [ ] **Step 3: Fill in the `todo!()` blocks against Blitz alpha.4**

Open the Phase 0 spike notes (`docs/superpowers/notes/2026-05-21-blitz-spike.md`) for the actual UI-event API surface. The spike confirmed `BaseDocument::handle_dom_event` and `Document::handle_ui_event` accept synthetic events. Adapt the three `todo!` arms to construct the appropriate Blitz event type and call. Each constructed event should:

1. Carry the pixel coordinates from `MouseEventPortable`.
2. Carry the button mapping (Left → primary, Right → secondary, Middle → auxiliary).
3. Carry modifier keys (`KeyMods` from `savvagent_plugin::types`).

After dispatching, check Blitz's "DOM dirty" signal (alpha.4 may expose this on the returned event handle or via a `base.is_dirty()` getter — check the spike notes / source). Set `dirty: bool` accordingly.

- [ ] **Step 4: Run the test**

```bash
cargo test -p savvagent-canvas events::tests::mouse_press_on_link_targets_link_node
```

Expected: PASS. If the hit-test returns `None`, the click coordinates probably miss the link's actual bounding box — adjust the coordinates in the test (or use `base.try_node_by_id(...)` to look up the link first and use its `final_layout.location` as the click point).

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-canvas/src/events.rs crates/savvagent-canvas/src/lib.rs
git commit -m "feat(canvas): synthetic event dispatch into Blitz"
```

---

## Task 10: `interceptor.rs` — default-action interceptor for links

**Files:**
- Create: `crates/savvagent-canvas/src/interceptor.rs`
- Modify: `crates/savvagent-canvas/src/lib.rs`

The Phase 0 spike documented that Blitz alpha.4 does NOT run browser default actions for synthetic events. Phase 2 ships a renderer-side interceptor inside `dispatch` that maps clicks on `<a href>` to `Effect::OpenUrl` with the URL classified per the spec's URL-scheme table. `<details>` toggle and form submit land in Tasks 11 + 12.

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent-canvas/src/interceptor.rs`:

```rust
//! Renderer-side default-action interceptor. Runs AFTER raw event
//! dispatch (`events::dispatch_raw`). Inspects the targeted DOM node;
//! if it matches a default-action element (link, summary, form button),
//! produces an `Effect` for the host to apply.
//!
//! Why renderer-side: keeps Blitz's headless eventing self-contained.
//! Effects flow up via `InputOutcome::effects` so the host still
//! mediates the actual shell-out.

#![warn(missing_docs)]

use blitz_dom::BaseDocument;
use savvagent_plugin::{Effect, UrlTarget};

/// Examine the node at `target_node`; if it triggers a default
/// action, return the `Effect` to apply. Returns `None` for
/// non-default-action targets.
pub fn intercept(base: &BaseDocument, target_node: Option<u32>) -> Option<Effect> {
    let id = target_node?;
    let node = base.try_node_by_id(id)?;
    let element = node.data.element_data()?;
    match element.name.local.as_ref() {
        "a" => link_effect(element),
        _ => None, // <summary>, form submit handled in Tasks 11 + 12
    }
}

fn link_effect(element: &blitz_dom::ElementData) -> Option<Effect> {
    let href = element.attr(blitz_dom::local_name!("href"))?.to_string();
    let target = classify_url(&href)?;
    Some(Effect::OpenUrl { url: href, target })
}

/// Classify an href per the Phase 2 spec's URL-scheme table.
/// Returns `None` for hrefs that should NOT produce an effect
/// (data:, file://, javascript:, unknown schemes).
pub fn classify_url(href: &str) -> Option<UrlTarget> {
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        Some(UrlTarget::SystemBrowser)
    } else if lower.starts_with("mailto:")
        || lower.starts_with("tel:")
        || lower.starts_with("sms:")
    {
        Some(UrlTarget::SystemBrowser)
    } else if lower.starts_with("data:") {
        tracing::debug!(href, "interceptor: data: URL ignored");
        None
    } else if lower.starts_with("javascript:") {
        tracing::warn!(href, "interceptor: javascript: URL blocked");
        None
    } else if lower.starts_with("file://") {
        tracing::debug!(href, "interceptor: file:// URL ignored (subset violation)");
        None
    } else if href.contains("://") {
        // Unknown scheme.
        tracing::warn!(href, "interceptor: unknown URL scheme; no effect emitted");
        None
    } else {
        // No scheme → relative path / bare filename → ContinueConversation.
        Some(UrlTarget::ContinueConversation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_routes_to_system_browser() {
        assert_eq!(classify_url("https://example.com"), Some(UrlTarget::SystemBrowser));
        assert_eq!(classify_url("HTTP://Example.com/path"), Some(UrlTarget::SystemBrowser));
    }

    #[test]
    fn mailto_routes_to_system_browser() {
        assert_eq!(classify_url("mailto:user@example.com"), Some(UrlTarget::SystemBrowser));
    }

    #[test]
    fn tel_and_sms_route_to_system_browser() {
        assert_eq!(classify_url("tel:+15551234567"), Some(UrlTarget::SystemBrowser));
        assert_eq!(classify_url("sms:+15551234567"), Some(UrlTarget::SystemBrowser));
    }

    #[test]
    fn data_url_emits_no_effect() {
        assert_eq!(classify_url("data:text/plain,hello"), None);
    }

    #[test]
    fn javascript_url_is_blocked() {
        assert_eq!(classify_url("javascript:alert(1)"), None);
        assert_eq!(classify_url("JAVASCRIPT:alert(1)"), None);
    }

    #[test]
    fn file_url_emits_no_effect() {
        assert_eq!(classify_url("file:///etc/passwd"), None);
    }

    #[test]
    fn unknown_scheme_emits_no_effect() {
        assert_eq!(classify_url("steam://run/440"), None);
    }

    #[test]
    fn bare_path_continues_conversation() {
        assert_eq!(classify_url("./foo.md"), Some(UrlTarget::ContinueConversation));
        assert_eq!(classify_url("docs/spec.md"), Some(UrlTarget::ContinueConversation));
        assert_eq!(classify_url("foo.rs"), Some(UrlTarget::ContinueConversation));
    }
}
```

- [ ] **Step 2: Wire the module**

In `crates/savvagent-canvas/src/lib.rs`, add:

```rust
mod interceptor;
```

- [ ] **Step 3: Run the unit tests**

```bash
cargo test -p savvagent-canvas interceptor::
```

Expected: all 8 classification tests pass.

- [ ] **Step 4: Now write the integration test that proves the interceptor fires on a real click**

Append to `events.rs`'s test module (the file already has Blitz fixture setup):

```rust
    #[test]
    fn link_click_produces_open_url_effect() {
        let mut d = HtmlDocument::from_html(
            "<!doctype html><body><a id='lnk' href='https://example.com'>x</a></body>",
            DocumentConfig {
                base_url: None,
                net_provider: None,
                style_threading: StyleThreading::Sequential,
                viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
                ..Default::default()
            },
        );
        {
            let base: &mut BaseDocument = d.as_mut();
            base.resolve(0.0);
        }
        let base = d.as_ref();
        // Find the <a>'s node id (test setup; in production the dispatch
        // path provides it).
        // Use a recursive walk OR a known coordinate. Use a walk for
        // reliability:
        let lnk_id = find_node_by_tag(base, "a").expect("a element present");
        let effect = crate::interceptor::intercept(base, Some(lnk_id));
        match effect {
            Some(savvagent_plugin::Effect::OpenUrl { url, target }) => {
                assert_eq!(url, "https://example.com");
                assert_eq!(target, savvagent_plugin::UrlTarget::SystemBrowser);
            }
            other => panic!("expected OpenUrl, got {other:?}"),
        }
    }

    fn find_node_by_tag(base: &BaseDocument, tag: &str) -> Option<u32> {
        fn walk(base: &BaseDocument, id: u32, tag: &str) -> Option<u32> {
            let node = base.try_node_by_id(id)?;
            if let Some(e) = node.data.element_data() {
                if e.name.local.as_ref() == tag { return Some(id); }
            }
            for c in node.children.iter().copied() {
                if let Some(found) = walk(base, c, tag) {
                    return Some(found);
                }
            }
            None
        }
        walk(base, base.root_element().id, tag)
    }
```

- [ ] **Step 5: Run the new integration test**

```bash
cargo test -p savvagent-canvas events::tests::link_click_produces_open_url_effect
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-canvas/src/interceptor.rs crates/savvagent-canvas/src/lib.rs crates/savvagent-canvas/src/events.rs
git commit -m "feat(canvas): interceptor for link default action + URL classification"
```

---

## Task 11: Interceptor — `<details>` toggle

**Files:**
- Modify: `crates/savvagent-canvas/src/interceptor.rs`
- Modify: `crates/savvagent-canvas/src/canvas.rs` (re-resolve after toggle)

A click on a `<summary>` toggles the parent `<details>`'s `open` attribute. The interceptor flips it and signals to the caller that a re-resolve is needed (so the next render reflects the new layout).

- [ ] **Step 1: Write the failing test**

Append to `interceptor.rs`'s test module:

```rust
    use blitz_dom::BaseDocument;

    #[test]
    fn summary_click_returns_redraw_signal() {
        let html = "<!doctype html><body><details><summary>s</summary><p>body</p></details></body>";
        let mut doc = blitz_html::HtmlDocument::from_html(
            html,
            blitz_dom::DocumentConfig {
                base_url: None,
                net_provider: None,
                style_threading: blitz_dom::StyleThreading::Sequential,
                viewport: Some(blitz_traits::shell::Viewport::new(
                    800, 600, 1.0,
                    blitz_traits::shell::ColorScheme::Light,
                )),
                ..Default::default()
            },
        );
        {
            let base: &mut BaseDocument = doc.as_mut();
            base.resolve(0.0);
        }
        let summary_id = {
            let base: &BaseDocument = doc.as_ref();
            find_node_by_tag(base, "summary").expect("summary present")
        };
        let details_id_before = {
            let base: &BaseDocument = doc.as_ref();
            // The details parent should NOT have an open attribute yet.
            let summary = base.try_node_by_id(summary_id).unwrap();
            let parent = summary.parent.expect("summary has parent");
            let details = base.try_node_by_id(parent).unwrap();
            let element = details.data.element_data().unwrap();
            assert!(
                element.attr(blitz_dom::local_name!("open")).is_none(),
                "details should start closed"
            );
            parent
        };
        let base: &mut BaseDocument = doc.as_mut();
        let outcome = crate::interceptor::intercept_mut(base, Some(summary_id));
        // intercept_mut returns whether the DOM was mutated, plus optionally an Effect.
        assert!(outcome.dirty, "summary click should mutate DOM");
        assert!(outcome.effect.is_none(), "no Effect for summary click — internal-only");
        // Verify the parent details now has open.
        let details = base.try_node_by_id(details_id_before).unwrap();
        let element = details.data.element_data().unwrap();
        assert!(
            element.attr(blitz_dom::local_name!("open")).is_some(),
            "details should now be open"
        );
    }

    fn find_node_by_tag(base: &BaseDocument, tag: &str) -> Option<u32> {
        fn walk(base: &BaseDocument, id: u32, tag: &str) -> Option<u32> {
            let node = base.try_node_by_id(id)?;
            if let Some(e) = node.data.element_data() {
                if e.name.local.as_ref() == tag { return Some(id); }
            }
            for c in node.children.iter().copied() {
                if let Some(found) = walk(base, c, tag) {
                    return Some(found);
                }
            }
            None
        }
        walk(base, base.root_element().id, tag)
    }
```

(The earlier test in this file probably also defined `find_node_by_tag`; if so, dedupe — put it once at the top of the test module.)

- [ ] **Step 2: Run; verify it fails**

```bash
cargo test -p savvagent-canvas interceptor::tests::summary_click_returns_redraw_signal
```

Expected: FAIL — `intercept_mut` doesn't exist yet.

- [ ] **Step 3: Add `intercept_mut`**

In `interceptor.rs`:

```rust
/// Result of a mutating interception pass.
#[derive(Debug)]
pub struct InterceptOutcome {
    /// Effect to surface to the host (e.g. `OpenUrl` for links), or
    /// `None` if the interception was purely internal (toggling
    /// `<details>` open attr).
    pub effect: Option<Effect>,
    /// True if the DOM was mutated and the caller must re-resolve
    /// before painting.
    pub dirty: bool,
}

/// Like [`intercept`] but allows mutating the DOM (for `<details>`
/// toggle and similar). Returns both the optional `Effect` and the
/// `dirty` flag.
pub fn intercept_mut(
    base: &mut BaseDocument,
    target_node: Option<u32>,
) -> InterceptOutcome {
    let id = match target_node {
        Some(id) => id,
        None => return InterceptOutcome { effect: None, dirty: false },
    };
    // Read tag first; if it's <a>, delegate to the immutable path.
    let tag = base
        .try_node_by_id(id)
        .and_then(|n| n.data.element_data())
        .map(|e| e.name.local.as_ref().to_string());
    match tag.as_deref() {
        Some("a") => InterceptOutcome {
            effect: intercept(base, Some(id)),
            dirty: false,
        },
        Some("summary") => toggle_details_parent(base, id),
        _ => InterceptOutcome { effect: None, dirty: false },
    }
}

fn toggle_details_parent(base: &mut BaseDocument, summary_id: u32) -> InterceptOutcome {
    let parent_id = match base.try_node_by_id(summary_id).and_then(|n| n.parent) {
        Some(p) => p,
        None => return InterceptOutcome { effect: None, dirty: false },
    };
    // Parent should be a <details>; if it isn't, no-op.
    let is_details = base
        .try_node_by_id(parent_id)
        .and_then(|n| n.data.element_data())
        .map(|e| e.name.local.as_ref() == "details")
        .unwrap_or(false);
    if !is_details {
        return InterceptOutcome { effect: None, dirty: false };
    }
    // Toggle the `open` attribute.
    let currently_open = base
        .try_node_by_id(parent_id)
        .and_then(|n| n.data.element_data())
        .map(|e| e.attr(blitz_dom::local_name!("open")).is_some())
        .unwrap_or(false);
    // The exact Blitz API for mutating attributes on a node is alpha.4-specific.
    // Common shape: `base.set_attribute(parent_id, name, value)`.
    if currently_open {
        base.remove_attribute(parent_id, blitz_dom::local_name!("open"));
    } else {
        base.set_attribute(parent_id, blitz_dom::local_name!("open"), "");
    }
    InterceptOutcome { effect: None, dirty: true }
}
```

(If `base.set_attribute` / `remove_attribute` aren't the exact alpha.4 method names, adapt — the spike notes have the real API.)

- [ ] **Step 4: Run; verify it passes**

```bash
cargo test -p savvagent-canvas interceptor::
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-canvas/src/interceptor.rs
git commit -m "feat(canvas): interceptor for <details> toggle"
```

---

## Task 12: Interceptor — form submit (button)

**Files:**
- Modify: `crates/savvagent-canvas/src/interceptor.rs`

A click on a `<button type="submit">` (or `<input type="submit">`) inside a form synthesizes an `Effect::OpenUrl` with the form's action attribute serialized in. For Phase 2 we keep it simple: scan up to the nearest `<form>` ancestor, take its `action` attribute (or current URL if absent), URL-encode form field name/value pairs, emit `OpenUrl { url, target: <classify_url> }`. The classification table from Task 10 applies — most form actions will be relative paths → `ContinueConversation`.

- [ ] **Step 1: Write the failing test**

Append to `interceptor.rs`'s test module:

```rust
    #[test]
    fn submit_button_inside_form_emits_open_url() {
        let html = r#"<!doctype html><body>
          <form action="./review.md" method="get">
            <input type="text" name="title" value="hello">
            <button type="submit">go</button>
          </form>
        </body>"#;
        let mut doc = blitz_html::HtmlDocument::from_html(
            html,
            blitz_dom::DocumentConfig {
                base_url: None, net_provider: None,
                style_threading: blitz_dom::StyleThreading::Sequential,
                viewport: Some(blitz_traits::shell::Viewport::new(
                    800, 600, 1.0,
                    blitz_traits::shell::ColorScheme::Light,
                )),
                ..Default::default()
            },
        );
        {
            let base: &mut BaseDocument = doc.as_mut();
            base.resolve(0.0);
        }
        let btn_id = {
            let base: &BaseDocument = doc.as_ref();
            find_node_by_tag(base, "button").expect("button present")
        };
        let base: &mut BaseDocument = doc.as_mut();
        let outcome = crate::interceptor::intercept_mut(base, Some(btn_id));
        match outcome.effect {
            Some(savvagent_plugin::Effect::OpenUrl { url, target }) => {
                assert!(url.starts_with("./review.md"), "url was {url:?}");
                assert!(url.contains("title=hello"), "expected query string");
                assert_eq!(target, savvagent_plugin::UrlTarget::ContinueConversation);
            }
            other => panic!("expected OpenUrl, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run; verify it fails**

```bash
cargo test -p savvagent-canvas interceptor::tests::submit_button_inside_form_emits_open_url
```

Expected: FAIL.

- [ ] **Step 3: Extend `intercept_mut`**

In `interceptor.rs`, add `"button" | "input"` arms to the match in `intercept_mut`. New helper:

```rust
fn maybe_submit_effect(base: &BaseDocument, node_id: u32) -> Option<Effect> {
    let node = base.try_node_by_id(node_id)?;
    let element = node.data.element_data()?;
    // Must be a submit-typed button or input.
    let is_submit = match element.name.local.as_ref() {
        "button" => element
            .attr(blitz_dom::local_name!("type"))
            .map(|t| t == "submit")
            .unwrap_or(true), // default button type is "submit"
        "input" => element
            .attr(blitz_dom::local_name!("type"))
            .map(|t| t == "submit")
            .unwrap_or(false),
        _ => false,
    };
    if !is_submit {
        return None;
    }
    // Walk up to find the nearest <form>.
    let form_id = find_ancestor_form(base, node_id)?;
    let form = base.try_node_by_id(form_id)?;
    let form_element = form.data.element_data()?;
    let action = form_element
        .attr(blitz_dom::local_name!("action"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| ".".to_string());
    // Collect form fields: inputs, selects, textareas with a name attr.
    let pairs = collect_form_values(base, form_id);
    let query = encode_query(&pairs);
    let separator = if action.contains('?') { '&' } else { '?' };
    let url = if pairs.is_empty() {
        action
    } else {
        format!("{action}{separator}{query}")
    };
    let target = classify_url(&url)?;
    Some(Effect::OpenUrl { url, target })
}

fn find_ancestor_form(base: &BaseDocument, node_id: u32) -> Option<u32> {
    let mut current = node_id;
    while let Some(node) = base.try_node_by_id(current) {
        if node.data.element_data()
            .map(|e| e.name.local.as_ref() == "form")
            .unwrap_or(false)
        {
            return Some(current);
        }
        current = node.parent?;
    }
    None
}

fn collect_form_values(base: &BaseDocument, form_id: u32) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk_inputs(base, form_id, &mut out);
    out
}

fn walk_inputs(base: &BaseDocument, node_id: u32, out: &mut Vec<(String, String)>) {
    let node = match base.try_node_by_id(node_id) {
        Some(n) => n,
        None => return,
    };
    if let Some(element) = node.data.element_data() {
        let tag = element.name.local.as_ref();
        if matches!(tag, "input" | "select" | "textarea") {
            if let Some(name) = element.attr(blitz_dom::local_name!("name")) {
                // For <input>, value is the value attribute.
                // For <select>/<textarea>, alpha.4's DOM probably tracks
                // the live value separately — adapt as needed during impl.
                let value = element
                    .attr(blitz_dom::local_name!("value"))
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                out.push((name.to_string(), value));
            }
        }
    }
    for child in node.children.iter().copied() {
        walk_inputs(base, child, out);
    }
}

fn encode_query(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(s: &str) -> String {
    // Minimal: escape spaces and a few well-known chars. Form submit
    // through the host is not security-critical here (no actual HTTP
    // request); the model receives the URL as a prompt and handles it.
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            other => format!("%{:02X}", other as u32),
        })
        .collect()
}
```

And add `"button" | "input"` to the `intercept_mut` match:

```rust
        Some("button") | Some("input") => InterceptOutcome {
            effect: maybe_submit_effect(base, id),
            dirty: false,
        },
```

- [ ] **Step 4: Run; verify it passes**

```bash
cargo test -p savvagent-canvas interceptor::
```

Expected: PASS. The action `./review.md?title=hello` is a relative path with a query string, so `classify_url` returns `ContinueConversation`.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-canvas/src/interceptor.rs
git commit -m "feat(canvas): interceptor for form submit"
```

---

> **PLAN AMENDMENT (2026-05-25, during execution):** Task 7 discovered
> that Blitz's `HtmlDocument` is `!Send` while `ContentRenderer: Send`
> and the crate is `#![forbid(unsafe_code)]`, so `HtmlCanvas` **cannot
> retain the document** between calls. Tasks 13/15/16 are therefore
> re-sequenced and redesigned around a **re-parse + replay** model:
>
> - **Execution order: do Task 14 (`CanvasState`) BEFORE Task 13.**
> - `HtmlCanvas` holds a `canvas_state: CanvasState` field (the
>   `Send` interactive-state log). It does NOT hold a document.
> - A shared helper `apply_state(base, &CanvasState)` replays the log
>   onto a freshly-parsed document; `collect_state(base) -> CanvasState`
>   re-derives the log from a document. Both live in `canvas.rs` (or a
>   small `state_apply` module).
> - `render`: parse source → `apply_state` → resolve → paint (+ refresh
>   focusable cache). (Phase 1 already parses every render; this just
>   adds the replay step.)
> - `dispatch`: parse source → `apply_state` → resolve → `dispatch_raw`
>   → `intercept_mut` → if dirty re-resolve → `collect_state` back into
>   `self.canvas_state` → discard document → return `InputOutcome`.
> - `snapshot_state`: serialize `self.canvas_state` (None if empty).
> - `restore_state`: deserialize bytes into `self.canvas_state`.
>
> Net effect: NodeId keys still work (the Task 1 spike proved cross-
> process determinism), state survives across calls via the `Send` log
> rather than a retained DOM, and Tasks 15/16 become trivial
> serialize/deserialize wrappers. The task text below is kept for
> reference; follow the amended model where they conflict.

## Task 13: `HtmlCanvas::dispatch` — wire raw + interceptor + dirty re-resolve

**Files:**
- Modify: `crates/savvagent-canvas/src/canvas.rs`

Promote the no-op default `dispatch` to: drop events when frozen; parse a fresh document from source; replay `self.canvas_state` via `apply_state`; resolve; call raw dispatch; call the interceptor (`intercept_mut`); if dirty, re-resolve; re-derive `self.canvas_state` via `collect_state`; return `InputOutcome { effects, dirty }`.

- [ ] **Step 1: Write the failing test**

Append to `canvas.rs`'s tests:

```rust
    #[tokio::test]
    async fn dispatch_link_click_returns_open_url_effect() {
        use savvagent_plugin::{InputEvent, KeyMods, MouseButton, MouseEventKind, MouseEventPortable};

        let mut c = HtmlCanvas::new(
            ContentBlockId(10),
            "<!doctype html><body><a href='https://example.com' style='display:block;width:100px;height:50px'>x</a></body>",
        );
        c.render(PixelSize { width: 200, height: 0 });
        let ev = InputEvent::Mouse(MouseEventPortable {
            kind: MouseEventKind::Press,
            button: Some(MouseButton::Left),
            x_pixel: 16,
            y_pixel: 24,
            modifiers: KeyMods::default(),
        });
        let outcome = c.dispatch(ev).await.expect("dispatch ok");
        assert_eq!(outcome.effects.len(), 1, "expected one effect");
        let savvagent_plugin::Effect::OpenUrl { url, target } = outcome.effects.into_iter().next().unwrap() else {
            panic!("expected OpenUrl");
        };
        assert_eq!(url, "https://example.com");
        assert_eq!(target, savvagent_plugin::UrlTarget::SystemBrowser);
    }

    #[tokio::test]
    async fn dispatch_drops_events_when_frozen() {
        use savvagent_plugin::{InputEvent, KeyMods, MouseButton, MouseEventKind, MouseEventPortable};

        let mut c = HtmlCanvas::new(
            ContentBlockId(11),
            "<!doctype html><body><a href='x'>x</a></body>",
        );
        c.render(PixelSize { width: 200, height: 0 });
        c.freeze();
        let ev = InputEvent::Mouse(MouseEventPortable {
            kind: MouseEventKind::Press,
            button: Some(MouseButton::Left),
            x_pixel: 16, y_pixel: 24,
            modifiers: KeyMods::default(),
        });
        let outcome = c.dispatch(ev).await.expect("dispatch ok");
        assert!(outcome.effects.is_empty(), "frozen canvas must drop effects");
        assert!(!outcome.dirty);
    }
```

- [ ] **Step 2: Run; verify they fail**

```bash
cargo test -p savvagent-canvas canvas::tests::dispatch
```

Expected: FAIL.

- [ ] **Step 3: Implement `dispatch`**

Replace the default `dispatch` impl on `HtmlCanvas`:

```rust
async fn dispatch(
    &mut self,
    event: savvagent_plugin::InputEvent,
) -> Result<savvagent_plugin::InputOutcome, savvagent_plugin::PluginError> {
    if self.frozen {
        return Ok(savvagent_plugin::InputOutcome {
            effects: Vec::new(),
            dirty: false,
        });
    }
    let document = match self.document.as_mut() {
        Some(d) => d,
        None => {
            // Renderer hasn't done its first render pass yet; nothing
            // to hit-test. Treat as no-op.
            return Ok(savvagent_plugin::InputOutcome {
                effects: Vec::new(),
                dirty: false,
            });
        }
    };
    let base: &mut BaseDocument = document.as_mut();
    let raw = crate::events::dispatch_raw(base, &event);
    let outcome = crate::interceptor::intercept_mut(base, raw.target_node);
    if outcome.dirty {
        base.resolve(0.0);
    }
    Ok(savvagent_plugin::InputOutcome {
        effects: outcome.effect.into_iter().collect(),
        dirty: raw.dirty || outcome.dirty,
    })
}
```

(Add necessary `use` statements at the top of `canvas.rs`.)

- [ ] **Step 4: Run; verify they pass**

```bash
cargo test -p savvagent-canvas
```

Expected: all canvas tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-canvas/src/canvas.rs
git commit -m "feat(canvas): dispatch wires raw + interceptor + dirty re-resolve"
```

---

## Task 14: `savvagent-canvas::state` — `CanvasState` serde wire format

**Files:**
- Create: `crates/savvagent-canvas/src/state.rs`
- Modify: `crates/savvagent-canvas/Cargo.toml` (add `serde` + `serde_json` if not already deps)
- Modify: `crates/savvagent-canvas/src/lib.rs`

The opaque blob `HtmlCanvas` writes to / reads from is a `serde_json`-encoded `CanvasState` struct. Includes a schema version so future fields can be added.

If Task 1's spike found NodeId is NOT cross-process stable, swap the `node_key` type from `u32` to a path-based key (e.g. `Vec<u16>` indexing siblings). The struct shape stays the same; only the key type changes.

- [ ] **Step 1: Verify deps**

```bash
grep -E '^serde\b|^serde_json\b' crates/savvagent-canvas/Cargo.toml
```

If serde + serde_json aren't already listed, add to `[dependencies]`:

```toml
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
```

- [ ] **Step 2: Write the failing test**

Create `crates/savvagent-canvas/src/state.rs`:

```rust
//! `CanvasState` — persisted interactive state for `HtmlCanvas`.
//!
//! Wire format: JSON. Phase 2 schema_version = 1. The host treats
//! the bytes as opaque; encoding choice lives entirely in this module.

#![warn(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Phase 2 v3 transcript state blob. Embedded as base64 in the
/// `Canvas` Entry's `state` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CanvasState {
    /// Wire schema version. Phase 2 ships v1; future state additions
    /// bump and the renderer's restore path branches.
    pub schema_version: u32,
    /// Form `<input>` / `<select>` / `<textarea>` values keyed by
    /// node id (if Task 1's spike confirmed NodeId stability) or
    /// by ancestor-sibling-index path.
    pub form_values: BTreeMap<String, String>,
    /// Expanded `<details>` element keys.
    pub open_details: BTreeSet<String>,
    /// Scroll offsets keyed by node id, as (x_px, y_px).
    pub scroll: BTreeMap<String, (u32, u32)>,
    /// Currently focused element id, if any.
    pub focused: Option<String>,
}

impl CanvasState {
    /// Serialize to JSON bytes for `snapshot_state` return.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("CanvasState serializes infallibly")
    }

    /// Parse JSON bytes from `restore_state` input.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|e| format!("CanvasState JSON: {e}"))
    }

    /// True if every field is at its default. Used by `snapshot_state`
    /// to return `None` when nothing interesting happened.
    pub fn is_empty(&self) -> bool {
        self.form_values.is_empty()
            && self.open_details.is_empty()
            && self.scroll.is_empty()
            && self.focused.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_bytes() {
        let mut s = CanvasState {
            schema_version: 1,
            ..CanvasState::default()
        };
        s.form_values.insert("42".into(), "hello".into());
        s.open_details.insert("88".into());
        s.scroll.insert("12".into(), (0, 50));
        s.focused = Some("42".into());

        let bytes = s.to_bytes();
        let back = CanvasState::from_bytes(&bytes).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn default_state_is_empty() {
        let s = CanvasState::default();
        assert!(s.is_empty());
    }

    #[test]
    fn from_bytes_error_on_garbage() {
        assert!(CanvasState::from_bytes(b"not json at all").is_err());
    }
}
```

- [ ] **Step 3: Wire the module**

In `crates/savvagent-canvas/src/lib.rs`:

```rust
mod state;
pub use state::CanvasState;
```

- [ ] **Step 4: Run; verify tests pass**

```bash
cargo test -p savvagent-canvas state::
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-canvas/src/state.rs crates/savvagent-canvas/src/lib.rs crates/savvagent-canvas/Cargo.toml
git commit -m "feat(canvas): CanvasState wire format for snapshot/restore"
```

---

## Task 15: `HtmlCanvas::snapshot_state`

**Files:**
- Modify: `crates/savvagent-canvas/src/canvas.rs`

Walk the live Blitz DOM and capture form values, open details, focused node into a `CanvasState`. Return `None` if every field is empty. Return `None` if the canvas hasn't been rendered yet (no document).

- [ ] **Step 1: Write the failing test**

Append to `canvas.rs`'s tests:

```rust
    #[test]
    fn snapshot_empty_canvas_returns_none() {
        let c = HtmlCanvas::new(
            ContentBlockId(20),
            "<!doctype html><body><p>plain</p></body>",
        );
        assert!(c.snapshot_state().is_none());
    }

    #[test]
    fn snapshot_captures_open_details() {
        // Build a canvas with <details>, render it, toggle via dispatch,
        // then snapshot — expect non-None with open_details populated.
        let mut c = HtmlCanvas::new(
            ContentBlockId(21),
            "<!doctype html><body><details><summary id='s'>x</summary><p>y</p></details></body>",
        );
        c.render(PixelSize { width: 200, height: 0 });
        // Toggle via the interceptor's mutating path indirectly: simulate
        // a click on the summary. We need its layout to land the click —
        // for simplicity, find the node and call intercept_mut directly.
        // (The high-level dispatch test path is already covered in Task 13.)
        let summary_id = {
            // Test-only access; expose with #[cfg(test)] if needed.
            let doc = c.test_document().expect("document");
            let base = doc.as_ref();
            // Walk to find the summary; reuse helper from interceptor tests
            // OR inline here. For brevity, assume a helper.
            // ... (use the existing find_node_by_tag pattern) ...
            find_node_by_tag(base, "summary").expect("summary present")
        };
        {
            let doc = c.test_document_mut().expect("document");
            let base = doc.as_mut();
            let _ = crate::interceptor::intercept_mut(base, Some(summary_id));
            base.resolve(0.0);
        }
        let snap = c.snapshot_state().expect("non-empty after toggle");
        let state = crate::state::CanvasState::from_bytes(&snap).unwrap();
        assert!(!state.open_details.is_empty());
    }
```

Add the `#[cfg(test)]` accessors:

```rust
#[cfg(test)]
impl HtmlCanvas {
    fn test_document(&self) -> Option<&blitz_html::HtmlDocument> {
        self.document.as_ref()
    }
    fn test_document_mut(&mut self) -> Option<&mut blitz_html::HtmlDocument> {
        self.document.as_mut()
    }
}
```

- [ ] **Step 2: Run; verify they fail**

```bash
cargo test -p savvagent-canvas canvas::tests::snapshot
```

Expected: FAIL.

- [ ] **Step 3: Implement `snapshot_state`**

In `canvas.rs`:

```rust
fn snapshot_state(&self) -> Option<Vec<u8>> {
    let document = self.document.as_ref()?;
    let base = document.as_ref();
    let mut state = crate::state::CanvasState {
        schema_version: 1,
        ..Default::default()
    };
    collect_state(base, &mut state);
    if let Some(focused_idx) = self.focused {
        if let Some(cache) = self.focusable_cache.as_ref() {
            if let Some((node_id, _)) = cache.get(focused_idx as usize) {
                state.focused = Some(node_id.to_string());
            }
        }
    }
    if state.is_empty() {
        None
    } else {
        Some(state.to_bytes())
    }
}
```

And the walk helper (free function in `canvas.rs`):

```rust
fn collect_state(base: &BaseDocument, state: &mut crate::state::CanvasState) {
    walk_state(base, base.root_element().id, state);
}

fn walk_state(base: &BaseDocument, node_id: u32, state: &mut crate::state::CanvasState) {
    let node = match base.try_node_by_id(node_id) {
        Some(n) => n,
        None => return,
    };
    if let Some(element) = node.data.element_data() {
        let tag = element.name.local.as_ref();
        match tag {
            "details" => {
                if element.attr(blitz_dom::local_name!("open")).is_some() {
                    state.open_details.insert(node_id.to_string());
                }
            }
            "input" | "select" | "textarea" => {
                if let Some(name) = element.attr(blitz_dom::local_name!("name")) {
                    // Use the value attribute as our snapshot source.
                    let value = element
                        .attr(blitz_dom::local_name!("value"))
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    if !value.is_empty() || !name.is_empty() {
                        state
                            .form_values
                            .insert(node_id.to_string(), value);
                    }
                }
            }
            _ => {}
        }
    }
    for child in node.children.iter().copied() {
        walk_state(base, child, state);
    }
}
```

- [ ] **Step 4: Run; verify they pass**

```bash
cargo test -p savvagent-canvas canvas::tests::snapshot
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-canvas/src/canvas.rs
git commit -m "feat(canvas): snapshot_state captures form/details/focus"
```

---

## Task 16: `HtmlCanvas::restore_state`

**Files:**
- Modify: `crates/savvagent-canvas/src/canvas.rs`

Parse the blob; mutate the DOM to set `value` attributes on inputs, `open` on details, and `set_focus` to the focused index. On JSON parse failure, return `PluginError::StateRestoreFailed`. After mutating, call `base.resolve(0.0)` so the next render reflects restored state.

- [ ] **Step 1: Write the failing test**

Append to `canvas.rs`'s tests:

```rust
    #[test]
    fn restore_state_round_trips_details_open() {
        let mut a = HtmlCanvas::new(
            ContentBlockId(30),
            "<!doctype html><body><details><summary>s</summary><p>y</p></details></body>",
        );
        a.render(PixelSize { width: 200, height: 0 });
        // Toggle details open in `a`.
        let summary_id = {
            let base = a.test_document().unwrap().as_ref();
            find_node_by_tag(base, "summary").unwrap()
        };
        {
            let base = a.test_document_mut().unwrap().as_mut();
            let _ = crate::interceptor::intercept_mut(base, Some(summary_id));
            base.resolve(0.0);
        }
        let snap = a.snapshot_state().unwrap();

        // Fresh canvas with same source; restore the snapshot.
        let mut b = HtmlCanvas::new(
            ContentBlockId(31),
            "<!doctype html><body><details><summary>s</summary><p>y</p></details></body>",
        );
        b.render(PixelSize { width: 200, height: 0 });
        b.restore_state(&snap).expect("restore ok");
        // Take a snapshot from b; expect open_details non-empty.
        let snap_b = b.snapshot_state().expect("non-empty after restore");
        let state_b = crate::state::CanvasState::from_bytes(&snap_b).unwrap();
        assert!(!state_b.open_details.is_empty());
    }

    #[test]
    fn restore_state_returns_error_on_garbage() {
        let mut c = HtmlCanvas::new(
            ContentBlockId(32),
            "<!doctype html><body></body>",
        );
        c.render(PixelSize { width: 100, height: 0 });
        let err = c.restore_state(b"not json").unwrap_err();
        assert!(
            matches!(err, savvagent_plugin::PluginError::StateRestoreFailed(_)),
            "expected StateRestoreFailed, got {err:?}",
        );
    }
```

- [ ] **Step 2: Run; verify they fail**

```bash
cargo test -p savvagent-canvas canvas::tests::restore
```

Expected: FAIL.

- [ ] **Step 3: Implement `restore_state`**

In `canvas.rs`:

```rust
fn restore_state(
    &mut self,
    bytes: &[u8],
) -> Result<(), savvagent_plugin::PluginError> {
    let state = crate::state::CanvasState::from_bytes(bytes)
        .map_err(savvagent_plugin::PluginError::StateRestoreFailed)?;
    let document = match self.document.as_mut() {
        Some(d) => d,
        None => return Ok(()), // No-op pre-render; ignore restore quietly
    };
    let base = document.as_mut();
    apply_state(base, &state);
    base.resolve(0.0);
    // Sync focus to the cache.
    if let Some(focused_id_str) = state.focused.as_ref() {
        if let Ok(focused_id) = focused_id_str.parse::<u32>() {
            if let Some(cache) = self.focusable_cache.as_ref() {
                self.focused = cache
                    .iter()
                    .position(|(id, _)| *id == focused_id)
                    .map(|i| i as u32);
            }
        }
    }
    Ok(())
}
```

And the apply helper:

```rust
fn apply_state(base: &mut BaseDocument, state: &crate::state::CanvasState) {
    apply_state_walk(base, base.root_element().id, state);
}

fn apply_state_walk(base: &mut BaseDocument, node_id: u32, state: &crate::state::CanvasState) {
    // Snapshot keys are stringified node ids; look up by parse.
    let key = node_id.to_string();
    // Read tag first (immutable borrow), then mutate via setters.
    let tag = base
        .try_node_by_id(node_id)
        .and_then(|n| n.data.element_data())
        .map(|e| e.name.local.as_ref().to_string());
    match tag.as_deref() {
        Some("details") => {
            if state.open_details.contains(&key) {
                base.set_attribute(node_id, blitz_dom::local_name!("open"), "");
            }
        }
        Some("input") | Some("select") | Some("textarea") => {
            if let Some(v) = state.form_values.get(&key) {
                base.set_attribute(node_id, blitz_dom::local_name!("value"), v);
            }
        }
        _ => {}
    }
    // Collect children first to release the borrow before recursing.
    let children: Vec<u32> = base
        .try_node_by_id(node_id)
        .map(|n| n.children.iter().copied().collect())
        .unwrap_or_default();
    for child in children {
        apply_state_walk(base, child, state);
    }
}
```

- [ ] **Step 4: Run; verify they pass**

```bash
cargo test -p savvagent-canvas canvas::tests::restore
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-canvas/src/canvas.rs
git commit -m "feat(canvas): restore_state applies persisted DOM state"
```

---

## Task 17: Tool-emitted HTML translation in `ToolRegistry::call`

**Files:**
- Modify: `crates/savvagent-host/src/session.rs` (or wherever tool-result translation lives — find via `grep`)

The MCP tool-result content array can contain `{"type":"html","source":"..."}` items. Today the host concatenates all `text` items into one `ContentBlock::Text`. Phase 2 walks the array and emits one block per item: `text` items still concatenate; `html` items each become a `ContentBlock::Html`; the host assigns the `ContentBlockId`.

- [ ] **Step 1: Locate the translation path**

```bash
grep -rn "ContentBlock::Text\|tool.*result\|content.*array" crates/savvagent-host/src/ | head -20
```

Find the function (likely in `session.rs` or `tool_call.rs`) that takes the MCP `CallToolResult` and turns it into one or more `ContentBlock`s. Read it; understand the current concatenation logic.

- [ ] **Step 2: Write the failing test**

Add to the appropriate `#[cfg(test)] mod tests` in `savvagent-host`:

```rust
    #[test]
    fn tool_result_html_item_becomes_html_block() {
        // Construct a synthetic MCP CallToolResult with mixed text + html
        // content items. The exact constructor depends on rmcp's types.
        // Pattern (adjust to the actual API):
        let result = make_call_tool_result(vec![
            content_text("Files written:"),
            content_html("<!doctype html><body><p>summary</p></body>"),
        ]);
        let blocks = translate_tool_result_to_blocks(&result);
        // Two blocks: Text("Files written:") then Html(...).
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Files written:"),
            other => panic!("expected Text first, got {other:?}"),
        }
        match &blocks[1] {
            ContentBlock::Html { source } => {
                assert!(source.contains("<p>summary</p>"));
            }
            other => panic!("expected Html second, got {other:?}"),
        }
    }
```

(The helper names `make_call_tool_result`, `content_text`, `content_html`, `translate_tool_result_to_blocks` are placeholders. Use whatever the codebase already has; if missing, create thin test helpers.)

- [ ] **Step 3: Run; verify it fails**

```bash
cargo test -p savvagent-host tool_result_html_item_becomes_html_block
```

Expected: FAIL.

- [ ] **Step 4: Implement**

In the translation function (found in Step 1), change the body from "collect all text into one Text block" to "iterate, emit per-type":

```rust
pub fn translate_tool_result_to_blocks(result: &CallToolResult) -> Vec<ContentBlock> {
    let mut out: Vec<ContentBlock> = Vec::new();
    let mut current_text = String::new();
    for item in result.content.iter() {
        match item {
            // rmcp's actual Content variants; adjust shape:
            Content::Text { text } => {
                if !current_text.is_empty() {
                    current_text.push('\n');
                }
                current_text.push_str(text);
            }
            // The MCP shape for html may be a "raw" content item with type=="html".
            // If rmcp doesn't yet model `html` natively, we read it from a generic
            // JSON-typed content item (Content::Resource { ... }) or via a custom
            // parse of the underlying JSON. Adapt to actual rmcp API.
            Content::Html { source } => {
                if !current_text.is_empty() {
                    out.push(ContentBlock::Text { text: std::mem::take(&mut current_text) });
                }
                out.push(ContentBlock::Html { source: source.clone() });
            }
            _ => {
                // Unknown content type: stringify into the text buffer with warning.
                tracing::warn!(?item, "unknown tool result content type — stringifying");
                if !current_text.is_empty() {
                    current_text.push('\n');
                }
                current_text.push_str(&format!("{item:?}"));
            }
        }
    }
    if !current_text.is_empty() {
        out.push(ContentBlock::Text { text: current_text });
    }
    out
}
```

If rmcp doesn't have a native `Html` variant, two options:

(a) Add a custom step that recognizes `{"type":"html","source":...}` shapes from raw JSON before rmcp's typed deserialization. Pattern: deserialize to `serde_json::Value` first; classify each content item by its `"type"` field; route to the typed enum only for known shapes.

(b) Reuse rmcp's `Content::Resource` or generic `Content::Unknown` (if either exists) as the carrier and document the trick in a comment.

Pick (a) for cleanliness — it makes the html type a first-class translation target without depending on rmcp upstream.

- [ ] **Step 5: Run; verify it passes**

```bash
cargo test -p savvagent-host
```

Expected: PASS.

- [ ] **Step 6: Verify host-level integration** — write a small integration test that drives a fake tool returning html, asserts the Host emits the Html block into the conversation messages.

Append to an existing host integration test file (or create `crates/savvagent-host/tests/tool_html.rs`):

```rust
#[tokio::test]
async fn host_emits_html_block_from_tool_result() {
    // Construct a minimal Host with a synthetic tool that returns
    // a Content::Html item. Drive a turn; assert the resulting
    // conversation messages contain a ContentBlock::Html.
    todo!("populate using the existing host test harness patterns")
}
```

Use the existing host test fixtures (`crates/savvagent-host/tests/support/`) for the harness. Drop the `todo!` once the fixture is wired.

- [ ] **Step 7: Run integration test**

```bash
cargo test -p savvagent-host --test tool_html
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent-host
git commit -m "feat(host): translate MCP html content items to ContentBlock::Html"
```

---

> **PLAN AMENDMENT #2 (2026-05-25, during execution):** Investigation
> for Task 18 found the plan's persistence model is wrong. `Entry`
> (`crates/savvagent/src/app.rs`) derives only `Debug, Clone` — it has
> NO serde and is NEVER persisted. The authoritative transcript is
> `TranscriptFile { schema_version, saved_at, messages: Vec<Message> }`
> (`savvagent-host`), holding SPP `ContentBlock`s; `Host::load_transcript`
> + `App::replay_transcript` rebuild `Entry::Canvas` from `messages` on
> `/resume`. Therefore:
>
> - **Task 18 is REWORKED:** instead of adding `state` to `Entry::Canvas`
>   and a `#[serde(other)] Unknown` on `Entry`, add an optional
>   `state: Option<String>` field to **`ContentBlock::Html { source, state }`**
>   in `savvagent-protocol` (with `#[serde(default, skip_serializing_if =
>   "Option::is_none")]`). This persists naturally in `TranscriptFile.messages`.
>   Bump the SPP spec/version note. The 4 providers already translate
>   `Html`→text in history, so the new field is inert for them; old
>   transcripts (no `state`) load via the serde default.
> - **Task 26 (snapshot triggers)** writes the renderer's
>   `snapshot_state()` (base64) into the matching `ContentBlock::Html.state`
>   in the host's message list before the `TranscriptFile` is written,
>   rather than into an `Entry` field.
> - **Task 27 (restore on /resume)** reads `ContentBlock::Html.state` from
>   the loaded messages and passes it to `HtmlCanvas::restore_state` when
>   `replay_transcript` constructs each canvas.
> - **Execution order:** per user direction, interaction Tasks 19-25 run
>   FIRST (they don't touch persistence); reworked 18/26/27 follow.
> - The `Entry::Unknown` forward-compat idea is dropped (Entry isn't
>   serialized, so it's moot). If `ContentBlock` forward-compat is wanted,
>   that's separate follow-up work, out of Phase 2 scope.

## Task 18 (REWORKED — see amendment #2 above): `ContentBlock::Html { state }` persistence field

**Files:**
- Modify: `crates/savvagent/src/app.rs`

Two changes:
1. Add an optional `state: Option<String>` (base64-encoded blob) field to `Entry::Canvas`.
2. Add `#[serde(other)] Unknown` variant so future Entry variants degrade gracefully when read by older builds. Renders as a one-line placeholder in the conversation view.

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` in `crates/savvagent/src/app.rs`:

```rust
    #[test]
    fn entry_canvas_state_round_trip() {
        let entry = Entry::Canvas {
            id: 42,
            source: "<!doctype html><body>x</body>".into(),
            state: Some("dGVzdC1ieXRlcw==".into()),  // base64 of "test-bytes"
        };
        let v = serde_json::to_value(&entry).unwrap();
        let back: Entry = serde_json::from_value(v).unwrap();
        match back {
            Entry::Canvas { id, source, state } => {
                assert_eq!(id, 42);
                assert!(source.contains("<body>"));
                assert_eq!(state.as_deref(), Some("dGVzdC1ieXRlcw=="));
            }
            other => panic!("expected Canvas, got {other:?}"),
        }
    }

    #[test]
    fn entry_unknown_absorbs_future_variants() {
        let raw = serde_json::json!({
            "type": "future_thing_we_did_not_invent_yet",
            "some_field": "with_a_value",
        });
        let entry: Entry = serde_json::from_value(raw).expect(
            "future entry variants must deserialize as Unknown, not error"
        );
        assert!(matches!(entry, Entry::Unknown));
    }
```

- [ ] **Step 2: Run; verify they fail**

```bash
cargo test -p savvagent --bin savvagent entry_canvas_state_round_trip entry_unknown_absorbs_future_variants
```

Expected: FAIL (no `state` field; no `Unknown` variant).

- [ ] **Step 3: Update `Entry`**

In `crates/savvagent/src/app.rs`, modify the `Entry` enum:

```rust
pub enum Entry {
    // ... existing variants ...

    Canvas {
        id: u32,
        source: String,
        /// Base64-encoded opaque blob produced by
        /// `ContentRenderer::snapshot_state`. `None` when the canvas
        /// is freshly created (no interaction yet) or the renderer
        /// returned `None` from `snapshot_state`.
        ///
        /// Phase 2 added this field. Phase 1 transcripts (without
        /// the field) deserialize fine via `#[serde(default)]`.
        #[serde(default)]
        state: Option<String>,
    },

    /// Phase 2: future Entry variants this build doesn't know about
    /// deserialize as `Unknown` rather than erroring the whole
    /// transcript load. Rendered as a single-line "[unknown entry
    /// type — open in a newer savvagent]" placeholder.
    #[serde(other)]
    Unknown,
}
```

- [ ] **Step 4: Update all `Entry::Canvas { ... }` literals**

`#[serde(default)]` means `state` is optional on deserialize. But Rust struct-variant literals must include all fields. Search:

```bash
grep -rn "Entry::Canvas {" crates/savvagent/src/ | head -20
```

For each match, add `state: None,` to the literal.

- [ ] **Step 5: Handle the new `Unknown` variant in `match`es**

```bash
cargo build --workspace 2>&1 | grep -E "non-exhaustive|missing match arm" | head -20
```

For each compile error, extend the match to handle `Entry::Unknown` — typically by rendering it as a one-line note. Example in `ui.rs`:

```rust
Entry::Unknown => {
    // Future Entry variant; render a placeholder so the transcript
    // is still readable.
    ratatui::widgets::Paragraph::new(
        "[unknown entry type — open in a newer savvagent build]"
    )
    .style(/* dim style */ )
    .render(area, buf);
}
```

- [ ] **Step 6: Run; verify the tests pass**

```bash
cargo test -p savvagent
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/savvagent/src/app.rs crates/savvagent/src/ui.rs
git commit -m "feat(tui): Entry::Canvas.state + Entry::Unknown serde fallback"
```

---

## Task 19: `AppFocus::Canvas` variant + state transitions

**Files:**
- Modify: `crates/savvagent/src/app.rs`

Add the focus variant carrying the focused canvas's `ContentBlockId` and (optionally) the currently focused element index within that canvas. Update the focus-transition helpers to fire `freeze`/`thaw` on the renderer at the right moments.

- [ ] **Step 1: Locate `AppFocus`**

```bash
grep -n "pub enum AppFocus" crates/savvagent/src/app.rs
```

- [ ] **Step 2: Write the failing test**

Append to `app.rs`'s tests:

```rust
    #[test]
    fn app_focus_canvas_variant_exists_and_carries_id() {
        let f = AppFocus::Canvas {
            id: ContentBlockId(7),
            element_idx: Some(2),
        };
        match f {
            AppFocus::Canvas { id, element_idx } => {
                assert_eq!(id, ContentBlockId(7));
                assert_eq!(element_idx, Some(2));
            }
            _ => panic!("expected Canvas variant"),
        }
    }
```

- [ ] **Step 3: Run; verify it fails**

```bash
cargo test -p savvagent app_focus_canvas_variant
```

Expected: FAIL — variant doesn't exist or has a different shape.

- [ ] **Step 4: Add / update the variant**

If `AppFocus` already has a placeholder `Canvas(ContentBlockId)` from the Phase 1 spec sketch, change to the struct variant:

```rust
pub enum AppFocus {
    ChatInput,
    ScreenStack,
    /// Phase 2: focus is inside an inline canvas. `element_idx`
    /// is the index into the canvas's `focusable_elements()` list,
    /// `None` if no specific element is focused yet (e.g. just-
    /// clicked-the-canvas-but-no-Tab-yet state).
    Canvas {
        id: ContentBlockId,
        element_idx: Option<u32>,
    },
}
```

Add the `use savvagent_plugin::ContentBlockId;` if not already imported.

- [ ] **Step 5: Extend `App` with the canvas focus accessor methods**

Add to `App`'s impl:

```rust
impl App {
    /// True iff `self.focus` is `AppFocus::Canvas` for the given block id.
    pub fn is_canvas_focused(&self, id: ContentBlockId) -> bool {
        matches!(self.focus, AppFocus::Canvas { id: x, .. } if x == id)
    }

    /// Transition focus to the given canvas. Calls `freeze` on any
    /// previously-focused canvas (if it's a different id) and
    /// `thaw` on the incoming one.
    pub fn focus_canvas(&mut self, id: ContentBlockId, element_idx: Option<u32>) {
        // Freeze the previous canvas if it was a different one.
        if let AppFocus::Canvas { id: prev, .. } = self.focus {
            if prev != id {
                if let Some(renderer) = self.canvas_registry.get_mut(prev) {
                    renderer.freeze();
                }
            }
        }
        // Thaw the new one.
        if let Some(renderer) = self.canvas_registry.get_mut(id) {
            renderer.thaw();
        }
        self.focus = AppFocus::Canvas { id, element_idx };
    }

    /// Transition focus away from a canvas back to chat input.
    pub fn unfocus_canvas(&mut self) {
        if let AppFocus::Canvas { id, .. } = self.focus {
            if let Some(renderer) = self.canvas_registry.get_mut(id) {
                renderer.freeze();
            }
        }
        self.focus = AppFocus::ChatInput;
    }
}
```

(`canvas_registry` is the existing `CanvasRegistry` field from Phase 1.)

- [ ] **Step 6: Handle `AppFocus::Canvas` in any existing matches**

```bash
cargo build --workspace 2>&1 | grep "non-exhaustive\|match" | head
```

Address any non-exhaustive match warnings.

- [ ] **Step 7: Run; verify tests pass**

```bash
cargo test -p savvagent
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent/src/app.rs
git commit -m "feat(tui): AppFocus::Canvas with freeze/thaw transitions"
```

---

## Task 20: Focus chrome (1-cell border)

**Files:**
- Modify: `crates/savvagent/src/ui.rs`

When the conversation log renders a canvas and `App.focus == AppFocus::Canvas { id, .. }` matches that canvas, draw a 1-cell-wide ratatui border around the canvas's image region.

- [ ] **Step 1: Locate the existing canvas render path**

```bash
grep -n "Entry::Canvas\|StatefulImage\|ratatui_image" crates/savvagent/src/ui.rs
```

- [ ] **Step 2: Write the test**

The focus chrome is a visual concern. A pure unit test would need a ratatui `TestBackend`. Add a `TestBackend`-based test that renders a small `Entry::Canvas` with `App.focus = Canvas(id)` and asserts that the border characters appear at the expected cells.

Append to `ui.rs`'s tests (or create one if none exist):

```rust
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn focused_canvas_gets_border() {
        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = build_test_app_with_canvas(ContentBlockId(1));
        app.focus_canvas(ContentBlockId(1), None);
        terminal.draw(|f| {
            // Drive whatever your existing render entry point is here.
            ui::render(f, &mut app);
        }).unwrap();
        let buf = terminal.backend().buffer().clone();
        // Assert there's at least one border-corner character (┌, ┐, └, ┘).
        let has_corner = buf
            .content()
            .iter()
            .any(|cell| matches!(cell.symbol(), "┌" | "┐" | "└" | "┘"));
        assert!(has_corner, "expected a border corner somewhere in the buffer");
    }
```

(`build_test_app_with_canvas` is a fixture helper; create one if it doesn't exist in `test_helpers.rs`.)

- [ ] **Step 3: Run; verify it fails**

```bash
cargo test -p savvagent focused_canvas_gets_border
```

Expected: FAIL.

- [ ] **Step 4: Wire the border**

In the canvas render path in `ui.rs`, before calling `StatefulImage::render`:

```rust
let is_focused = app.is_canvas_focused(ContentBlockId(canvas_id));
let inner_area = if is_focused {
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(/* highlight palette color */)
        .border_type(ratatui::widgets::BorderType::Plain);
    let inner = block.inner(canvas_area);
    block.render(canvas_area, buf);
    inner
} else {
    canvas_area
};
// Render the image into `inner_area`.
```

- [ ] **Step 5: Run; verify it passes**

```bash
cargo test -p savvagent focused_canvas_gets_border
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/src/ui.rs crates/savvagent/src/test_helpers.rs
git commit -m "feat(tui): focus chrome around focused canvas"
```

---

## Task 21: Mouse routing — click in canvas focuses + dispatches

**Files:**
- Modify: `crates/savvagent/src/tui.rs`
- Modify: `crates/savvagent/src/ui.rs` (track canvas cell rects)

Crossterm mouse mode is already enabled (ratatui-image needed it in Phase 1). On each mouse event the TUI receives, walk the table of visible canvases (with their cell rects from the most recent render), find the one the click landed in, transition `AppFocus::Canvas`, and dispatch the event into the renderer via `coords::cell_to_pixel`.

- [ ] **Step 1: Add canvas cell-rect tracking on `App`**

In `app.rs`:

```rust
pub struct App {
    // ... existing fields ...

    /// Cell rect each visible canvas occupies in the conversation log,
    /// keyed by canvas id. Populated during render in `ui.rs`; consumed
    /// by mouse-event routing in `tui.rs`. Stale entries (canvases that
    /// scrolled out of view) are pruned on each render.
    pub canvas_rects: HashMap<ContentBlockId, savvagent_canvas::CellRect>,
}
```

Initialize in `App::new` (empty map). Add `use savvagent_canvas::CellRect;` and `use std::collections::HashMap;` if not present.

- [ ] **Step 2: Populate `canvas_rects` during render**

In the canvas render path in `ui.rs`, after computing `canvas_area`:

```rust
app.canvas_rects.insert(
    ContentBlockId(canvas_id),
    savvagent_canvas::CellRect {
        col: canvas_area.x,
        row: canvas_area.y,
        width: canvas_area.width,
        height: canvas_area.height,
    },
);
```

Clear the map at the start of each render pass so stale entries don't accumulate.

- [ ] **Step 3: Write the failing test**

Append to `tui.rs`'s tests (or wherever mouse routing lives):

```rust
    #[tokio::test]
    async fn mouse_press_inside_canvas_focuses_it() {
        let mut app = build_test_app_with_canvas(ContentBlockId(1));
        // Pretend the canvas was rendered at cells (col=2..12, row=3..8).
        app.canvas_rects.insert(
            ContentBlockId(1),
            savvagent_canvas::CellRect { col: 2, row: 3, width: 10, height: 5 },
        );
        let event = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 5,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::empty(),
        };
        let cell_size = savvagent_canvas::CellPixelSize { width: 8, height: 16 };
        handle_mouse_event(&mut app, event, cell_size).await;
        assert!(app.is_canvas_focused(ContentBlockId(1)));
    }
```

- [ ] **Step 4: Run; verify it fails**

```bash
cargo test -p savvagent mouse_press_inside_canvas_focuses_it
```

Expected: FAIL — `handle_mouse_event` doesn't exist or doesn't yet route to canvas focus.

- [ ] **Step 5: Implement mouse routing**

In `tui.rs`:

```rust
pub async fn handle_mouse_event(
    app: &mut App,
    event: crossterm::event::MouseEvent,
    cell_size: savvagent_canvas::CellPixelSize,
) {
    // Find a canvas containing the event's cell.
    let hit = app
        .canvas_rects
        .iter()
        .find(|(_, rect)| savvagent_canvas::contains_cell(**rect, event.column, event.row))
        .map(|(id, rect)| (*id, *rect));
    let Some((canvas_id, rect)) = hit else {
        // Pass through to whatever the chat input does (transcript scroll, etc.)
        return handle_non_canvas_mouse(app, event).await;
    };

    let (x_px, y_px) = match savvagent_canvas::cell_to_pixel(
        rect, cell_size, event.column, event.row,
    ) {
        Some(p) => p,
        None => return,
    };

    // Focus if not already focused.
    if !app.is_canvas_focused(canvas_id) {
        app.focus_canvas(canvas_id, None);
    }

    // Translate kind and dispatch.
    let kind = match event.kind {
        crossterm::event::MouseEventKind::Down(_) => savvagent_plugin::MouseEventKind::Press,
        crossterm::event::MouseEventKind::Up(_) => savvagent_plugin::MouseEventKind::Release,
        crossterm::event::MouseEventKind::Drag(_) | crossterm::event::MouseEventKind::Moved => {
            savvagent_plugin::MouseEventKind::Move
        }
        crossterm::event::MouseEventKind::ScrollUp => savvagent_plugin::MouseEventKind::ScrollUp,
        crossterm::event::MouseEventKind::ScrollDown => savvagent_plugin::MouseEventKind::ScrollDown,
    };
    let button = match event.kind {
        crossterm::event::MouseEventKind::Down(b)
        | crossterm::event::MouseEventKind::Up(b)
        | crossterm::event::MouseEventKind::Drag(b) => Some(match b {
            crossterm::event::MouseButton::Left => savvagent_plugin::MouseButton::Left,
            crossterm::event::MouseButton::Right => savvagent_plugin::MouseButton::Right,
            crossterm::event::MouseButton::Middle => savvagent_plugin::MouseButton::Middle,
        }),
        _ => None,
    };

    let portable = savvagent_plugin::MouseEventPortable {
        kind, button, x_pixel: x_px, y_pixel: y_px,
        modifiers: convert_modifiers(event.modifiers),
    };

    if let Some(renderer) = app.canvas_registry.get_mut(canvas_id) {
        if let Ok(outcome) = renderer.dispatch(savvagent_plugin::InputEvent::Mouse(portable)).await {
            apply_canvas_effects(app, outcome.effects).await;
            // Mark dirty for re-render if needed.
        }
    }
}

fn convert_modifiers(km: crossterm::event::KeyModifiers) -> savvagent_plugin::KeyMods {
    let mut out = savvagent_plugin::KeyMods::default();
    out.shift = km.contains(crossterm::event::KeyModifiers::SHIFT);
    out.ctrl = km.contains(crossterm::event::KeyModifiers::CONTROL);
    out.alt = km.contains(crossterm::event::KeyModifiers::ALT);
    out
}

async fn apply_canvas_effects(app: &mut App, effects: Vec<savvagent_plugin::Effect>) {
    for effect in effects {
        match effect {
            savvagent_plugin::Effect::OpenUrl { url, target } => match target {
                savvagent_plugin::UrlTarget::SystemBrowser => {
                    let _ = open_in_system_browser(&url).await;
                }
                savvagent_plugin::UrlTarget::ContinueConversation => {
                    app.pending_user_prompt = Some(url);
                }
            },
            other => {
                // Route through the existing effect dispatcher.
                app.dispatch_effect(other).await;
            }
        }
    }
}
```

(`open_in_system_browser` is a small helper that shells out — Task 24 implements it for Ctrl-O; here we reuse. If not yet implemented, factor it now into `crates/savvagent/src/plugin/builtin/html_canvas/open_in_browser.rs`.)

- [ ] **Step 6: Wire the entrypoint**

In the main event loop in `tui.rs`, route `Event::Mouse` through `handle_mouse_event`:

```rust
Event::Mouse(mouse) => {
    handle_mouse_event(&mut app, mouse, cell_pixel_size).await;
}
```

`cell_pixel_size` should be cached in `App` at startup from `ratatui-image::Picker`.

- [ ] **Step 7: Run; verify it passes**

```bash
cargo test -p savvagent mouse_press_inside_canvas_focuses_it
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent/src/tui.rs crates/savvagent/src/app.rs crates/savvagent/src/ui.rs
git commit -m "feat(tui): mouse routing focuses canvas + dispatches events"
```

---

## Task 22: Keyboard routing — Tab/Shift-Tab/Esc within canvas

**Files:**
- Modify: `crates/savvagent/src/tui.rs`

When `AppFocus::Canvas`, intercept Tab (next focusable), Shift-Tab (prev), Esc (unfocus). Anything else passes through to the renderer as `InputEvent::Key`.

- [ ] **Step 1: Write the failing test**

```rust
    #[tokio::test]
    async fn tab_cycles_focused_element() {
        let mut app = build_test_app_with_canvas_with_focusables(ContentBlockId(1), 3);
        app.focus_canvas(ContentBlockId(1), Some(0));
        let event = crossterm::event::KeyEvent {
            code: crossterm::event::KeyCode::Tab,
            modifiers: crossterm::event::KeyModifiers::empty(),
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        handle_key_event(&mut app, event).await;
        if let AppFocus::Canvas { element_idx, .. } = app.focus {
            assert_eq!(element_idx, Some(1));
        } else { panic!("expected canvas focus"); }
    }

    #[tokio::test]
    async fn esc_unfocuses_canvas() {
        let mut app = build_test_app_with_canvas(ContentBlockId(2));
        app.focus_canvas(ContentBlockId(2), None);
        let event = crossterm::event::KeyEvent {
            code: crossterm::event::KeyCode::Esc,
            modifiers: crossterm::event::KeyModifiers::empty(),
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        handle_key_event(&mut app, event).await;
        assert!(matches!(app.focus, AppFocus::ChatInput));
    }
```

- [ ] **Step 2: Run; verify they fail**

```bash
cargo test -p savvagent tab_cycles_focused_element esc_unfocuses_canvas
```

Expected: FAIL.

- [ ] **Step 3: Implement the keyboard router**

In `tui.rs`:

```rust
pub async fn handle_key_event(app: &mut App, event: crossterm::event::KeyEvent) {
    use crossterm::event::{KeyCode, KeyModifiers};

    let in_canvas = matches!(app.focus, AppFocus::Canvas { .. });
    if !in_canvas {
        return handle_non_canvas_key(app, event).await;
    }

    // Built-in canvas keys (precedence over plugin bindings — see Task 25).
    match event.code {
        KeyCode::Esc => {
            app.unfocus_canvas();
            return;
        }
        KeyCode::Tab => {
            cycle_canvas_focus(app, 1).await;
            return;
        }
        KeyCode::BackTab => {
            // Crossterm reports Shift-Tab as BackTab.
            cycle_canvas_focus(app, -1).await;
            return;
        }
        // Ctrl-J / Ctrl-K / Ctrl-O lands in Tasks 23 + 24.
        _ => {}
    }

    // Everything else → dispatch to renderer as InputEvent::Key.
    if let AppFocus::Canvas { id, .. } = app.focus {
        let portable = savvagent_plugin::KeyEventPortable {
            // Map KeyCode → portable shape. Reuse the same mapper Phase 1
            // used for KeyScope plugin keybindings.
            ..convert_key_event(event)
        };
        if let Some(renderer) = app.canvas_registry.get_mut(id) {
            if let Ok(outcome) = renderer
                .dispatch(savvagent_plugin::InputEvent::Key(portable))
                .await
            {
                apply_canvas_effects(app, outcome.effects).await;
            }
        }
    }
}

async fn cycle_canvas_focus(app: &mut App, delta: i32) {
    let AppFocus::Canvas { id, element_idx } = app.focus else {
        return;
    };
    let len = app
        .canvas_registry
        .get(id)
        .map(|r| r.focusable_elements().len())
        .unwrap_or(0) as i32;
    if len == 0 {
        return;
    }
    let next_idx = match element_idx {
        None => if delta > 0 { 0 } else { (len - 1) as u32 },
        Some(i) => {
            let n = (i as i32 + delta).rem_euclid(len);
            n as u32
        }
    };
    if let AppFocus::Canvas { id: x, element_idx: ref mut e } = app.focus {
        *e = Some(next_idx);
        if let Some(renderer) = app.canvas_registry.get_mut(x) {
            renderer.set_focus(Some(next_idx));
        }
    }
}
```

`convert_key_event` reuses whatever mapping Phase 1 set up for plugin keybindings — find it via `grep KeyEventPortable crates/savvagent/src/`.

- [ ] **Step 4: Run; verify it passes**

```bash
cargo test -p savvagent tab_cycles_focused_element esc_unfocuses_canvas
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/tui.rs
git commit -m "feat(tui): keyboard routing Tab/Shift-Tab/Esc within canvas"
```

---

## Task 23: Keyboard routing — Ctrl-J / Ctrl-K canvas traversal

**Files:**
- Modify: `crates/savvagent/src/tui.rs`

Ctrl-J jumps focus to the next canvas in the transcript log (after the currently focused one); Ctrl-K jumps to the previous. From `ChatInput` focus, Ctrl-J jumps to the first canvas in the log; Ctrl-K to the last.

- [ ] **Step 1: Write the failing test**

```rust
    #[tokio::test]
    async fn ctrl_j_jumps_to_next_canvas() {
        let mut app = build_test_app_with_canvases(&[ContentBlockId(1), ContentBlockId(2), ContentBlockId(3)]);
        app.focus_canvas(ContentBlockId(1), None);
        let event = crossterm::event::KeyEvent {
            code: crossterm::event::KeyCode::Char('j'),
            modifiers: crossterm::event::KeyModifiers::CONTROL,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        handle_key_event(&mut app, event).await;
        assert!(app.is_canvas_focused(ContentBlockId(2)));
    }
```

- [ ] **Step 2: Run; verify it fails**

```bash
cargo test -p savvagent ctrl_j_jumps_to_next_canvas
```

Expected: FAIL.

- [ ] **Step 3: Extend `handle_key_event`**

Add helper:

```rust
fn canvas_ids_in_order(app: &App) -> Vec<ContentBlockId> {
    app.entries
        .iter()
        .filter_map(|e| match e {
            Entry::Canvas { id, .. } => Some(ContentBlockId(*id)),
            _ => None,
        })
        .collect()
}

async fn jump_canvas(app: &mut App, delta: i32) {
    let canvases = canvas_ids_in_order(app);
    if canvases.is_empty() {
        return;
    }
    let current_idx = match app.focus {
        AppFocus::Canvas { id, .. } => canvases.iter().position(|x| *x == id),
        _ => None,
    };
    let target_idx = match current_idx {
        None => if delta > 0 { 0 } else { canvases.len() - 1 },
        Some(i) => {
            let n = (i as i32 + delta).rem_euclid(canvases.len() as i32);
            n as usize
        }
    };
    app.focus_canvas(canvases[target_idx], None);
}
```

In `handle_key_event`'s built-in match (after Tab/Shift-Tab):

```rust
        KeyCode::Char('j') if event.modifiers.contains(KeyModifiers::CONTROL) => {
            jump_canvas(app, 1).await;
            return;
        }
        KeyCode::Char('k') if event.modifiers.contains(KeyModifiers::CONTROL) => {
            jump_canvas(app, -1).await;
            return;
        }
```

Also wire Ctrl-J from `ChatInput` focus (in the non-canvas branch):

```rust
async fn handle_non_canvas_key(app: &mut App, event: crossterm::event::KeyEvent) {
    if event.code == KeyCode::Char('j')
        && event.modifiers.contains(KeyModifiers::CONTROL)
    {
        jump_canvas(app, 1).await;
        return;
    }
    if event.code == KeyCode::Char('k')
        && event.modifiers.contains(KeyModifiers::CONTROL)
    {
        jump_canvas(app, -1).await;
        return;
    }
    // ... rest of existing non-canvas key handling
}
```

- [ ] **Step 4: Run; verify it passes**

```bash
cargo test -p savvagent ctrl_j_jumps_to_next_canvas
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/tui.rs
git commit -m "feat(tui): Ctrl-J/Ctrl-K canvas traversal"
```

---

## Task 24: Keyboard routing — Ctrl-O open in browser

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/html_canvas/open_in_browser.rs`
- Modify: `crates/savvagent/src/plugin/builtin/html_canvas/mod.rs`
- Modify: `crates/savvagent/src/tui.rs`

Ctrl-O on a focused canvas writes the canvas's source to a temp file and shells out to `xdg-open` / `open` / `start`. The temp file path includes the canvas id and a timestamp so reopening the same canvas doesn't blow away a previous one mid-read.

- [ ] **Step 1: Create the helper module**

Create `crates/savvagent/src/plugin/builtin/html_canvas/open_in_browser.rs`:

```rust
//! Ctrl-O implementation: write canvas to a temp file and shell out
//! to the platform's "open" command.

use std::path::PathBuf;

/// Write `source` to a fresh temp file and return the path.
pub fn write_temp_html(id: u32, source: &str) -> std::io::Result<PathBuf> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let path = std::env::temp_dir().join(format!("savvagent-canvas-{id}-{now}.html"));
    std::fs::write(&path, source)?;
    Ok(path)
}

/// Shell out to the platform's URL opener.
pub async fn shell_open(path: &std::path::Path) -> std::io::Result<()> {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "start"
    } else {
        "xdg-open"
    };
    let output = tokio::process::Command::new(cmd)
        .arg(path)
        .output()
        .await?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "{cmd} {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_temp_html_creates_readable_file() {
        let path = write_temp_html(42, "<!doctype html><body>hi</body>").unwrap();
        let read = std::fs::read_to_string(&path).unwrap();
        assert!(read.contains("hi"));
        let _ = std::fs::remove_file(&path);
    }
}
```

- [ ] **Step 2: Wire the module**

In `crates/savvagent/src/plugin/builtin/html_canvas/mod.rs`, add:

```rust
pub mod open_in_browser;
```

- [ ] **Step 3: Write the failing test in tui.rs**

```rust
    #[tokio::test]
    async fn ctrl_o_writes_temp_file_and_shells_open() {
        // We can't easily mock shell_open in the test; instead, factor
        // the Ctrl-O handler so the "write temp + return path" part is
        // testable without the shell step.
        let mut app = build_test_app_with_canvas(ContentBlockId(1));
        app.focus_canvas(ContentBlockId(1), None);
        let temp_path = handle_ctrl_o(&mut app).await.expect("Ctrl-O ok");
        assert!(temp_path.exists(), "temp file should exist after Ctrl-O");
        let _ = std::fs::remove_file(&temp_path);
    }
```

`handle_ctrl_o` is a new helper that returns the temp path; the actual shell-open call is invoked separately for testability.

- [ ] **Step 4: Run; verify it fails**

```bash
cargo test -p savvagent ctrl_o_writes_temp_file_and_shells_open
```

Expected: FAIL.

- [ ] **Step 5: Implement Ctrl-O**

In `tui.rs`:

```rust
async fn handle_ctrl_o(app: &mut App) -> std::io::Result<std::path::PathBuf> {
    let AppFocus::Canvas { id, .. } = app.focus else {
        return Err(std::io::Error::other("no canvas focused"));
    };
    let source = app
        .entries
        .iter()
        .find_map(|e| match e {
            Entry::Canvas { id: x, source, .. } if ContentBlockId(*x) == id => Some(source.clone()),
            _ => None,
        })
        .ok_or_else(|| std::io::Error::other("canvas not found in entries"))?;
    let path = crate::plugin::builtin::html_canvas::open_in_browser::write_temp_html(
        id.0, &source,
    )?;
    // Best-effort shell out; do not error the TUI if it fails.
    if let Err(e) = crate::plugin::builtin::html_canvas::open_in_browser::shell_open(&path).await {
        tracing::warn!(error = ?e, "shell_open failed");
    }
    Ok(path)
}
```

In the key-routing match:

```rust
        KeyCode::Char('o') if event.modifiers.contains(KeyModifiers::CONTROL) => {
            let _ = handle_ctrl_o(app).await;
            return;
        }
```

- [ ] **Step 6: Run; verify it passes**

```bash
cargo test -p savvagent ctrl_o_writes_temp_file_and_shells_open
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/html_canvas/open_in_browser.rs crates/savvagent/src/plugin/builtin/html_canvas/mod.rs crates/savvagent/src/tui.rs
git commit -m "feat(tui): Ctrl-O writes canvas to temp file + shells open"
```

---

## Task 25: `KeyScope::OnFocusedCanvas` precedence enforcement

**Files:**
- Modify: `crates/savvagent/src/tui.rs`
- Modify: `crates/savvagent/src/plugin/registry.rs` (or wherever scope-resolution lives)

The spec says: built-in canvas keys (Tab, Shift-Tab, Esc, Ctrl-J, Ctrl-K, Ctrl-O) take precedence over plugin-registered `OnFocusedCanvas` bindings. The host runs the built-in matcher first; only on a miss does it look at plugin bindings.

- [ ] **Step 1: Write the failing test**

```rust
    #[tokio::test]
    async fn plugin_binding_does_not_steal_built_in_canvas_keys() {
        let mut app = build_test_app_with_canvas(ContentBlockId(1));
        // Register a plugin keybinding for Tab on OnFocusedCanvas scope.
        app.register_test_keybinding(
            KeyScope::OnFocusedCanvas,
            "tab",
            // Some action — must NOT fire.
            TestAction::SetFlag,
        );
        app.focus_canvas(ContentBlockId(1), Some(0));
        let event = key_event_tab();
        handle_key_event(&mut app, event).await;
        // Built-in handled it: focus moves to next element. Flag not set.
        assert!(matches!(app.focus, AppFocus::Canvas { element_idx: Some(1), .. }));
        assert!(!app.test_flag, "plugin handler must not have fired");
    }

    #[tokio::test]
    async fn plugin_binding_handles_unbound_built_in_key() {
        let mut app = build_test_app_with_canvas(ContentBlockId(2));
        // Register a plugin keybinding for Ctrl-Y (not built-in).
        app.register_test_keybinding(
            KeyScope::OnFocusedCanvas,
            "ctrl+y",
            TestAction::SetFlag,
        );
        app.focus_canvas(ContentBlockId(2), None);
        let event = key_event_ctrl_y();
        handle_key_event(&mut app, event).await;
        assert!(app.test_flag, "plugin handler should fire for unbound key");
    }
```

- [ ] **Step 2: Run; verify they fail**

```bash
cargo test -p savvagent plugin_binding
```

Expected: FAIL — either the test helpers don't exist or the precedence isn't enforced.

- [ ] **Step 3: Implement precedence**

In `handle_key_event`, after the built-in match block but before passing through to renderer, consult plugin bindings:

```rust
    // ... existing built-in match arms with early `return`s for handled keys ...

    // Built-in didn't claim the key. Try plugin keybindings in
    // KeyScope::OnFocusedCanvas.
    if let Some(action) = app
        .keybindings
        .lookup(KeyScope::OnFocusedCanvas, &portable_key)
    {
        // Dispatch the action via the existing effect-applier.
        app.apply_keybinding_action(action).await;
        return;
    }

    // Still unhandled: dispatch to renderer as InputEvent::Key.
    // ... existing renderer dispatch code ...
```

`app.keybindings` is the existing keybinding registry built from plugin contributions; `lookup` is its existing API.

- [ ] **Step 4: Run; verify they pass**

```bash
cargo test -p savvagent plugin_binding
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/tui.rs
git commit -m "feat(tui): canvas key precedence over plugin OnFocusedCanvas bindings"
```

---

## Task 26: Transcript snapshot triggers

**Files:**
- Modify: `crates/savvagent-host/src/session.rs` (or wherever transcript saving lives)
- Modify: `crates/savvagent/src/app.rs` (snapshot collection)

Snapshots fire at: every `TurnComplete`, before manual `/save`, at clean TUI shutdown. The snapshot iterates all canvases in the registry; for each non-streaming one, calls `snapshot_state()` and base64-encodes the result into the matching `Entry::Canvas.state` field before writing the transcript JSON.

- [ ] **Step 1: Find the existing transcript-save path**

```bash
grep -rn "transcript.*write\|save_transcript\|to_json\|TurnComplete" crates/savvagent crates/savvagent-host | head -20
```

- [ ] **Step 2: Write the failing test**

```rust
    #[tokio::test]
    async fn transcript_save_includes_canvas_state() {
        let mut app = build_test_app_with_canvas(ContentBlockId(1));
        // Toggle a <details> inside the canvas (via dispatch path or direct).
        // Then save the transcript and verify the JSON includes a non-empty
        // `state` field on the canvas Entry.
        let path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
        toggle_details_in_test_canvas(&mut app, ContentBlockId(1));
        save_transcript(&app, &path).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let entries = json["entries"].as_array().unwrap();
        let canvas = entries.iter().find(|e| e["type"] == "canvas").unwrap();
        assert!(canvas["state"].is_string(), "state should be a base64 string");
    }
```

- [ ] **Step 3: Run; verify it fails**

```bash
cargo test -p savvagent transcript_save_includes_canvas_state
```

Expected: FAIL — `save_transcript` doesn't yet call `snapshot_state`.

- [ ] **Step 4: Implement snapshot collection during save**

In the transcript-save path (location from Step 1), before serializing entries:

```rust
// For each canvas entry, refresh its `state` field from the live renderer.
for entry in &mut app.entries {
    if let Entry::Canvas { id, source: _, state } = entry {
        if let Some(renderer) = app.canvas_registry.get(ContentBlockId(*id)) {
            // Skip canvases still streaming (source_preview).
            // The check belongs in HtmlCanvas::snapshot_state per
            // Task 7's spec; this is defensive.
            if let Some(bytes) = renderer.snapshot_state() {
                use base64::Engine as _;
                *state = Some(base64::engine::general_purpose::STANDARD.encode(bytes));
            } else {
                *state = None;
            }
        }
    }
}
// ... existing serde_json::to_writer ...
```

Add `base64 = { workspace = true }` to `crates/savvagent/Cargo.toml` if not already present.

- [ ] **Step 5: Wire the snapshot to fire on `TurnComplete`**

Find the `TurnComplete` event handler in `tui.rs` or `app.rs`; if auto-save already runs, the snapshot collection happens inside it. If not, run snapshot collection just before auto-save.

- [ ] **Step 6: Wire on clean shutdown**

In the TUI shutdown path (search `tui.rs` for `Event::Quit` or similar), call save before exiting.

- [ ] **Step 7: Run; verify it passes**

```bash
cargo test -p savvagent transcript_save_includes_canvas_state
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent crates/savvagent-host
git commit -m "feat(host): snapshot canvas state on save / TurnComplete / shutdown"
```

---

## Task 27: Transcript restore on `/resume`

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/html_canvas/mod.rs`

When the TUI loads a transcript via `/resume`, each `Entry::Canvas { source, state }` triggers `HtmlCanvas::new(source)`; if `state` is `Some`, base64-decode and pass to `renderer.restore_state(&bytes)`. On error, log and continue (renderer falls back to defaults).

- [ ] **Step 1: Find the existing `/resume` path**

```bash
grep -rn "resume\|load_transcript\|create_renderer.*Canvas" crates/savvagent | head -20
```

- [ ] **Step 2: Write the failing test**

```rust
    #[tokio::test]
    async fn resume_restores_canvas_state() {
        // Build a transcript with a Canvas Entry containing a state blob
        // that says "details is open". Resume it. Assert the canvas's
        // live state reflects the restored open-details.
        let state_blob = sample_state_with_open_details();
        let base64_state = base64::engine::general_purpose::STANDARD.encode(&state_blob);
        let entries = vec![Entry::Canvas {
            id: 1,
            source: "<!doctype html><body><details><summary>s</summary><p>y</p></details></body>".into(),
            state: Some(base64_state),
        }];
        let app = resume_with_entries(entries).await;
        let renderer = app.canvas_registry.get(ContentBlockId(1)).expect("renderer present");
        // Take a snapshot of the live state; verify open_details is non-empty.
        let live = renderer.snapshot_state().expect("non-empty after restore");
        let live_state = savvagent_canvas::CanvasState::from_bytes(&live).unwrap();
        assert!(!live_state.open_details.is_empty());
    }
```

- [ ] **Step 3: Run; verify it fails**

```bash
cargo test -p savvagent resume_restores_canvas_state
```

Expected: FAIL.

- [ ] **Step 4: Implement state restoration**

In the `/resume` canvas-construction path, after `HtmlCanvas::new(source)` and the initial `render` (needed because restore needs a live document):

```rust
let mut renderer = HtmlCanvas::new(ContentBlockId(id), &source);
// Initial render to populate the document; restore needs it.
renderer.render(PixelSize { width: 800, height: 0 });
if let Some(state_b64) = state.as_deref() {
    use base64::Engine as _;
    match base64::engine::general_purpose::STANDARD.decode(state_b64) {
        Ok(bytes) => {
            if let Err(e) = renderer.restore_state(&bytes) {
                tracing::warn!(
                    canvas_id = id,
                    error = ?e,
                    "canvas state restore failed; continuing with defaults"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                canvas_id = id,
                error = ?e,
                "canvas state base64 decode failed; continuing with defaults"
            );
        }
    }
}
```

- [ ] **Step 5: Run; verify it passes**

```bash
cargo test -p savvagent resume_restores_canvas_state
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/html_canvas/mod.rs
git commit -m "feat(tui): /resume restores canvas interactive state"
```

---

## Task 28: CHANGELOG, README, release(0.17.0) rollup, tag push

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `README.md`
- Modify: `docs/canvas-terminal-compat.md` (note keyboard nav additions if relevant)

This is the release-plumbing task. Phase 1 already bumped the workspace version to `0.17.0`; Phase 2 consolidates the CHANGELOG entry, refreshes README user-facing prose, and pushes the `v0.17.0` git tag (cargo-dist handles the rest).

- [ ] **Step 1: Update CHANGELOG**

In `CHANGELOG.md`, replace the existing `## [0.17.0] - unreleased` block with the combined Phase 1 + Phase 2 entry. Keep Phase 1's existing bullets verbatim; append Phase 2 sections:

```markdown
## [0.17.0] - 2026-MM-DD

> Inline HTML canvas — Phase 1 (static rendering + export) and Phase 2
> (interaction, state persistence, tool-emitted HTML) ship together
> in this release.

### Added

[... keep all existing Phase 1 Added bullets ...]

#### Phase 2 (interaction + persistence + tool HTML)

- `ContentRenderer::dispatch` is now implemented on `HtmlCanvas` with
  a renderer-side default-action interceptor for links, `<details>`,
  and form submit. Blitz alpha.4 accepts synthetic events but does not
  run browser default actions; the interceptor closes the gap.
- `ContentRenderer::freeze` / `thaw` soft-freeze a canvas when focus
  leaves it (DOM state retained; event dispatch paused).
- `ContentRenderer::focusable_elements` / `focused_index` / `set_focus`
  expose focus state to the host for Tab traversal + focus chrome.
- `ContentRenderer::snapshot_state` / `restore_state` (new trait
  methods) persist form values, expanded `<details>` set, scroll
  offsets, and the focused element across `/save`+`/resume`.
- TUI: `AppFocus::Canvas { id, element_idx }` variant. Mouse click in
  a canvas focuses it; Tab/Shift-Tab cycles focusable elements within;
  Esc unfocuses; Ctrl-J/Ctrl-K traverse canvases in the transcript;
  Ctrl-O writes the canvas to a temp file and shells out to the
  system's URL opener; built-in canvas keys take precedence over
  plugin `KeyScope::OnFocusedCanvas` bindings.
- 1-cell focus chrome rendered around the focused canvas.
- Tool-emitted HTML: MCP tool results with content items of type
  `html` translate into `ContentBlock::Html` blocks. Host assigns
  `ContentBlockId`; same renderer pipeline as model-emitted HTML.
- Transcript schema bump to v3: optional `state` field on `Canvas`
  Entries (base64-encoded opaque blob). Snapshots taken on
  `TurnComplete`, before manual `/save`, and at clean shutdown.
- `PluginError::StateRestoreFailed(String)` variant for soft
  state-restore failures.
- `KeyScope::OnFocusedCanvas` keybinding scope.
- `Entry::Unknown` serde fallback: future Entry variants degrade to a
  one-line placeholder instead of erroring the whole transcript load.

### Changed

- URL classification on link follow: absolute URLs (`http://`,
  `https://`, `mailto:`, `tel:`, `sms:`) → SystemBrowser. Bare paths /
  relative URLs → ContinueConversation. `data:`, `file://`, `javascript:`
  → no effect (logged at debug / warn).

### Known Issues

- Windows CI continues to skip `savvagent-canvas` and `savvagent` test
  binaries due to a Blitz / DirectWrite font-discovery hang on the
  GitHub-hosted `windows-latest` runner image. Local Windows dev runs
  exercise these crates normally. Tracked separately.
```

- [ ] **Step 2: Update README**

In `README.md`, find the existing HTML canvas blurb (added in Phase 1) and append a sentence:

```markdown
Mouse-click to focus a canvas; Tab/Shift-Tab walks focusable elements
within; Esc unfocuses; Ctrl-J/Ctrl-K traverse canvases in the
transcript; Ctrl-O opens the canvas in your system browser. Form
values, expanded `<details>`, and scroll positions survive
`/save`+`/resume`.
```

- [ ] **Step 3: Run the full test suite**

```bash
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 4: Run CI-equivalent format + clippy**

```bash
rustup run stable cargo fmt --all -- --check
RUSTFLAGS="-D warnings" rustup run stable cargo clippy --workspace --all-targets
```

Expected: PASS.

- [ ] **Step 5: Commit the release scaffolding**

```bash
git add CHANGELOG.md README.md
git commit -m "release(0.17.0): inline HTML canvas (Phase 1 + Phase 2)"
```

- [ ] **Step 6: Push the v0.17.0 tag**

Per `feedback_cargo_dist_release`, do NOT run `gh release create` manually — cargo-dist's Release workflow handles the artifact build on tag push.

```bash
git tag v0.17.0
git push origin v0.17.0
```

This triggers cargo-dist's Release workflow. Watch it via `gh run watch --workflow Release` or via the GitHub Actions UI. On success, the GitHub Release for v0.17.0 will appear with binaries attached.

- [ ] **Step 7: Verify the release published**

```bash
gh release view v0.17.0 --json name,tagName,assets 2>&1
```

Expected: JSON output showing the release name, tag `v0.17.0`, and binary assets for each platform target.

- [ ] **Step 8: Final check — the canvas Phase 2 PR**

Open the PR's merge state:

```bash
gh pr view 97 --json mergeStateStatus,mergeable
```

Expected: `MERGEABLE` / `CLEAN`. Coordinate the merge per project convention. After merge, this entire Phase 2 plan + the Phase 1 plan are complete; the inline-html-canvas initiative is fully shipped.

---

## Acceptance check (run before final release commit)

These map to the Phase 2 spec amendment items.

- [ ] `cargo test --workspace` is green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is green.
- [ ] `cargo fmt --all -- --check` is green.
- [ ] `PluginError::StateRestoreFailed` variant exists with `Display` (Task 2).
- [ ] `ContentRenderer::snapshot_state` / `restore_state` exist with defaults (Task 3).
- [ ] `KeyScope::OnFocusedCanvas` variant exists and round-trips through serde (Task 4).
- [ ] `HtmlCanvas::dispatch` emits `Effect::OpenUrl` for link clicks; emits no effects when frozen (Task 13 tests).
- [ ] `HtmlCanvas::dispatch` toggles `<details>` open state on summary click and marks DOM dirty (Task 11 tests).
- [ ] `HtmlCanvas::dispatch` emits `Effect::OpenUrl` with form-encoded query string on form submit (Task 12 tests).
- [ ] URL classification table is honored for `https:`, `mailto:`, `data:`, `javascript:`, bare paths (Task 10 tests).
- [ ] `HtmlCanvas::snapshot_state` returns `None` for default state and a non-empty blob after `<details>` toggle (Task 15 tests).
- [ ] `HtmlCanvas::restore_state` round-trips state from a snapshot, returns `StateRestoreFailed` on garbage (Task 16 tests).
- [ ] `ToolRegistry::call` translates MCP `html` content items into `ContentBlock::Html` blocks (Task 17 tests).
- [ ] `Entry::Canvas` carries an optional base64 `state` field; `Entry::Unknown` absorbs future variants (Task 18 tests).
- [ ] `AppFocus::Canvas { id, element_idx }` exists; `focus_canvas` calls `freeze`/`thaw` correctly (Task 19 tests).
- [ ] Focused canvas renders a 1-cell border (Task 20 test).
- [ ] Mouse click in canvas region focuses the canvas + dispatches event (Task 21 test).
- [ ] Tab cycles focusable elements; Esc unfocuses (Task 22 tests).
- [ ] Ctrl-J/Ctrl-K traverse canvases (Task 23 test).
- [ ] Ctrl-O writes a temp file and shells open (Task 24 test).
- [ ] Built-in canvas keys take precedence over plugin `OnFocusedCanvas` bindings (Task 25 tests).
- [ ] Transcript save populates the `state` field from `snapshot_state()` (Task 26 test).
- [ ] `/resume` restores state via `restore_state` (Task 27 test).
- [ ] CHANGELOG entry for v0.17.0 covers both phases.
- [ ] README reflects the new interaction story.
- [ ] `v0.17.0` git tag is pushed; cargo-dist's Release workflow ran green.
- [ ] No commits push to remote outside of the v0.17.0 tag push (and ongoing PR updates to PR #97).
