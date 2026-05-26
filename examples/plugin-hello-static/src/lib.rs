//! `plugin-hello-static` — the simplest possible savvagent external plugin.
//!
//! It exports the `plugin-static` world and registers a single slash command,
//! `/hello`, which surfaces a "Hello from WASM!" toast in the TUI.
//!
//! ## Build
//!
//! ```bash
//! cargo component build --release --target wasm32-unknown-unknown
//! ```
//!
//! ## Install
//!
//! Copy the produced `.wasm` plus this directory's `plugin.toml` into one of
//! savvagent's plugin discovery paths, e.g.
//! `~/.savvagent/plugins/hello-static/`, then run `/plugins install` (or
//! reload). On first activation savvagent will prompt you to trust the
//! plugin's tree hash.
//!
//! ## What this example demonstrates
//!
//! - The four required `Guest` methods of the `plugin-static` world:
//!   `manifest`, `handle_slash`, `on_event`, `render_slot`, plus `themes`.
//! - Returning a single `Effect::PushNote` from a slash handler.
//! - The minimum manifest a plugin can ship (no hooks, no render slots, no
//!   keybindings, no themes).

// cargo-component generates `src/bindings.rs` at build time; we re-export it
// as a module here. The generated code triggers a handful of lints we don't
// own, hence `#[allow(warnings)]`.
#[allow(warnings)]
mod bindings;

use bindings::Guest;
use bindings::savvagent::plugin::types as t;

struct Component;

impl Guest for Component {
    /// Tell the host what this plugin is and what it contributes. The host
    /// matches the manifest's contributions against the active world's
    /// allow-list and surfaces the plugin in `/plugins`.
    fn manifest() -> Result<t::PluginManifest, t::PluginError> {
        Ok(t::PluginManifest {
            id: "savvagent.hello-static".to_string(),
            name: "Hello (Static)".to_string(),
            version: "0.1.0".to_string(),
            description: "Minimal static-world example. Defines /hello.".to_string(),
            kind: t::PluginKind::Optional,
            contributions: t::Contributions {
                slash_commands: vec!["hello".to_string()],
                hooks: vec![],
                screens: vec![],
                render_slots: vec![],
                keybindings: vec![],
                themes: false,
            },
        })
    }

    /// Handle `/hello`. Anything else falls through to an empty effect list,
    /// which is the correct behavior for an unrecognised command in a
    /// well-behaved plugin (the host routes by name and only calls us for
    /// commands we declared, but we stay defensive anyway).
    fn handle_slash(
        name: String,
        _args: Vec<String>,
    ) -> Result<Vec<t::Effect>, t::PluginError> {
        if name == "hello" {
            return Ok(vec![t::Effect::PushNote(t::Note {
                text: "Hello from WASM!".to_string(),
                level: t::NoteLevel::Info,
            })]);
        }
        Ok(Vec::new())
    }

    /// No hooks subscribed, so this is a no-op — but the world still
    /// requires the export.
    fn on_event(_event_json: String) -> Result<Vec<t::Effect>, t::PluginError> {
        Ok(Vec::new())
    }

    /// No render slots declared.
    fn render_slot(_slot_id: String, _area: t::Region) -> Vec<t::StyledLine> {
        Vec::new()
    }

    /// No theme contributions.
    fn themes() -> Vec<t::ThemeEntry> {
        Vec::new()
    }
}

bindings::export!(Component with_types_in bindings);
