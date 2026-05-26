//! `fixture-static` — minimum-viable wasm component for the
//! `savvagent-plugin-wasm` static-world adapter tests.
//!
//! Exports a `plugin-static` world that:
//! - declares an `"echo"` slash command in the manifest;
//! - subscribes to `turn-start`;
//! - responds to `/echo <args...>` with one `PushNote` containing the
//!   args joined by spaces;
//! - returns empty effects for any other slash or any event;
//! - contributes no themes, no render slots, no keybindings.
//!
//! Rebuilt via `just build-fixtures` from the repo root.

// `cargo-component` generates a `bindings` module at the standard
// `src/bindings.rs` path; we use it via `mod bindings`.
#[allow(warnings)]
mod bindings;

// The `plugin-static` world's exports live at the *root* of the bindings
// module (since they're declared at world level rather than inside a
// nested interface). The types come in via `savvagent::plugin::types`.
use bindings::Guest;
use bindings::savvagent::plugin::types as t;

struct Component;

impl Guest for Component {
    fn manifest() -> Result<t::PluginManifest, t::PluginError> {
        Ok(t::PluginManifest {
            id: "fixture.static".to_string(),
            name: "fixture-static".to_string(),
            version: "0.1.0".to_string(),
            description: "Test fixture for the static-world adapter".to_string(),
            kind: t::PluginKind::Optional,
            contributions: t::Contributions {
                slash_commands: vec!["echo".to_string()],
                hooks: vec![t::HookKind::TurnStart],
                screens: vec![],
                render_slots: vec![],
                keybindings: vec![],
                themes: false,
            },
        })
    }

    fn handle_slash(
        name: String,
        args: Vec<String>,
    ) -> Result<Vec<t::Effect>, t::PluginError> {
        if name == "echo" {
            return Ok(vec![t::Effect::PushNote(t::Note {
                text: args.join(" "),
                level: t::NoteLevel::Info,
            })]);
        }
        Ok(Vec::new())
    }

    fn on_event(_event_json: String) -> Result<Vec<t::Effect>, t::PluginError> {
        Ok(Vec::new())
    }

    fn render_slot(_slot_id: String, _area: t::Region) -> Vec<t::StyledLine> {
        Vec::new()
    }

    fn themes() -> Vec<t::ThemeEntry> {
        Vec::new()
    }
}

bindings::export!(Component with_types_in bindings);
