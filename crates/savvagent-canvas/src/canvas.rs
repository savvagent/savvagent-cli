//! `HtmlCanvas` — the static-rendering implementation of
//! `ContentRenderer` for SPP `ContentBlock::Html`.

use std::fmt;

use async_trait::async_trait;
use savvagent_plugin::{
    ContentBlockId, ContentRenderer, FocusableElement, Frame, PixelFormat, PixelSize,
};

use crate::focus;

/// Static HTML canvas renderer. Phase 1: render-only; Phase 2 adds
/// event dispatch + focus + freeze/thaw.
pub struct HtmlCanvas {
    id: ContentBlockId,
    source: String,
    /// Cached focusable-element list from the most recent render.
    /// `None` before the first render.
    focusable_cache: Option<Vec<(u32, FocusableElement)>>,
    /// Index into `focusable_cache` that's currently focused.
    focused: Option<u32>,
    /// Phase 2: frozen flag. Set/cleared by `freeze`/`thaw`; soft-freeze
    /// just pauses event dispatch (no re-layout, no re-paint).
    frozen: bool,
    /// Interactive-state log. Replayed onto each freshly-parsed document
    /// (render + dispatch); re-derived after each mutating dispatch.
    canvas_state: crate::state::CanvasState,
    /// Width (px) of the most recent render. dispatch must parse + resolve
    /// at this width so hit-test pixel coords line up with what's on screen.
    /// `None` before the first render.
    last_render_width: Option<u32>,
    // NOTE on the "retain Blitz document on self" item in the Task 7 plan:
    // `blitz_html::HtmlDocument` contains `dyn HtmlParserProvider` and
    // `dyn FontMetricsProvider` trait objects that are neither `Send` nor
    // `Sync`. `ContentRenderer: Send` therefore makes it impossible to
    // store the document as a field of `HtmlCanvas` without `unsafe impl
    // Send`, which is forbidden by `#![forbid(unsafe_code)]` at the crate
    // root. We keep the parse-on-every-render model from Phase 1 and
    // snapshot only the post-render data we actually need (the focusable
    // cache below). The downstream tasks that depend on retained DOM
    // state (Task 8 freeze/thaw, Task 13 dispatch, Task 16 restore) will
    // need to either (a) wrap document access in a Send-safe shim that
    // pins it to a dedicated thread, or (b) reconstruct state from a
    // serializable snapshot rather than retaining the document. That
    // design decision is deferred to those tasks.
}

impl fmt::Debug for HtmlCanvas {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HtmlCanvas")
            .field("id", &self.id)
            .field("source_len", &self.source.len())
            .field(
                "focusable_cache_len",
                &self.focusable_cache.as_ref().map(Vec::len),
            )
            .field("focused", &self.focused)
            .field("frozen", &self.frozen)
            .field("last_render_width", &self.last_render_width)
            .finish()
    }
}

impl HtmlCanvas {
    /// Construct a canvas from HTML source.
    pub fn new(id: ContentBlockId, source: &str) -> Self {
        crate::subset::validate(source);
        Self {
            id,
            source: source.to_string(),
            focusable_cache: None,
            focused: None,
            frozen: false,
            canvas_state: crate::state::CanvasState::default(),
            last_render_width: None,
        }
    }

    /// Return the HTML source this canvas was constructed from.
    pub fn source(&self) -> &str {
        &self.source
    }
}

#[async_trait]
impl ContentRenderer for HtmlCanvas {
    fn id(&self) -> ContentBlockId {
        self.id
    }

    fn render(&mut self, size: PixelSize) -> Frame {
        render_html_to_rgba(self, size.width)
    }

