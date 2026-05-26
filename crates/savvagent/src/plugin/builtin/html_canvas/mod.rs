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

pub mod auto_export;
pub mod open_in_browser;
mod plugin;
mod prompt_text;
pub mod slash;

pub use plugin::HtmlCanvasPlugin;
