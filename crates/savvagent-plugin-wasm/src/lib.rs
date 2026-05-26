//! Wasmtime-backed runtime for savvagent external plugins.
//!
//! This crate adapts WASM components implementing one of three WIT worlds
//! (plugin-static / plugin-interactive / plugin-provider) to
//! `Box<dyn savvagent_plugin::Plugin>` — making them indistinguishable from
//! built-ins to the rest of the host.
//!
//! Tasks 1–2 landed the WIT contract and the host-side bindgen output.
//! Task 3 added the runtime's discovery + trust layer. Task 4 (this
//! revision) layers in the first wasmtime adapter — the static-world
//! adapter — plus the shared infrastructure Tasks 5 and 6 will reuse:
//!
//! - [`engine`] — process-wide shared `wasmtime::Engine` (Task 4).
//! - [`convert`] — free-function conversions between the WIT bindgen
//!   output and `savvagent_plugin` types (Effect, Manifest, HookKind,
//!   ThemeColor, …; Task 4).
//! - [`host_imports`] — host-side implementations of the capability
//!   surface every WIT world declares as `import`s (`log`,
//!   `current-theme`; Task 4). Draw + HTTP/keyring/progress land in Tasks
//!   5 and 6.
//! - [`adapter`] — `StaticAdapter` wraps a `plugin-static` wasm component
//!   as a `Box<dyn savvagent_plugin::Plugin>` (Task 4). Interactive and
//!   provider adapters land in Tasks 5 and 6.
//!
//! Trust enforcement, capability denial paths, and the interactive /
//! provider adapters land in subsequent tasks.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod adapter;
pub mod convert;
pub mod discovery;
pub mod engine;
pub mod error;
pub mod host_imports;
pub mod manifest;
pub mod register;
pub mod spp_convert;
pub mod strikes;
pub mod trust;

/// Re-export of the WIT-resources crate so downstream callers don't have to
/// pull it in separately when they want the canonical `WIT_DIR` path.
pub use savvagent_plugin_wit as wit;

// ---- Host-side bindings ------------------------------------------------
//
// `wasmtime::component::bindgen!` expands the `.wit` tree at the given
// `path:` into Rust types, traits, and a `World` struct. The macro is a
// proc-macro and therefore requires a *string-literal* path at the call
// site — it cannot read `savvagent_plugin_wit::WIT_DIR` even though that
// would resolve to the same directory. The relative path below resolves
// from this crate's `src/` to the sibling crate's `wit/` directory.
//
// Each world gets its own module to keep the three sets of generated
// types from colliding. `async: true` is required so the generated traits
// match the async wasmtime store the adapters will use in Tasks 4–6.

/// Host bindings for the `plugin-static` world.
#[allow(missing_docs, clippy::needless_lifetimes)]
pub mod static_world {
    wasmtime::component::bindgen!({
        path: "../savvagent-plugin-wit/wit",
        world: "plugin-static",
        async: true,
    });
}

/// Host bindings for the `plugin-interactive` world.
///
/// The `with:` clause aliases the shared `savvagent:plugin/types`
/// interface to the *same* Rust types the static world emits — so
/// conversion helpers in [`crate::convert`] (which use the static-world
/// type module) compose cleanly with the interactive adapter without
/// per-world duplicates.
#[allow(missing_docs, clippy::needless_lifetimes)]
pub mod interactive_world {
    wasmtime::component::bindgen!({
        path: "../savvagent-plugin-wit/wit",
        world: "plugin-interactive",
        async: true,
        with: {
            "savvagent:plugin/types@0.1.0": crate::static_world::savvagent::plugin::types,
        },
    });
}

/// Host bindings for the `plugin-provider` world.
///
/// Like the interactive world, this aliases the shared
/// `savvagent:plugin/types` interface to the *same* Rust types the static
/// world emits — so `LogLevel`, `PluginManifest`, and `PluginError`
/// resolve to identical types across the three worlds (callers can pass
/// a `wit::LogLevel` from any world's bindings to the shared host
/// imports without per-world dispatch). The `spp` interface is *not*
/// aliased: it has no counterpart in the other worlds, so per-world
/// duplicates would be empty anyway.
#[allow(missing_docs, clippy::needless_lifetimes)]
pub mod provider_world {
    wasmtime::component::bindgen!({
        path: "../savvagent-plugin-wit/wit",
        world: "plugin-provider",
        async: true,
        with: {
            "savvagent:plugin/types@0.1.0": crate::static_world::savvagent::plugin::types,
        },
    });
}
