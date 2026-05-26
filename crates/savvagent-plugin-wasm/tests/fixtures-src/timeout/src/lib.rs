//! `fixture-timeout` — Task 7 fault fixture.
//!
//! Implements `plugin-static`. `handle_slash("forever", ..)` enters an
//! infinite busy-loop. The integration test wraps the call in
//! `tokio::time::timeout` to prove the *host* can bound its own
//! awaiting; the wasm-thread interruption path lands in Task 8 (epoch
//! bumping is currently disabled in `engine.rs`).
//!
//! Rebuilt via `just build-fixture-timeout` from the repo root.

#[allow(warnings)]
mod bindings;

use bindings::Guest;
use bindings::savvagent::plugin::types as t;

struct Component;

impl Guest for Component {
    fn manifest() -> Result<t::PluginManifest, t::PluginError> {
        Ok(t::PluginManifest {
            id: "fixture.timeout".to_string(),
            name: "fixture-timeout".to_string(),
            version: "0.1.0".to_string(),
            description: "Fault fixture: handle_slash loops on /forever".to_string(),
            kind: t::PluginKind::Optional,
            contributions: t::Contributions {
                slash_commands: vec!["forever".to_string()],
                hooks: vec![],
                screens: vec![],
                render_slots: vec![],
                keybindings: vec![],
                themes: false,
            },
        })
    }

    fn handle_slash(
        name: String,
        _args: Vec<String>,
    ) -> Result<Vec<t::Effect>, t::PluginError> {
        if name == "forever" {
            // Busy-loop indefinitely. Without epoch interruption (Task 8)
            // this can only be terminated by tearing down the engine.
            //
            // `core::hint::spin_loop` keeps wasmtime from optimizing the
            // loop into a single trap-or-return on some opt-level paths.
            loop {
                core::hint::spin_loop();
            }
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