    async fn dispatch(
        &mut self,
        event: savvagent_plugin::InputEvent,
    ) -> Result<savvagent_plugin::InputOutcome, savvagent_plugin::PluginError> {
        use blitz_dom::BaseDocument;

        if self.frozen {
            return Ok(savvagent_plugin::InputOutcome {
                effects: Vec::new(),
                dirty: false,
            });
        }
        // dispatch must hit-test against the same layout the user sees, so it
        // re-parses + resolves at the last render width. Before the first
        // render there is no width and nothing has been painted, so drop.
        let width = match self.last_render_width {
            Some(w) => w.max(1),
            None => {
                return Ok(savvagent_plugin::InputOutcome {
                    effects: Vec::new(),
                    dirty: false,
                });
            }
        };

        // Parse fresh at the last render width, replay current state, resolve.
        let mut document = parse_and_apply(&self.source, width, &self.canvas_state);
        {
            let base: &mut BaseDocument = document.as_mut();
            base.resolve(0.0);
        }

        let base: &mut BaseDocument = document.as_mut();
        let raw = crate::events::dispatch_raw(base, &event);
        let outcome = crate::interceptor::intercept_mut(base, raw.target_node);
        if outcome.dirty {
            base.resolve(0.0);
        }
        // Re-derive state so this event's mutation persists to the next parse.
        // Preserve host-managed `focused` (collect_state never sets it).
        let focused = self.canvas_state.focused.take();
        self.canvas_state = collect_state(base);
        self.canvas_state.focused = focused;

        Ok(savvagent_plugin::InputOutcome {
            effects: outcome.effect.into_iter().collect(),
            dirty: raw.dirty || outcome.dirty,
        })
    }

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

    fn freeze(&mut self) {
        self.frozen = true;
    }

    fn thaw(&mut self) {
        self.frozen = false;
    }

    fn snapshot_state(&self) -> Option<Vec<u8>> {
        // Start from the live state log (open_details + form_values, kept
        // current by dispatch's collect_state).
        let mut state = self.canvas_state.clone();
        state.schema_version = 1;
        // Fold in the currently-focused element: translate the focusable
        // index (self.focused) to its NodeId string.
        state.focused = self.focused.and_then(|idx| {
            self.focusable_cache
                .as_ref()
                .and_then(|cache| cache.get(idx as usize))
                .map(|(node_id, _)| node_id.to_string())
        });
        if state.is_empty() {
            None
        } else {
            Some(state.to_bytes())
        }
    }

    fn restore_state(&mut self, bytes: &[u8]) -> Result<(), savvagent_plugin::PluginError> {
        let state = crate::state::CanvasState::from_bytes(bytes)
            .map_err(savvagent_plugin::PluginError::StateRestoreFailed)?;
        // Sync the focus index if the focusable cache is already populated
        // (best-effort; a subsequent render rebuilds the cache anyway).
        if let Some(focused_id) = state.focused.as_ref().and_then(|s| s.parse::<u32>().ok())
            && let Some(cache) = self.focusable_cache.as_ref()
        {
            self.focused = cache
                .iter()
                .position(|(id, _)| *id == focused_id)
                .map(|i| i as u32);
        }
        self.canvas_state = state;
        Ok(())
    }
}

/// Parse `source` into a fresh, not-yet-resolved `HtmlDocument` at the
/// given measure viewport width, then replay `state`'s semantic
/// mutations onto it via [`apply_state`]. The caller must
/// `base.resolve(0.0)` afterwards.
///
/// Shared by `render` (measure pass) and `dispatch` so both see an
/// identical replayed document; the `!Send` document never escapes the
/// caller's stack frame.
fn parse_and_apply(
    source: &str,
    width: u32,
    state: &crate::state::CanvasState,
) -> blitz_html::HtmlDocument {
    use blitz_dom::{BaseDocument, DocumentConfig, StyleThreading};
    use blitz_html::HtmlDocument;
    use blitz_traits::shell::{ColorScheme, Viewport};

    // Generous measure-pass height; render replaces it with the natural
    // height before the final paint, and dispatch only needs a viewport big
    // enough that hit-testable content isn't clipped.
    let measure_height: u32 = 100_000;
    let mut document = HtmlDocument::from_html(
        source,
        DocumentConfig {
            base_url: None,
            net_provider: None,
            // Sequential: Blitz's default Parallel threading panics with
            // `already mutably borrowed` when two HtmlCanvas instances resolve
            // concurrently against Stylo's global thread pool (blitz #430).
            style_threading: StyleThreading::Sequential,
            viewport: Some(Viewport::new(
                width,
                measure_height,
                1.0,
                ColorScheme::Light,
            )),
            ..Default::default()
        },
    );
    {
        let base: &mut BaseDocument = document.as_mut();
        apply_state(base, state);
    }
    document
}

