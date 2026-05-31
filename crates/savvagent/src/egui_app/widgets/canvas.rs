//! Inline-canvas painting + GPU texture cache for the egui front-end.
//!
//! Renderer ownership stays on `App::canvas_registry` (shared with the TUI).
//! This module owns only the GUI-side texture handles. See
//! `docs/superpowers/specs/2026-05-28-v0.19.0-egui-canvas-design.md`.

use std::collections::{HashMap, HashSet};

use savvagent_plugin::{ContentBlockId, Frame, PixelFormat, PixelSize};

use crate::app::App;
use crate::palette::Palette;

/// One cached texture for an `Entry::Canvas`. `size` records the pixel
/// dimensions the texture was built at so a width change invalidates the
/// cache without re-querying the handle.
pub(super) struct GuiTexEntry {
    pub(super) size: PixelSize,
    pub(super) handle: egui::TextureHandle,
}

/// Texture cache keyed by `ContentBlockId`. Entries are dropped when their
/// width no longer matches the desired width, when dispatch reports
/// `dirty=true`, or when `clear()` is called.
#[derive(Default)]
pub struct GuiCanvasCache {
    textures: HashMap<ContentBlockId, GuiTexEntry>,
    /// Per-id failure-mode tracking so the conversation Note stream is hit
    /// at most once per canvas per failure mode (missing renderer or bad
    /// frame). The set is pruned on `invalidate` and cleared on `clear`,
    /// so a canvas that recovers can re-notify on a subsequent failure.
    noted_failures: HashSet<ContentBlockId>,
}

impl std::fmt::Debug for GuiCanvasCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuiCanvasCache")
            .field("entries", &self.textures.len())
            .finish()
    }
}

impl GuiCanvasCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every cached texture handle. Call alongside
    /// `App::canvas_registry.clear()` (today only in `App::replay_transcript`).
    #[allow(dead_code)] // Consumed transitively via SavvagentApp::clear_canvas_caches.
    pub fn clear(&mut self) {
        self.textures.clear();
        self.noted_failures.clear();
    }

    /// Drop the cached texture for `id` if present. Also clears any
    /// failure-mode marker for the id — the canvas may recover.
    pub fn invalidate(&mut self, id: ContentBlockId) {
        self.textures.remove(&id);
        self.noted_failures.remove(&id);
    }

    /// Internal: get the current entry for `id` whose width matches.
    pub(super) fn get_if_fits(&self, id: ContentBlockId, width_px: u32) -> Option<&GuiTexEntry> {
        let entry = self.textures.get(&id)?;
        (entry.size.width == width_px).then_some(entry)
    }

    /// Internal: insert (or replace) the entry for `id`.
    pub(super) fn insert(
        &mut self,
        id: ContentBlockId,
        size: PixelSize,
        handle: egui::TextureHandle,
    ) {
        self.textures.insert(id, GuiTexEntry { size, handle });
    }
}

/// Translate a plugin-emitted `Frame` into an `egui::ColorImage`.
/// Accepts both RGBA8 (canonical) and BGRA8 (byte-swapped) frames.
/// Returns `None` for zero-sized frames or when the byte length does not
/// match `width * height * 4`.
pub(super) fn frame_to_color_image(frame: &Frame) -> Option<egui::ColorImage> {
    if frame.width == 0 || frame.height == 0 {
        return None;
    }
    let expected = (frame.width as usize)
        .checked_mul(frame.height as usize)?
        .checked_mul(4)?;
    if frame.bytes.len() != expected {
        return None;
    }
    let mut rgba = frame.bytes.clone();
    if matches!(frame.format, PixelFormat::Bgra8) {
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [frame.width as usize, frame.height as usize],
        &rgba,
    ))
}

