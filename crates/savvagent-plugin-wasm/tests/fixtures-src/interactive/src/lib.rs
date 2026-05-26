//! `fixture-interactive` — minimum-viable wasm component for the
//! `savvagent-plugin-wasm` interactive-world adapter tests.
//!
//! Exports a `plugin-interactive` world that:
//! - declares a single `"test"` screen in the manifest;
//! - `screens.create-screen("test", _)` returns a `screen-instance`
//!   resource holding a mutable `key_count: u32`;
//! - `render(area)` returns one styled-line with `"hello"` plus, if
//!   `key_count > 0`, a second line `"keys=<count>"` so the
//!   "key event mutates state" test can observe the change;
//! - `tips()` returns one styled-line containing `"press any key"`;
//! - `on_key(_)` increments `key_count` and returns one
//!   `Effect::PushNote { text: "keyed", level: Info }`;
//! - `on_event(_)` returns empty effects.
//!
//! Rebuilt via `just build-fixtures` from the repo root.

// `cargo-component` generates a `bindings` module at the standard
// `src/bindings.rs` path; we use it via `mod bindings`.
#[allow(warnings)]
mod bindings;

use std::cell::Cell;

use bindings::Guest;
use bindings::exports::savvagent::plugin::screens::{
    Guest as ScreensGuest, GuestScreenInstance, ScreenArgs, ScreenInstance,
};
use bindings::savvagent::plugin::types as t;

/// The top-level world export. Plugin manifest entry point. The
/// per-screen `create_screen` constructor lives on the `screens`
/// exported interface alongside the `screen-instance` resource.
struct Component;

impl Guest for Component {
    fn manifest() -> Result<t::PluginManifest, t::PluginError> {
        Ok(t::PluginManifest {
            id: "fixture.interactive".to_string(),
            name: "fixture-interactive".to_string(),
            version: "0.1.0".to_string(),
            description: "Test fixture for the interactive-world adapter".to_string(),
            kind: t::PluginKind::Optional,
            contributions: t::Contributions {
                slash_commands: vec![],
                hooks: vec![],
                screens: vec!["test".to_string()],
                render_slots: vec![],
                keybindings: vec![],
                themes: false,
            },
        })
    }
}

/// The `screens` exported interface — carries the `create_screen`
/// constructor and the `screen-instance` resource's `GuestScreenInstance`
/// impl.
impl ScreensGuest for Component {
    type ScreenInstance = TestScreen;

    fn create_screen(
        screen_id: String,
        _args: ScreenArgs,
    ) -> Result<ScreenInstance, t::PluginError> {
        if screen_id != "test" {
            return Err(t::PluginError::ScreenNotFound(screen_id));
        }
        Ok(ScreenInstance::new(TestScreen::new()))
    }
}

/// Per-screen state: count of `on_key` invocations.
pub struct TestScreen {
    key_count: Cell<u32>,
}

impl TestScreen {
    fn new() -> Self {
        Self {
            key_count: Cell::new(0),
        }
    }
}

impl GuestScreenInstance for TestScreen {
    fn on_key(&self, _key: t::KeyEventPortable) -> Result<Vec<t::Effect>, t::PluginError> {
        self.key_count.set(self.key_count.get() + 1);
        Ok(vec![t::Effect::PushNote(t::Note {
            text: "keyed".to_string(),
            level: t::NoteLevel::Info,
        })])
    }

    fn on_event(&self, _event_json: String) -> Result<Vec<t::Effect>, t::PluginError> {
        Ok(Vec::new())
    }

    fn render(&self, _area: t::Region) -> Vec<t::StyledLine> {
        let mut out = vec![styled_line("hello")];
        let n = self.key_count.get();
        if n > 0 {
            out.push(styled_line(&format!("keys={n}")));
        }
        out
    }

    fn tips(&self) -> Vec<t::StyledLine> {
        vec![styled_line("press any key")]
    }
}

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