/// Replay a [`CanvasState`](crate::state::CanvasState)'s semantic
/// mutations onto a freshly-parsed, not-yet-resolved document: sets the
/// `open` attribute on the `<details>` nodes in `open_details` and the
/// `value` attribute on the form fields in `form_values`. Call BEFORE
/// `base.resolve(0.0)`.
///
/// NodeId keys are stringified `usize` slab keys (e.g. `"42"`); the
/// Task 1 spike proved they're stable across parses of identical source,
/// so a key collected from one parse is valid on the next. Keys that no
/// longer resolve, or resolve to the wrong element kind, are skipped.
fn apply_state(base: &mut blitz_dom::BaseDocument, state: &crate::state::CanvasState) {
    use blitz_dom::qual_name;

    // Collect the concrete (id, attr-value) edits up front under immutable
    // borrows, then apply them through a single mutator scope so the
    // immutable borrows are released before the mutable one is taken.
    let mut details_to_open: Vec<usize> = Vec::new();
    for key in &state.open_details {
        let Ok(id) = key.parse::<usize>() else {
            continue;
        };
        let is_details = base
            .get_node(id)
            .and_then(|n| n.data.downcast_element())
            .map(|e| *e.name.local == *"details")
            .unwrap_or(false);
        if is_details {
            details_to_open.push(id);
        }
    }

    let mut field_values: Vec<(usize, String)> = Vec::new();
    for (key, val) in &state.form_values {
        let Ok(id) = key.parse::<usize>() else {
            continue;
        };
        let is_field = base
            .get_node(id)
            .and_then(|n| n.data.downcast_element())
            .map(|e| {
                let local = &e.name.local;
                *local == *"input" || *local == *"select" || *local == *"textarea"
            })
            .unwrap_or(false);
        if is_field {
            field_values.push((id, val.clone()));
        }
    }

    if details_to_open.is_empty() && field_values.is_empty() {
        return;
    }

    let mut mutator = base.mutate();
    for id in details_to_open {
        // `<details open>` is a boolean attribute; empty value is canonical.
        mutator.set_attribute(id, qual_name!("open"), "");
    }
    for (id, val) in field_values {
        mutator.set_attribute(id, qual_name!("value"), &val);
    }
    // Drop flushes the mutator's pending mutations; must happen before the
    // caller's `resolve`.
    drop(mutator);
}

/// Re-derive a [`CanvasState`](crate::state::CanvasState) by walking a
/// document (resolved or not): collects open `<details>` ids and named
/// form-field values. Does NOT set `focused` (host-managed via
/// `set_focus`; the dispatch path folds the prior `focused` back in).
fn collect_state(base: &blitz_dom::BaseDocument) -> crate::state::CanvasState {
    use blitz_dom::{BaseDocument, local_name};

    fn walk(base: &BaseDocument, id: usize, state: &mut crate::state::CanvasState) {
        let Some(node) = base.get_node(id) else {
            return;
        };
        if let Some(e) = node.data.downcast_element() {
            let local = &e.name.local;
            let is_field = *local == *"input" || *local == *"select" || *local == *"textarea";
            if *local == *"details" && e.attr(local_name!("open")).is_some() {
                state.open_details.insert(format!("{id}"));
            } else if is_field
                && let Some(name) = e.attr(local_name!("name"))
                && !name.is_empty()
            {
                let value = e.attr(local_name!("value")).unwrap_or("").to_string();
                state.form_values.insert(format!("{id}"), value);
            }
        }
        for c in node.children.iter().copied() {
            walk(base, c, state);
        }
    }

    let mut state = crate::state::CanvasState {
        schema_version: 1,
        ..crate::state::CanvasState::default()
    };
    walk(base, base.root_element().id, &mut state);
    state
}