/// Paint one `Entry::Canvas` into the current `ui`.
///
/// Renders (or reuses a cached texture for) the canvas, then drains pointer
/// events for the painted rect and forwards them to
/// [`crate::canvas_input::handle_canvas_mouse`]. A `dirty=true` outcome
/// invalidates the texture cache so the next frame re-renders.
///
/// When `screen_open` is `true`, event dispatch (mouse + keyboard +
/// click-to-focus) is suppressed; only the texture render + cache logic
/// runs. The screen overlay owns input while it is up.
#[allow(clippy::too_many_arguments)] // Each arg is a distinct structural dependency.
pub fn paint(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    app: &mut App,
    cache: &mut GuiCanvasCache,
    host_slot: &crate::HostSlot,
    rt: &tokio::runtime::Handle,
    id: ContentBlockId,
    source: &str,
    source_preview: Option<&str>,
    screen_open: bool,
    _palette: &Palette,
) -> bool {
    // Streaming preview: monospace text, no Blitz call.
    if let Some(preview) = source_preview {
        ui.label(egui::RichText::new("Rendering HTML canvas…").weak());
        for line in preview.split('\n') {
            ui.label(egui::RichText::new(line).monospace());
        }
        // Return false: a streaming preview has no renderer in the registry, so it can't receive focus yet.
        return false;
    }
    if source.is_empty() {
        ui.weak("[empty canvas]");
        // No source yet; nothing to paint or to focus.
        return false;
    }

    let ppp = ctx.pixels_per_point();
    let width_pts = ui.available_width().max(1.0);
    let width_px = (width_pts * ppp).floor().max(1.0) as u32;

    // Two paths build a Response; both must surface it for input handling.
    let resp = if let Some(entry) = cache.get_if_fits(id, width_px) {
        let display = egui::vec2(
            entry.size.width as f32 / ppp,
            entry.size.height as f32 / ppp,
        );
        ui.add(
            egui::Image::new(egui::load::SizedTexture::new(entry.handle.id(), display))
                .sense(egui::Sense::click_and_drag()),
        )
    } else {
        let frame = match app.canvas_registry.get_mut(id) {
            Some(r) => r.render(PixelSize {
                width: width_px,
                height: 0,
            }),
            None => {
                tracing::warn!(?id, "no renderer for canvas — skipping paint");
                ui.weak("[canvas renderer missing]");
                if !cache.noted_failures.contains(&id) {
                    app.push_note(format!(
                        "Canvas {} unavailable: renderer not registered",
                        id.0
                    ));
                    cache.noted_failures.insert(id);
                }
                return false;
            }
        };
        let Some(img) = frame_to_color_image(&frame) else {
            let expected_bytes = (frame.width as usize)
                .checked_mul(frame.height as usize)
                .and_then(|n| n.checked_mul(4));
            let actual_bytes = frame.bytes.len();
            tracing::warn!(
                ?id,
                w = frame.width,
                h = frame.height,
                expected_bytes = ?expected_bytes,
                actual_bytes,
                "bad canvas frame"
            );
            ui.weak("[canvas render failed]");
            if !cache.noted_failures.contains(&id) {
                app.push_note(format!(
                    "Canvas {} render failed (bad frame: {}x{}, {} bytes)",
                    id.0,
                    frame.width,
                    frame.height,
                    frame.bytes.len()
                ));
                cache.noted_failures.insert(id);
            }
            return false;
        };
        let handle = ctx.load_texture(
            format!("canvas-{}", id.0),
            img,
            egui::TextureOptions::LINEAR,
        );
        let display = egui::vec2(frame.width as f32 / ppp, frame.height as f32 / ppp);
        let resp = ui.add(
            egui::Image::new(egui::load::SizedTexture::new(handle.id(), display))
                .sense(egui::Sense::click_and_drag()),
        );
        cache.insert(
            id,
            PixelSize {
                width: frame.width,
                height: frame.height,
            },
            handle,
        );
        resp
    };

    // While a screen overlay is up, the texture still paints but the
    // canvas must NOT process clicks, mouse, or keyboard — the screen
    // owns input.
    if screen_open {
        return false;
    }

    // Click-to-focus: any primary-press inside this canvas takes focus.
    // Runs before the mouse-dispatch loop so a click in the same frame
    // freezes/thaws renderers via `focus_canvas` before any wheel/move
    // events are forwarded to the plugin.
    if resp.clicked() && !app.is_canvas_focused(id) {
        app.focus_canvas(id, None);
    }

    let rect = resp.rect;
    let pointer_pos = ctx.input(|i| i.pointer.latest_pos());
    let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
    for ev in events {
        if let Some(mouse) = mouse_event_to_portable(&ev, rect, ppp, pointer_pos) {
            // Enter the tokio runtime so any task spawned during dispatch
            // lands on the right scheduler. Drop the guard before the next
            // iteration so we re-enter freshly each dispatch.
            let _guard = rt.enter();
            let host_slot = host_slot.clone();
            let dirty = crate::egui_app::block_on_ui(crate::canvas_input::handle_canvas_mouse(
                app, &host_slot, id, mouse,
            ));
            if dirty {
                cache.invalidate(id);
            }
        }
    }

    // Keyboard dispatch: only when this canvas holds focus. Keys consumed
    // here must not also drive the home keybindings — the focused-canvas
    // handler owns the precedence ladder (built-ins → plugin → raw).
    // Skip the keyboard branch on the frame the canvas just received focus.
    // Otherwise a Tab keystroke that egui already routed to the prompt's
    // TextEdit earlier in this frame would be re-dispatched to the canvas
    // via `ctx.input(|i| i.events.clone())`.
    if app.is_canvas_focused(id) && !resp.clicked() {
        let element_idx = match app.input_mode {
            crate::app::InputMode::Canvas { element_idx, .. } => element_idx,
            _ => None,
        };
        let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
        // De-duplicate egui's paired Key/Text events so a single printable
        // keystroke doesn't double-dispatch as both `Char` and the text Char.
        let keys = crate::egui_app::screen::portable_keys_from_events(&events);
        for portable in keys {
            let host_slot = host_slot.clone();
            let dirty =
                crate::egui_app::block_on_ui(crate::canvas_input::handle_focused_canvas_key(
                    app,
                    &host_slot,
                    id,
                    element_idx,
                    portable,
                ));
            if dirty {
                cache.invalidate(id);
            }
        }
    }

    // Report to `paint_log` whether this canvas absorbed a click this
    // frame; used to drive global click-outside unfocus.
    resp.clicked()
}

