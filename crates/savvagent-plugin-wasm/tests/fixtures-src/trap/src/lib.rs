//! `fixture-trap` — Task 7 fault fixture.
//!
//! Implements `plugin-static`. `manifest()` is well-formed and declares
//! one slash command `"boom"`. `handle_slash("boom", ..)` deliberately
//! emits a wasm trap so the host's adapter can be exercised on the
//! recovery path.
//!
//! Why `core::arch::wasm32::unreachable()` rather than a plain
//! `panic!("trap")`?
//!
//! cargo-component lets you compile `core::panic!` against the
//! wasm32-unknown-unknown target only by linking a tiny panic adapter,
//! but the way it surfaces the panic varies between cargo-component
//! releases — some versions abort the wasm with `wasi:exit`, some emit
//! a clean trap, and the wasi exit path would force this crate to
//! depend on `wasm32-wasip2` (which would in turn force the host to
//! wire up wasi-cli stubs the static world doesn't otherwise need).
//!
//! `wasm32::unreachable()` is the canonical "emit a `unreachable`
//! instruction" intrinsic; the wasmtime engine always surfaces it as a
//! trap with a message containing "unreachable". That gives the host
//! test a stable string to match on regardless of cargo-component's
//! panic policy.
//!
//! Rebuilt via `just build-fixture-trap` from the repo root.

#[allow(warnings)]
mod bindings;

use bindings::Guest;
use bindings::savvagent::plugin::types as t;

struct Component;

impl Guest for Component {
    fn manifest() -> Result<t::PluginManifest, t::PluginError> {
        Ok(t::PluginManifest {
            id: "fixture.trap".to_string(),
            name: "fixture-trap".to_string(),
            version: "0.1.0".to_string(),
            description: "Fault fixture: handle_slash traps on /boom".to_string(),
            kind: t::PluginKind::Optional,
            contributions: t::Contributions {
                slash_commands: vec!["boom".to_string()],
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
        if name == "boom" {
            // Emit a wasm `unreachable` instruction. Wasmtime maps this
            // to `wasmtime::Trap::UnreachableCodeReached`, which the host
            // adapter stringifies into "wasm trap in handle_slash:
            // wasm trap: wasm `unreachable` instruction executed". The
            // integration test asserts on the lowercase string
            // containing "unreachable" (also matches "panic" / "trap"
            // for forward-compatibility with future error-message
            // wording changes).
            core::arch::wasm32::unreachable();
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
