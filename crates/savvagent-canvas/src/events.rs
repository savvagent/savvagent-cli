//! Synthetic event dispatch into Blitz. Translates portable
//! `InputEvent` values into Blitz's UI/DOM event shape and routes
//! them through `Document::handle_ui_event` (which internally performs
//! hit-testing, the pointer-down/up sequence, and Blitz's own default
//! actions via a `NoopEventHandler`).
//!
//! Default actions that Blitz does NOT run in headless alpha.4 (link
//! follow, `<details>` toggle, form submit — see the Phase 0 spike
//! notes, `docs/superpowers/notes/2026-05-21-blitz-spike.md`) are NOT
//! done here — they live in `interceptor` (Tasks 10-12) and are called
//! from `HtmlCanvas::dispatch` AFTER this raw dispatch.
//!
//! ## Why `Document::handle_ui_event` (not `handle_dom_event` directly)
//!
//! `BaseDocument` implements the `blitz_dom::Document` trait, whose
//! `handle_ui_event(UiEvent)` builds an `EventDriver` over a
//! `NoopEventHandler` and runs the full synchronous pipeline:
//! hover/active/focus state update, hit-test (`set_hover_to`), the
//! pointer → mouse → click event chain, and Blitz's built-in default
//! actions. It takes a `&mut BaseDocument` and returns `()`
//! synchronously — no threads, no async, no blocking I/O — so it
//! cannot hang the caller. `handle_dom_event` is the lower-level
//! primitive that skips the hit-test/state plumbing; we don't want
//! that here.

#![warn(missing_docs)]

use blitz_dom::{BaseDocument, Document};
use blitz_traits::events::{
    BlitzPointerEvent, BlitzPointerId, MouseEventButton, MouseEventButtons, PointerCoords,
    PointerDetails, UiEvent,
};
use savvagent_plugin::{InputEvent, MouseButton, MouseEventKind, MouseEventPortable};

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
        InputEvent::Key(_) => RawDispatch {
            target_node: None,
            dirty: false,
        },
        InputEvent::Focus(_) => RawDispatch {
            target_node: None,
            dirty: false,
        },
    }
}

fn dispatch_mouse(base: &mut BaseDocument, m: &MouseEventPortable) -> RawDispatch {
    let x = m.x_pixel as f32;
    let y = m.y_pixel as f32;

    // Hit-test against the resolved layout. `BaseDocument::hit(x, y)`
    // returns `Option<HitResult>` whose `node_id: usize` is the slab
    // key of the element under the coordinate. We surface it as the
    // `target_node` regardless of event kind so the interceptor
    // (Tasks 10-12) can decide what default action to run.
    let target = base.hit(x, y).map(|h| h.node_id as u32);

    match m.kind {
        MouseEventKind::Press => {
            let ev = pointer_event(x, y, m.button, true);
            // `Document::handle_ui_event` does the hit-test + state
            // update + event-chain dispatch synchronously. See module docs.
            base.handle_ui_event(UiEvent::PointerDown(ev));
        }
        MouseEventKind::Release => {
            let ev = pointer_event(x, y, m.button, false);
            base.handle_ui_event(UiEvent::PointerUp(ev));
        }
        MouseEventKind::Move => {
            let ev = pointer_event(x, y, None, false);
            base.handle_ui_event(UiEvent::PointerMove(ev));
        }
        // Phase 2's subset has no overflow/scroll containers, so scrolls
        // are a no-op against Blitz. We still report the hit-test target
        // so higher layers can reason about cursor position.
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            return RawDispatch {
                target_node: target,
                dirty: false,
            };
        }
    }

    // `dirty` is intentionally `false` for raw Blitz mouse dispatch.
    // Per the Phase 0 spike (§"Synthetic event dispatch"), headless
    // alpha.4 does NOT run browser-level default actions (link follow,
    // `<details>` toggle, form submit), so a raw click on the Phase 2
    // subset leaves the DOM unchanged. All DOM mutation — and therefore
    // all dirtiness — is produced by the `interceptor` layer (Tasks
    // 10-12), which runs AFTER this and reports its own dirtiness up
    // through `HtmlCanvas::dispatch`. Blitz's `handle_ui_event` returns
    // `()` and exposes no clean per-call "is dirty" signal, so there is
    // nothing to read here even if it did mutate.
    RawDispatch {
        target_node: target,
        dirty: false,
    }
}