/// Headless Blitz pipeline: parse `canvas.source` → resolve at the
/// requested width → measure natural height → repaint at exact natural
/// height → return an Rgba8 [`Frame`]. After the paint, refresh
/// `canvas.focusable_cache` from the just-resolved layout so
/// `focusable_elements()` reflects what's on screen.
///
/// Refactor choice (Task 7): we *don't* retain the `HtmlDocument` on
/// `self`. See the comment on `HtmlCanvas` — Blitz's document is
/// `!Send`, and the `ContentRenderer: Send` bound prevents storing it
/// in a `Send` renderer without `unsafe impl`. The focusable cache
/// (which is `Send`) is the only post-render artefact we keep.
///
/// The implementation follows the Phase 0 spike notes
/// (`docs/superpowers/notes/2026-05-21-blitz-spike.md` §"Static
/// rendering" / §"Pixel-buffer access" / §"Natural height").
fn render_html_to_rgba(canvas: &mut HtmlCanvas, width: u32) -> Frame {
    use anyrender::{ImageRenderer as _, PaintScene as _};
    use anyrender_vello_cpu::VelloCpuImageRenderer;
    use blitz_dom::BaseDocument;
    use blitz_paint::paint_scene;
    use blitz_traits::shell::{ColorScheme, Viewport};
    use peniko::{
        Color, Fill,
        kurbo::{Affine, Rect},
    };

    // `size.height` is ignored by design: we always return the natural height
    // for the requested width per the trait's contract (PixelSize::height is
    // a hint; Frame::height is authoritative).

    // Width must be > 0 — guard against accidental 0 by clamping to 1px.
    // (The trait contract says `size.width > 0`; we never want a panic.)
    let width = width.max(1);
    // Record the width dispatch must re-parse at so hit-test pixel coords
    // line up with the frame we're about to paint.
    canvas.last_render_width = Some(width);
    let scale: f32 = 1.0;

    // ---- Measure pass: parse + replay state + resolve at requested width to
    // get natural height. `parse_and_apply` replays `canvas_state` (details
    // toggles, form values) onto the fresh document before we resolve, so the
    // painted frame reflects prior interactions.
    let mut document = parse_and_apply(&canvas.source, width, &canvas.canvas_state);
    {
        let base: &mut BaseDocument = document.as_mut();
        base.resolve(0.0);
    }

    let natural_height: u32 = {
        let base: &BaseDocument = document.as_ref();
        let root = base.root_element();
        // Root element's `final_layout.size.height` is f32 pixels.
        // Round up so we don't clip the bottom row of content.
        root.final_layout.size.height.ceil().max(1.0) as u32
    };

    // ---- Final paint at the natural height. Set the viewport to the
    // measured dimensions and re-resolve so layout matches the paint.
    {
        let base: &mut BaseDocument = document.as_mut();
        base.set_viewport(Viewport::new(
            width,
            natural_height,
            scale,
            ColorScheme::Light,
        ));
        base.resolve(0.0);
    }

    // VelloCpuImageRenderer truncates to u16 internally; warn + clamp so
    // pathological canvases fail visibly rather than producing a buffer
    // whose size doesn't match the renderer's expectations.
    const MAX_DIM: u32 = u16::MAX as u32;
    let width = if width > MAX_DIM {
        tracing::warn!(width, max = MAX_DIM, "canvas width truncated to u16 max");
        MAX_DIM
    } else {
        width
    };
    let natural_height = if natural_height > MAX_DIM {
        tracing::warn!(
            natural_height,
            max = MAX_DIM,
            "canvas height truncated to u16 max"
        );
        MAX_DIM
    } else {
        natural_height
    };

    let buffer = {
        let base: &mut BaseDocument = document.as_mut();
        // VelloCpuImageRenderer renders a single frame to a Vec<u8>.
        // The renderer hands us a scene; we paint a white background
        // first (so transparent or unstyled regions don't end up with
        // garbage from an uninitialized buffer in some backends) and
        // then delegate the document paint to `blitz_paint::paint_scene`.
        let mut renderer = VelloCpuImageRenderer::new(width, natural_height);
        let mut out: Vec<u8> = Vec::new();
        renderer.render_to_vec(
            |scene| {
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::WHITE,
                    None,
                    &Rect::new(0.0, 0.0, width as f64, natural_height as f64),
                );
                paint_scene(scene, base, scale as f64, width, natural_height, 0, 0);
            },
            &mut out,
        );
        out
    };

    // Refresh the focusable-element cache from the just-resolved layout so
    // `focusable_elements()` reflects what's on screen. We extract this
    // before `document` drops; the cache is `Send` even though the
    // document itself isn't.
    {
        let base: &BaseDocument = document.as_ref();
        canvas.focusable_cache = Some(focus::collect(base));
    }
    // Clamp `focused` to the new cache length so a removed element doesn't
    // leave a stale out-of-range index behind across re-renders.
    if let Some(i) = canvas.focused {
        let len = canvas
            .focusable_cache
            .as_ref()
            .map(|v| v.len() as u32)
            .unwrap_or(0);
        if i >= len {
            canvas.focused = None;
        }
    }
    // Re-sync the focus index from the state log. If `restore_state` ran
    // before any render, `self.focused` is still None even though the
    // restored NodeId survives in `canvas_state.focused`. Now that the cache
    // is freshly built, translate that NodeId back into an index so focus
    // restoration is robust regardless of restore/render ordering.
    if canvas.focused.is_none()
        && let Some(node_id) = canvas
            .canvas_state
            .focused
            .as_ref()
            .and_then(|s| s.parse::<u32>().ok())
        && let Some(cache) = canvas.focusable_cache.as_ref()
    {
        canvas.focused = cache
            .iter()
            .position(|(id, _)| *id == node_id)
            .map(|i| i as u32);
    }

    // Sanity-check the buffer length matches the trait contract.
    // anyrender_vello_cpu produces RGBA8 row-major top-down, which is
    // exactly what `PixelFormat::Rgba8` is defined to be.
    debug_assert_eq!(
        buffer.len() as u32,
        width * natural_height * 4,
        "Blitz produced unexpected pixel buffer size: {} bytes, expected {}",
        buffer.len(),
        width * natural_height * 4,
    );

    Frame {
        width,
        height: natural_height,
        format: PixelFormat::Rgba8,
        bytes: buffer,
    }
}

