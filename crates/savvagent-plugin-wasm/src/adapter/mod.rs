//! Adapters that bridge wasm components to host-facing trait objects.
//!
//! Each adapter handles one WIT world:
//!
//! - [`static_::StaticAdapter`] — `plugin-static`, the simplest world:
//!   slash commands, hooks, themes, render slots. One long-lived store per
//!   adapter. Surfaces as `Box<dyn savvagent_plugin::Plugin>`.
//!
//! - [`interactive::InteractiveAdapter`] — `plugin-interactive`,
//!   per-screen-open Store + a `screen-instance` resource that owns
//!   instance-local state. The trait surface for `Screen::render`/`tips`
//!   is sync-returns-Vec<StyledLine>; the adapter caches the most recent
//!   wasm render output and re-issues the wasm call after every key/event.
//!   Surfaces as `Box<dyn savvagent_plugin::Plugin>`.
//!
//! - [`provider::WasmProviderClient`] — `plugin-provider`, one Store per
//!   call (no reuse), HTTP + keyring + streaming-progress capabilities.
//!   Surfaces as `Box<dyn savvagent_mcp::ProviderClient>` — distinct from
//!   the `Plugin` trait the static/interactive adapters target.

pub mod interactive;
pub mod provider;
pub mod static_;

pub use interactive::InteractiveAdapter;
pub use provider::WasmProviderClient;
pub use static_::StaticAdapter;