/// Build a Blitz `BlitzPointerEvent` at viewport coords `(x, y)`.
///
/// `pressed` selects the `buttons` bitmask (which buttons are currently
/// held): set for `PointerDown`, clear for `PointerUp`/`PointerMove`.
/// `button` is the button that triggered the event (irrelevant for
/// moves → defaults to `Main`).
///
/// Modifier translation from the portable `KeyMods` is deferred: Blitz's
/// `mods` field is a `keyboard_types::Modifiers`, a type this crate does
/// not depend on directly, and modifiers are not load-bearing for the
/// Phase 2 raw-dispatch path (the interceptor reads the portable
/// `KeyMods`, not Blitz's copy). We pass `Default::default()` (no
/// modifiers); revisit if a future task needs modifier-aware default
/// actions inside Blitz.
fn pointer_event(x: f32, y: f32, button: Option<MouseButton>, pressed: bool) -> BlitzPointerEvent {
    let button = map_button(button);
    let buttons = if pressed {
        MouseEventButtons::from(button)
    } else {
        MouseEventButtons::None
    };
    BlitzPointerEvent {
        id: BlitzPointerId::Mouse,
        is_primary: true,
        coords: PointerCoords {
            page_x: x,
            page_y: y,
            screen_x: x,
            screen_y: y,
            client_x: x,
            client_y: y,
        },
        button,
        buttons,
        mods: Default::default(),
        details: PointerDetails::default(),
    }
}

fn map_button(button: Option<MouseButton>) -> MouseEventButton {
    match button {
        Some(MouseButton::Left) | None => MouseEventButton::Main,
        Some(MouseButton::Middle) => MouseEventButton::Auxiliary,
        Some(MouseButton::Right) => MouseEventButton::Secondary,
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

    /// Locate the `<a>` node and return the center of its laid-out box
    /// so the click reliably lands on it regardless of font metrics.
    fn link_center(base: &BaseDocument) -> (u32, u32) {
        // The sample doc's only focusable element is the link; take its
        // laid-out box center so the click lands regardless of font metrics.
        match crate::focus::collect(base).into_iter().next() {
            Some((_, fe)) => {
                let cx = fe.bounds.x + fe.bounds.width / 2;
                let cy = fe.bounds.y + fe.bounds.height / 2;
                (cx.max(1), cy.max(1))
            }
            None => (16, 24),
        }
    }

    #[test]
    fn mouse_press_on_link_targets_link_node() {
        let mut d = doc();
        {
            let base: &mut BaseDocument = d.as_mut();
            base.resolve(0.0);
        }
        let base: &mut BaseDocument = d.as_mut();
        let (x, y) = link_center(base);
        let ev = InputEvent::Mouse(MouseEventPortable {
            kind: MouseEventKind::Press,
            button: Some(MouseButton::Left),
            x_pixel: x,
            y_pixel: y,
            modifiers: KeyMods::default(),
        });
        let out = dispatch_raw(base, &ev);
        assert!(
            out.target_node.is_some(),
            "expected hit-test to find a node at ({x},{y})"
        );
    }

    #[test]
    fn mouse_release_and_move_do_not_panic() {
        let mut d = doc();
        {
            let base: &mut BaseDocument = d.as_mut();
            base.resolve(0.0);
        }
        let base: &mut BaseDocument = d.as_mut();
        let (x, y) = link_center(base);
        for kind in [MouseEventKind::Move, MouseEventKind::Release] {
            let ev = InputEvent::Mouse(MouseEventPortable {
                kind,
                button: Some(MouseButton::Left),
                x_pixel: x,
                y_pixel: y,
                modifiers: KeyMods::default(),
            });
            let out = dispatch_raw(base, &ev);
            assert!(!out.dirty, "raw mouse dispatch must not report dirty");
        }
    }

    #[test]
    fn scroll_is_noop_but_reports_target() {
        let mut d = doc();
        {
            let base: &mut BaseDocument = d.as_mut();
            base.resolve(0.0);
        }
        let base: &mut BaseDocument = d.as_mut();
        let (x, y) = link_center(base);
        let ev = InputEvent::Mouse(MouseEventPortable {
            kind: MouseEventKind::ScrollDown,
            button: None,
            x_pixel: x,
            y_pixel: y,
            modifiers: KeyMods::default(),
        });
        let out = dispatch_raw(base, &ev);
        assert!(!out.dirty);
        assert!(out.target_node.is_some());
    }

    #[test]
    fn key_and_focus_events_are_noop() {
        use savvagent_plugin::FocusKind;
        let mut d = doc();
        {
            let base: &mut BaseDocument = d.as_mut();
            base.resolve(0.0);
        }
        let base: &mut BaseDocument = d.as_mut();
        let out = dispatch_raw(base, &InputEvent::Focus(FocusKind::Gained));
        assert_eq!(out.target_node, None);
        assert!(!out.dirty);
    }

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

    /// Depth-first search for the first element whose local tag name
    /// matches `tag`, returning its node id. Mirrors `focus.rs`'s walk:
    /// Blitz node ids and `node.children` entries are `usize` slab keys;
    /// we cast to `u32` only at the boundary the interceptor expects.
    fn find_node_by_tag(base: &BaseDocument, tag: &str) -> Option<u32> {
        fn walk(base: &BaseDocument, id: usize, tag: &str) -> Option<usize> {
            let node = base.get_node(id)?;
            if let Some(e) = node.data.downcast_element()
                && *e.name.local == *tag
            {
                return Some(id);
            }
            for c in node.children.iter().copied() {
                if let Some(found) = walk(base, c, tag) {
                    return Some(found);
                }
            }
            None
        }
        walk(base, base.root_element().id, tag).map(|id| id as u32)
    }
}
