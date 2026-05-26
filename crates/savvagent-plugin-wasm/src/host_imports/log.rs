//! `log(level, msg)` host capability.
//!
//! Translates a guest-issued `(LogLevel, String)` pair into a structured
//! `tracing` event under `target: "plugin-wasm"`. Plugins use this for
//! anything they want surfaced through the embedder's tracing-subscriber
//! pipeline — typically debug traces during development.
//!
//! The translation is intentionally cheap and side-effect-free: no
//! per-event allocation beyond the borrowed `msg`, no IO that could block
//! the wasm caller.
//!
//! The function lives outside the adapter so the same logic is shareable
//! between the static / interactive / provider worlds.

use crate::convert::log_level_to_tracing;
use crate::static_world::savvagent::plugin::types as wit;

/// Emit a `tracing` event at the given WIT log level. `plugin_id` is the
/// declared id from `plugin.toml`; it tags every event so subscribers can
/// route per-plugin.
pub fn emit(plugin_id: &str, level: wit::LogLevel, msg: &str) {
    let level = log_level_to_tracing(level);
    // `tracing` requires a literal `Level` for its `event!` macro, so we
    // switch by const.
    match level {
        tracing::Level::TRACE => tracing::event!(
            target: "plugin-wasm",
            tracing::Level::TRACE,
            plugin = plugin_id,
            "{msg}"
        ),
        tracing::Level::DEBUG => tracing::event!(
            target: "plugin-wasm",
            tracing::Level::DEBUG,
            plugin = plugin_id,
            "{msg}"
        ),
        tracing::Level::INFO => tracing::event!(
            target: "plugin-wasm",
            tracing::Level::INFO,
            plugin = plugin_id,
            "{msg}"
        ),
        tracing::Level::WARN => tracing::event!(
            target: "plugin-wasm",
            tracing::Level::WARN,
            plugin = plugin_id,
            "{msg}"
        ),
        tracing::Level::ERROR => tracing::event!(
            target: "plugin-wasm",
            tracing::Level::ERROR,
            plugin = plugin_id,
            "{msg}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_does_not_panic_at_every_level() {
        // Without a subscriber configured the events are silently dropped;
        // we only care that `emit` is total and side-effect-safe.
        emit("test:plugin", wit::LogLevel::Trace, "trace");
        emit("test:plugin", wit::LogLevel::Debug, "debug");
        emit("test:plugin", wit::LogLevel::Info, "info");
        emit("test:plugin", wit::LogLevel::Warn, "warn");
        emit("test:plugin", wit::LogLevel::Error, "error");
    }
}
