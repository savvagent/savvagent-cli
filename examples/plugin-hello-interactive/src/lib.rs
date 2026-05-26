//! `plugin-hello-interactive` — a screen-only external plugin.
//!
//! Exports the `plugin-interactive` world and owns one screen, `hello.screen`,
//! that renders `"Hello, world!"` and converts an Enter key press into a
//! toast.
//!
//! ## How interactive plugins are opened
//!
//! Interactive plugins **cannot register slash commands** — slash commands
//! live in the `plugin-static` world. To open this screen from the TUI you
//! need a second (static-world) plugin that emits
//! `Effect::OpenScreen { id: "hello.screen", args: ... }` from one of its
//! slash handlers (or any other effect-producing entry point). The
//! built-in `internal:plugins` manager is one example; user-installed
//! static plugins can do the same.
//!
//! ## Build
//!
//! ```bash
//! cargo component build --release --target wasm32-unknown-unknown
//! ```
//!
//! ## What this example demonstrates
//!
//! - The two-layer interactive-world export shape: a top-level `Guest`
//!   (with `manifest`) plus the `screens` interface (with `create_screen`)
//!   and a `screen-instance` resource.
//! - Per-open state: `HelloScreen` holds a mutable greeted-count using
//!   `std::cell::Cell` because the WIT resource methods take `&self`.
//! - Returning a `StyledLine` of `StyledSpan`s from `render` and `tips`.

#[allow(warnings)]
mod bindings;

use std::cell::Cell;

use bindings::Guest;
use bindings::exports::savvagent::plugin::screens::{
    Guest as ScreensGuest, GuestScreenInstance, ScreenArgs, ScreenInstance,
};
use bindings::savvagent::plugin::types as t;

struct Component;

impl Guest for Component {
    fn manifest() -> Result<t::PluginManifest, t::PluginError> {
        Ok(t::PluginManifest {
            id: "savvagent.hello-interactive".to_string(),
            name: "Hello (Interactive)".to_string(),
            version: "0.1.0".to_string(),
            description:
                "Minimal interactive-world example. Owns the hello.screen screen.".to_string(),
            kind: t::PluginKind::Optional,
            contributions: t::Contributions {
                slash_commands: vec![],
                hooks: vec![],
                screens: vec!["hello.screen".to_string()],
                render_slots: vec![],
                keybindings: vec![],
                themes: false,
            },
        })
    }
}

impl ScreensGuest for Component {
    type ScreenInstance = HelloScreen;

    /// The host calls this whenever something emits
    /// `Effect::OpenScreen { id, args }` and routes the open to us. We only
    /// own one screen; anything else is an error so the host can surface a
    /// useful diagnostic.
    fn create_screen(
        screen_id: String,
        _args: ScreenArgs,
    ) -> Result<ScreenInstance, t::PluginError> {
        if screen_id != "hello.screen" {
            return Err(t::PluginError::ScreenNotFound(screen_id));
        }
        Ok(ScreenInstance::new(HelloScreen::new()))
    }
}

/// Per-open screen state. The WIT resource methods (`on_key`, `render`,
/// `tips`, `on_event`) all take `&self`, so any mutable state lives behind
/// a `Cell`.
pub struct HelloScreen {
    greeted: Cell<u32>,
}

impl HelloScreen {
    fn new() -> Self {
        Self {
            greeted: Cell::new(0),
        }
    }
}

impl GuestScreenInstance for HelloScreen {
    /// Pressing Enter bumps the local counter and pushes a toast back to
    /// the host. Any other key is ignored.
    fn on_key(&self, key: t::KeyEventPortable) -> Result<Vec<t::Effect>, t::PluginError> {
        if matches!(key.code, t::KeyCode::Enter) {
            self.greeted.set(self.greeted.get() + 1);
            return Ok(vec![t::Effect::PushNote(t::Note {
                text: "you pressed Enter".to_string(),
                level: t::NoteLevel::Info,
            })]);
        }
        Ok(Vec::new())
    }

    fn on_event(&self, _event_json: String) -> Result<Vec<t::Effect>, t::PluginError> {
        Ok(Vec::new())
    }

    /// One styled line containing one span — the host's renderer turns that
    /// into a `ratatui::text::Line`.
    fn render(&self, _area: t::Region) -> Vec<t::StyledLine> {
        vec![styled_line("Hello, world!")]
    }

    /// Footer/tips line surfaced by the host's screen chrome.
    fn tips(&self) -> Vec<t::StyledLine> {
        vec![styled_line("press Enter to greet")]
    }
}

/// Tiny helper — most real plugins will want to vary fg/bg/mods per span,
/// but for a "hello world" the defaults are fine.
fn styled_line(text: &str) -> t::StyledLine {
    t::StyledLine {
        spans: vec![t::StyledSpan {
            text: text.to_string(),
            fg: t::ThemeColor::Reset,
            bg: t::ThemeColor::Reset,
            mods: t::TextMods {
                bold: false,
                italic: false,
                underline: false,
                reverse: false,
                dim: false,
            },
        }],
    }
}

bindings::export!(Component with_types_in bindings);