/// Translate an `egui::Event` into a frame-pixel
/// [`savvagent_plugin::MouseEventPortable`] for the painted rect.
///
/// `pointer_pos` is the latest known pointer position this frame. Wheel
/// events have no embedded position, so they require `pointer_pos` to be
/// `Some(p)` with `p` inside `rect` — otherwise the event is broadcast to
/// every canvas (every canvas's `rect.center()` is inside its own rect).
///
/// Returns `None` for events outside the rect, for unsupported button kinds,
/// or for events without a meaningful pointer position.
fn mouse_event_to_portable(
    ev: &egui::Event,
    rect: egui::Rect,
    ppp: f32,
    pointer_pos: Option<egui::Pos2>,
) -> Option<savvagent_plugin::MouseEventPortable> {
    use savvagent_plugin::{KeyMods, MouseButton, MouseEventKind, MouseEventPortable};

    let (kind, button, pos, modifiers) = match ev {
        egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        } => {
            let btn = match button {
                egui::PointerButton::Primary => Some(MouseButton::Left),
                egui::PointerButton::Secondary => Some(MouseButton::Right),
                egui::PointerButton::Middle => Some(MouseButton::Middle),
                _ => None,
            };
            (
                if *pressed {
                    MouseEventKind::Press
                } else {
                    MouseEventKind::Release
                },
                btn,
                *pos,
                modifiers_to_portable(modifiers),
            )
        }
        egui::Event::PointerMoved(pos) => (MouseEventKind::Move, None, *pos, KeyMods::default()),
        egui::Event::MouseWheel {
            delta, modifiers, ..
        } => {
            // Wheel events don't carry a pointer position; require an
            // actual pointer position inside this canvas's rect to claim
            // them. Without this check every canvas would see every wheel
            // event (a `rect.center()` fallback satisfies any rect).
            let pos = pointer_pos?;
            if !rect.contains(pos) {
                return None;
            }
            let kind = if delta.y > 0.0 {
                MouseEventKind::ScrollUp
            } else if delta.y < 0.0 {
                MouseEventKind::ScrollDown
            } else {
                return None;
            };
            (kind, None, pos, modifiers_to_portable(modifiers))
        }
        _ => return None,
    };

    if !rect.contains(pos) {
        return None;
    }
    let x_pixel = ((pos.x - rect.min.x) * ppp).max(0.0) as u32;
    let y_pixel = ((pos.y - rect.min.y) * ppp).max(0.0) as u32;
    Some(MouseEventPortable {
        kind,
        button,
        x_pixel,
        y_pixel,
        modifiers,
    })
}

