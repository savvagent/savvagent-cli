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
// `events::dispatch_raw` is consumed by `HtmlCanvas::dispatch` in Task 13;
// until then the public-in-module fns are unreferenced outside tests.
#[allow(dead_code)]
mod events;
mod focus;
// `interceptor::intercept` is consumed by `HtmlCanvas::dispatch` in Task 13;
// the dead-code allow lives on `intercept` itself. `classify_url` and its
// tests exercise the pure path.
mod interceptor;
// `CanvasState` is `pub use`'d below because the host/TUI needs it for
// transcript persistence in later tasks (13/15/16/18+).
mod state;
mod subset;

/// Cell ↔ pixel coordinate translation helpers.
pub mod coords;
pub use canvas::HtmlCanvas;
pub use coords::{CellPixelSize, CellRect, cell_to_pixel, contains_cell};
pub use state::CanvasState;