#[cfg(test)]
impl HtmlCanvas {
    fn is_frozen(&self) -> bool {
        self.frozen
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
            height: 0, // 0 means "natural height"
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

    #[test]
    fn focusable_elements_returns_walk_results() {
        let mut c = HtmlCanvas::new(
            ContentBlockId(1),
            "<!doctype html><body><a href='x'>link</a><button>b</button></body>",
        );
        c.render(PixelSize {
            width: 200,
            height: 0,
        });
        let elements = c.focusable_elements();
        assert_eq!(elements.len(), 2, "got {elements:#?}");
        assert_ne!(elements[0].id, elements[1].id);
    }

    #[test]
    fn set_focus_updates_focused_index() {
        let mut c = HtmlCanvas::new(
            ContentBlockId(2),
            "<!doctype html><body><a href='x'>l1</a><a href='y'>l2</a></body>",
        );
        c.render(PixelSize {
            width: 200,
            height: 0,
        });
        assert_eq!(c.focused_index(), None);
        c.set_focus(Some(1));
        assert_eq!(c.focused_index(), Some(1));
        c.set_focus(None);
        assert_eq!(c.focused_index(), None);
    }

    #[test]
    fn set_focus_out_of_range_clears() {
        let mut c = HtmlCanvas::new(
            ContentBlockId(3),
            "<!doctype html><body><a href='x'>l1</a></body>",
        );
        c.render(PixelSize {
            width: 200,
            height: 0,
        });
        c.set_focus(Some(99));
        assert_eq!(
            c.focused_index(),
            None,
            "out-of-range set_focus should clear"
        );
    }

    #[test]
    fn freeze_and_thaw_flip_internal_flag() {
        let mut c = HtmlCanvas::new(
            ContentBlockId(3),
            "<!doctype html><body><a href='x'>l</a></body>",
        );
        c.render(PixelSize {
            width: 100,
            height: 0,
        });
        c.freeze();
        assert!(c.is_frozen());
        c.thaw();
        assert!(!c.is_frozen());
    }

    #[tokio::test]
    async fn dispatch_link_click_returns_open_url_effect() {
        use savvagent_plugin::{
            InputEvent, KeyMods, MouseButton, MouseEventKind, MouseEventPortable,
        };
        let mut c = HtmlCanvas::new(
            ContentBlockId(10),
            "<!doctype html><body><a href='https://example.com' style='display:block;width:100px;height:50px'>x</a></body>",
        );
        c.render(PixelSize {
            width: 200,
            height: 0,
        });
        let ev = InputEvent::Mouse(MouseEventPortable {
            kind: MouseEventKind::Press,
            button: Some(MouseButton::Left),
            x_pixel: 16,
            y_pixel: 24,
            modifiers: KeyMods::default(),
        });
        let outcome = c.dispatch(ev).await.expect("dispatch ok");
        assert_eq!(outcome.effects.len(), 1, "expected one effect");
        let savvagent_plugin::Effect::OpenUrl { url, target } =
            outcome.effects.into_iter().next().unwrap()
        else {
            panic!("expected OpenUrl");
        };
        assert_eq!(url, "https://example.com");
        assert_eq!(target, savvagent_plugin::UrlTarget::SystemBrowser);
    }

    #[tokio::test]
    async fn dispatch_drops_events_when_frozen() {
        use savvagent_plugin::{
            InputEvent, KeyMods, MouseButton, MouseEventKind, MouseEventPortable,
        };
        let mut c = HtmlCanvas::new(
            ContentBlockId(11),
            "<!doctype html><body><a href='x'>x</a></body>",
        );
        c.render(PixelSize {
            width: 200,
            height: 0,
        });
        c.freeze();
        let ev = InputEvent::Mouse(MouseEventPortable {
            kind: MouseEventKind::Press,
            button: Some(MouseButton::Left),
            x_pixel: 16,
            y_pixel: 24,
            modifiers: KeyMods::default(),
        });
        let outcome = c.dispatch(ev).await.expect("dispatch ok");
        assert!(
            outcome.effects.is_empty(),
            "frozen canvas must drop effects"
        );
        assert!(!outcome.dirty);
    }

    /// End-to-end proof of the amended re-parse + state-log replay model:
    /// a `<details>` toggle from one dispatch must survive into the next
    /// freshly-parsed document. We click the summary twice; both clicks
    /// report `dirty`, which is only possible if the first toggle's `open`
    /// attribute was re-derived into `canvas_state` and replayed before the
    /// second dispatch (a fresh parse starts closed, so without the replay
    /// the second click would just re-open and produce identical results —
    /// but more importantly the toggle would be lost). We additionally
    /// assert the state log itself records the open `<details>` after the
    /// first click and is empty again after the second.
    #[tokio::test]
    async fn details_toggle_persists_across_dispatch_via_state_log() {
        use savvagent_plugin::{
            InputEvent, KeyMods, MouseButton, MouseEventKind, MouseEventPortable,
        };
        let mut c = HtmlCanvas::new(
            ContentBlockId(12),
            "<!doctype html><body><details><summary style='display:block;width:80px;height:20px'>s</summary><p>body</p></details></body>",
        );
        c.render(PixelSize {
            width: 200,
            height: 0,
        });

        // Click the summary at its laid-out center.
        let summary = c
            .focusable_elements()
            .into_iter()
            .next()
            .expect("summary focusable");
        let cx = (summary.bounds.x + summary.bounds.width / 2).max(1);
        let cy = (summary.bounds.y + summary.bounds.height / 2).max(1);
        let click = |cx: u32, cy: u32| {
            InputEvent::Mouse(MouseEventPortable {
                kind: MouseEventKind::Press,
                button: Some(MouseButton::Left),
                x_pixel: cx,
                y_pixel: cy,
                modifiers: KeyMods::default(),
            })
        };

        let outcome = c.dispatch(click(cx, cy)).await.expect("dispatch ok");
        assert!(outcome.dirty, "summary toggle should be dirty");
        assert_eq!(
            c.canvas_state.open_details.len(),
            1,
            "first click should record the details as open in the state log"
        );

        // A SECOND identical click must toggle it back to closed. This only
        // works if the first toggle persisted: the second dispatch re-parses
        // from source (closed), replays the open state, then the click flips
        // it shut again — proving the replay round-trip.
        let outcome2 = c.dispatch(click(cx, cy)).await.expect("dispatch ok");
        assert!(outcome2.dirty, "second toggle should also be dirty");
        assert!(
            c.canvas_state.open_details.is_empty(),
            "second click should toggle the details closed again"
        );
    }

    #[test]
    fn snapshot_empty_canvas_returns_none() {
        let mut c = HtmlCanvas::new(
            ContentBlockId(20),
            "<!doctype html><body><p>plain</p></body>",
        );
        c.render(PixelSize {
            width: 200,
            height: 0,
        });
        assert!(c.snapshot_state().is_none(), "no stateful elements → None");
    }

    #[tokio::test]
    async fn snapshot_captures_open_details_after_toggle() {
        use savvagent_plugin::{
            InputEvent, KeyMods, MouseButton, MouseEventKind, MouseEventPortable,
        };
        let mut c = HtmlCanvas::new(
            ContentBlockId(21),
            "<!doctype html><body><details><summary style='display:block;width:80px;height:20px'>s</summary><p>y</p></details></body>",
        );
        c.render(PixelSize {
            width: 200,
            height: 0,
        });
        let summary = c
            .focusable_elements()
            .into_iter()
            .next()
            .expect("summary focusable");
        let ev = InputEvent::Mouse(MouseEventPortable {
            kind: MouseEventKind::Press,
            button: Some(MouseButton::Left),
            x_pixel: summary.bounds.x + summary.bounds.width / 2,
            y_pixel: summary.bounds.y + summary.bounds.height / 2,
            modifiers: KeyMods::default(),
        });
        c.dispatch(ev).await.expect("dispatch ok");
        let snap = c.snapshot_state().expect("non-empty after toggle");
        let state = crate::state::CanvasState::from_bytes(&snap).unwrap();
        assert!(
            !state.open_details.is_empty(),
            "open details should be captured"
        );
    }

    #[test]
    fn snapshot_includes_focused_nodeid() {
        let mut c = HtmlCanvas::new(
            ContentBlockId(22),
            "<!doctype html><body><a href='x'>l1</a><a href='y'>l2</a></body>",
        );
        c.render(PixelSize {
            width: 200,
            height: 0,
        });
        c.set_focus(Some(1));
        let snap = c.snapshot_state().expect("focus makes state non-empty");
        let state = crate::state::CanvasState::from_bytes(&snap).unwrap();
        assert!(state.focused.is_some(), "focused NodeId should be recorded");
        // The focused id should be the NodeId of the 2nd focusable (index 1).
        // (We can't assert the exact number, but it must be present + parseable.)
        assert!(state.focused.as_ref().unwrap().parse::<u32>().is_ok());
    }

    #[tokio::test]
    async fn restore_state_round_trips_open_details() {
        use savvagent_plugin::{
            InputEvent, KeyMods, MouseButton, MouseEventKind, MouseEventPortable,
        };
        // Build canvas A, toggle a <details> open, snapshot it.
        let mut a = HtmlCanvas::new(
            ContentBlockId(30),
            "<!doctype html><body><details><summary style='display:block;width:80px;height:20px'>s</summary><p>y</p></details></body>",
        );
        a.render(PixelSize {
            width: 200,
            height: 0,
        });
        let summary = a.focusable_elements().into_iter().next().expect("summary");
        let ev = InputEvent::Mouse(MouseEventPortable {
            kind: MouseEventKind::Press,
            button: Some(MouseButton::Left),
            x_pixel: summary.bounds.x + summary.bounds.width / 2,
            y_pixel: summary.bounds.y + summary.bounds.height / 2,
            modifiers: KeyMods::default(),
        });
        a.dispatch(ev).await.expect("dispatch ok");
        let snap = a.snapshot_state().expect("non-empty after toggle");

        // Fresh canvas B with the SAME source; restore the snapshot.
        let mut b = HtmlCanvas::new(
            ContentBlockId(31),
            "<!doctype html><body><details><summary style='display:block;width:80px;height:20px'>s</summary><p>y</p></details></body>",
        );
        b.render(PixelSize {
            width: 200,
            height: 0,
        });
        b.restore_state(&snap).expect("restore ok");
        // Snapshot B; its open_details should now match A's.
        let snap_b = b.snapshot_state().expect("non-empty after restore");
        let state_b = crate::state::CanvasState::from_bytes(&snap_b).unwrap();
        assert!(!state_b.open_details.is_empty(), "restored open details");
    }

    #[test]
    fn restore_state_returns_error_on_garbage() {
        let mut c = HtmlCanvas::new(ContentBlockId(32), "<!doctype html><body></body>");
        c.render(PixelSize {
            width: 100,
            height: 0,
        });
        let err = c.restore_state(b"not json").unwrap_err();
        assert!(
            matches!(err, savvagent_plugin::PluginError::StateRestoreFailed(_)),
            "expected StateRestoreFailed, got {err:?}",
        );
    }

    #[tokio::test]
    async fn form_value_round_trips_through_snapshot_restore() {
        // A named input with a value attribute. `snapshot_state` reads
        // `self.canvas_state`, which is only populated by `dispatch`'s
        // `collect_state` — a render alone never folds form values in. So a
        // snapshot taken right after render is None (nothing changed from
        // default). To meaningfully exercise the form_values round-trip we
        // construct a CanvasState with form_values directly, serialize it,
        // restore it into a canvas, and assert the value survives a
        // snapshot → restore → snapshot cycle.
        let source = "<!doctype html><body><form><input type='text' name='title' value='hello'></form></body>";

        // Confirm the documented behavior: a fresh render captures no form
        // values (collect_state only runs on dispatch).
        let mut a = HtmlCanvas::new(ContentBlockId(40), source);
        a.render(PixelSize {
            width: 200,
            height: 0,
        });
        assert!(
            a.snapshot_state().is_none(),
            "render alone must not capture form values (collect_state runs only on dispatch)"
        );

        // Build a CanvasState carrying a form value keyed by a NodeId and
        // round-trip it through restore → snapshot. The NodeId need not
        // resolve in the document for the state log to survive a round-trip;
        // `apply_state`/`collect_state` only touch the rendered document.
        let mut seed = crate::state::CanvasState {
            schema_version: 1,
            ..crate::state::CanvasState::default()
        };
        seed.form_values.insert("title".into(), "hello".into());
        let bytes = seed.to_bytes();

        let mut b = HtmlCanvas::new(ContentBlockId(41), source);
        b.render(PixelSize {
            width: 200,
            height: 0,
        });
        b.restore_state(&bytes).expect("restore ok");
        let snap_b = b.snapshot_state().expect("non-empty after restore");
        let state_b = crate::state::CanvasState::from_bytes(&snap_b).unwrap();
        assert_eq!(
            state_b.form_values, seed.form_values,
            "form values must round-trip through restore → snapshot"
        );
    }

    #[test]
    fn focus_index_restores_when_restore_precedes_render() {
        // restore_state called BEFORE any render (empty cache), then render
        // must re-sync self.focused from canvas_state.focused.
        let source = "<!doctype html><body><a href='x'>l1</a><a href='y'>l2</a></body>";
        // Build a snapshot that focuses the 2nd link.
        let mut a = HtmlCanvas::new(ContentBlockId(42), source);
        a.render(PixelSize {
            width: 200,
            height: 0,
        });
        a.set_focus(Some(1));
        let snap = a.snapshot_state().expect("focused → non-empty");

        // Fresh canvas: restore BEFORE render (cache empty at restore time).
        let mut b = HtmlCanvas::new(ContentBlockId(43), source);
        b.restore_state(&snap).expect("restore ok");
        // Cache is empty now, so self.focused may be None here.
        b.render(PixelSize {
            width: 200,
            height: 0,
        });
        // After render rebuilds the cache, focus must be re-synced to index 1.
        assert_eq!(
            b.focused_index(),
            Some(1),
            "focus index must restore after render"
        );
    }
}