/// Map `egui::Modifiers` to the plugin-portable [`savvagent_plugin::KeyMods`].
///
/// `egui::Modifiers::command` is `ctrl` on Linux/Windows and `⌘` on macOS,
/// which matches the semantics of `KeyMods::meta` (Super / Windows / Command).
fn modifiers_to_portable(m: &egui::Modifiers) -> savvagent_plugin::KeyMods {
    savvagent_plugin::KeyMods {
        ctrl: m.ctrl,
        shift: m.shift,
        alt: m.alt,
        meta: m.command,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, format: PixelFormat, fill: [u8; 4]) -> Frame {
        let bytes = (0..(w * h)).flat_map(|_| fill).collect();
        Frame {
            width: w,
            height: h,
            format,
            bytes,
        }
    }

    #[test]
    fn rgba_round_trips() {
        // Alpha is 255 so `Color32::from_rgba_unmultiplied` short-circuits
        // to `from_rgb` and skips premultiplication — keeping the test
        // about RGB channel ordering, not the linear-space alpha math.
        let f = frame(2, 1, PixelFormat::Rgba8, [10, 20, 30, 255]);
        let img = frame_to_color_image(&f).unwrap();
        assert_eq!(img.size, [2, 1]);
        assert_eq!(img.pixels[0].r(), 10);
        assert_eq!(img.pixels[0].g(), 20);
        assert_eq!(img.pixels[0].b(), 30);
        assert_eq!(img.pixels[0].a(), 255);
    }

    #[test]
    fn bgra_is_byte_swapped() {
        // Input is BGRA = (B=10, G=20, R=30, A=255); output must be R=30,
        // G=20, B=10, A=255 — first and third channels swapped. Alpha 255
        // keeps premultiplication a no-op so we can assert raw RGB values.
        let f = frame(1, 1, PixelFormat::Bgra8, [10, 20, 30, 255]);
        let img = frame_to_color_image(&f).unwrap();
        assert_eq!(img.pixels[0].r(), 30);
        assert_eq!(img.pixels[0].g(), 20);
        assert_eq!(img.pixels[0].b(), 10);
        assert_eq!(img.pixels[0].a(), 255);
    }

    #[test]
    fn zero_size_returns_none() {
        assert!(frame_to_color_image(&frame(0, 1, PixelFormat::Rgba8, [0; 4])).is_none());
        assert!(frame_to_color_image(&frame(1, 0, PixelFormat::Rgba8, [0; 4])).is_none());
    }

    #[test]
    fn mismatched_byte_length_returns_none() {
        let bad = Frame {
            width: 2,
            height: 2,
            format: PixelFormat::Rgba8,
            bytes: vec![0; 7], // not 2*2*4
        };
        assert!(frame_to_color_image(&bad).is_none());
    }

    #[test]
    fn cache_get_if_fits_matches_width() {
        let mut cache = GuiCanvasCache::new();
        let ctx = egui::Context::default();
        let img = egui::ColorImage::filled([1, 1], egui::Color32::RED);
        let h = ctx.load_texture("t", img, egui::TextureOptions::LINEAR);
        cache.insert(
            ContentBlockId(0),
            PixelSize {
                width: 100,
                height: 50,
            },
            h,
        );
        assert!(cache.get_if_fits(ContentBlockId(0), 100).is_some());
        assert!(cache.get_if_fits(ContentBlockId(0), 200).is_none());
    }

    #[test]
    fn invalidate_drops_only_one_entry() {
        let mut cache = GuiCanvasCache::new();
        let ctx = egui::Context::default();
        let img = || egui::ColorImage::filled([1, 1], egui::Color32::RED);
        cache.insert(
            ContentBlockId(0),
            PixelSize {
                width: 10,
                height: 10,
            },
            ctx.load_texture("a", img(), egui::TextureOptions::LINEAR),
        );
        cache.insert(
            ContentBlockId(1),
            PixelSize {
                width: 10,
                height: 10,
            },
            ctx.load_texture("b", img(), egui::TextureOptions::LINEAR),
        );
        cache.invalidate(ContentBlockId(0));
        assert!(cache.get_if_fits(ContentBlockId(0), 10).is_none());
        assert!(cache.get_if_fits(ContentBlockId(1), 10).is_some());
    }

    #[test]
    fn clear_drops_everything() {
        let mut cache = GuiCanvasCache::new();
        let ctx = egui::Context::default();
        let img = egui::ColorImage::filled([1, 1], egui::Color32::RED);
        cache.insert(
            ContentBlockId(0),
            PixelSize {
                width: 10,
                height: 10,
            },
            ctx.load_texture("a", img, egui::TextureOptions::LINEAR),
        );
        cache.clear();
        assert!(cache.get_if_fits(ContentBlockId(0), 10).is_none());
    }

    #[test]
    fn mouse_translates_pointer_button_inside_rect() {
        use savvagent_plugin::{MouseButton, MouseEventKind};

        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(100.0, 50.0));
        let pos = egui::pos2(30.0, 40.0);
        let ev = egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let m = mouse_event_to_portable(&ev, rect, 2.0, Some(pos)).expect("inside rect");
        assert_eq!(m.kind, MouseEventKind::Press);
        assert_eq!(m.button, Some(MouseButton::Left));
        // (30 - 10) * 2.0 = 40px ; (40 - 20) * 2.0 = 40px.
        assert_eq!(m.x_pixel, 40);
        assert_eq!(m.y_pixel, 40);
    }

    #[test]
    fn mouse_outside_rect_returns_none() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
        let pos = egui::pos2(100.0, 100.0);
        let ev = egui::Event::PointerMoved(pos);
        assert!(mouse_event_to_portable(&ev, rect, 1.0, Some(pos)).is_none());
    }

    #[test]
    fn mouse_pointer_moved_inside_rect_returns_some_move() {
        use savvagent_plugin::MouseEventKind;

        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(100.0, 50.0));
        let pos = egui::pos2(30.0, 40.0);
        let ev = egui::Event::PointerMoved(pos);
        let m = mouse_event_to_portable(&ev, rect, 2.0, Some(pos)).expect("inside rect");
        assert_eq!(m.kind, MouseEventKind::Move);
        assert_eq!(m.button, None);
        // (30 - 10) * 2.0 = 40px ; (40 - 20) * 2.0 = 40px.
        assert_eq!(m.x_pixel, 40);
        assert_eq!(m.y_pixel, 40);
    }

    #[test]
    fn mouse_wheel_up_returns_scroll_up() {
        use savvagent_plugin::MouseEventKind;

        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let pos = egui::pos2(50.0, 50.0);
        let ev = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 1.0),
            modifiers: egui::Modifiers::default(),
        };
        let m = mouse_event_to_portable(&ev, rect, 1.0, Some(pos)).expect("wheel inside");
        assert_eq!(m.kind, MouseEventKind::ScrollUp);
        assert_eq!(m.button, None);
    }

    #[test]
    fn mouse_wheel_down_returns_scroll_down() {
        use savvagent_plugin::MouseEventKind;

        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let pos = egui::pos2(50.0, 50.0);
        let ev = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, -1.0),
            modifiers: egui::Modifiers::default(),
        };
        let m = mouse_event_to_portable(&ev, rect, 1.0, Some(pos)).expect("wheel inside");
        assert_eq!(m.kind, MouseEventKind::ScrollDown);
    }

    #[test]
    fn mouse_wheel_zero_delta_returns_none() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let pos = egui::pos2(50.0, 50.0);
        let ev = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 0.0),
            modifiers: egui::Modifiers::default(),
        };
        assert!(mouse_event_to_portable(&ev, rect, 1.0, Some(pos)).is_none());
    }

    #[test]
    fn mouse_wheel_without_pointer_returns_none() {
        // Regression net for C1: wheel events have no embedded position,
        // so a `None` pointer position must NOT fall back to `rect.center()`
        // (which would silently claim the event for every canvas).
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let ev = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 1.0),
            modifiers: egui::Modifiers::default(),
        };
        assert!(mouse_event_to_portable(&ev, rect, 1.0, None).is_none());
    }

    #[test]
    fn mouse_wheel_pointer_outside_returns_none() {
        // Regression net for C1: a wheel event with a pointer outside this
        // canvas's rect must not claim the event (it belongs to whichever
        // canvas the pointer is over, if any).
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
        let outside = egui::pos2(100.0, 100.0);
        let ev = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 1.0),
            modifiers: egui::Modifiers::default(),
        };
        assert!(mouse_event_to_portable(&ev, rect, 1.0, Some(outside)).is_none());
    }

    #[test]
    fn mouse_unsupported_event_returns_none() {
        // `_ => return None` fallback: an event variant the translator
        // doesn't recognize (here, window-focus) yields None.
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let ev = egui::Event::WindowFocused(true);
        assert!(mouse_event_to_portable(&ev, rect, 1.0, None).is_none());
    }

    #[test]
    fn mouse_extra_button_yields_no_button() {
        // egui exposes Extra1/Extra2 PointerButtons; the translator's
        // `_ => None` arm in the button-match keeps `button: None` but
        // still emits the Press/Release kind. Document that behavior.
        use savvagent_plugin::MouseEventKind;

        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let pos = egui::pos2(50.0, 50.0);
        let ev = egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Extra1,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let m = mouse_event_to_portable(&ev, rect, 1.0, Some(pos)).expect("inside rect");
        assert_eq!(m.kind, MouseEventKind::Press);
        assert_eq!(
            m.button, None,
            "Extra1 maps to button: None (no plugin-portable variant)",
        );
    }
}
