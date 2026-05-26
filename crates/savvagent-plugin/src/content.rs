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
///
/// Not `Eq`: `Effect` is only `PartialEq` (it transitively includes an
/// `f64`-bearing in-process tool handler), so `InputOutcome` mirrors
/// that bound.
#[derive(Debug, Clone, PartialEq)]
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
    async fn dispatch(&mut self, _event: InputEvent) -> Result<InputOutcome, PluginError> {
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
}
