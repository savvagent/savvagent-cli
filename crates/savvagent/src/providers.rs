//! Provider catalog.
//!
//! Each entry describes one supported LLM provider. The TUI links every
//! built-in provider crate as a library; nothing is spawned.
//!
//! Adding a new built-in provider is a one-entry change — implement
//! `ProviderHandler` in a new crate, expose a builder, and append to
//! [`PROVIDERS`].
//!
//! ## External (wasm) provider plugins
//!
//! Discovered wasm plugins that implement the `plugin-provider` world
//! contribute additional `ProviderSpec` entries at runtime via
//! [`install_external_providers`]. The combined catalog (built-ins first,
//! externals appended in discovery order) is the source of truth every
//! TUI surface should read; call [`effective_providers`] instead of
//! iterating [`PROVIDERS`] directly so external providers show up in
//! `/connect`, `/model`, and the splash banner alongside built-ins.
//!
//! `EXTERNAL_PROVIDERS` is a [`std::sync::OnceLock`] — set exactly once
//! by the TUI's bootstrap path (right after [`crate::plugin::register_builtins_with_external`]).
//! Mutating the catalog after startup (`/plugins reload <id>`) is a Task
//! 11+ follow-up; for v0.18.0 the user restarts the TUI to pick up a
//! re-trusted plugin.

use std::sync::OnceLock;

/// Static metadata for one provider.
///
/// Built-in entries below use string literals for every field; external
/// (wasm-discovered) entries leak `Box<str>` into `'static` storage at
/// startup so the `&'static str` shape is uniform across both
/// populations. Keeping `Copy` lets the rest of the TUI hold `Option<&'static ProviderSpec>`
/// fields without lifetime gymnastics.
#[derive(Clone, Copy)]
pub struct ProviderSpec {
    /// Stable identifier — keyring account name and `/connect` selector key.
    pub id: &'static str,
    /// Pretty name shown in the selector.
    pub display_name: &'static str,
    /// The env var the underlying SDK conventionally reads. Used only as a
    /// hint in the API-key prompt; we never actually read or set it.
    /// For keyless providers (see [`api_key_required`]) this is the URL
    /// override env var instead.
    pub api_key_env: &'static str,
    /// Default model id passed to the host when this provider connects.
    pub default_model: &'static str,
    /// When `false`, the `/connect` flow skips the API-key prompt and the
    /// keyring read/write entirely.
    pub api_key_required: bool,
}

/// Built-in providers the TUI offers in `/connect`. External wasm
/// providers extend this list at runtime via [`install_external_providers`];
/// call [`effective_providers`] to see the combined catalog.
pub const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "anthropic",
        display_name: "Anthropic (Claude)",
        api_key_env: "ANTHROPIC_API_KEY",
        default_model: "claude-haiku-4-5",
        api_key_required: true,
    },
    ProviderSpec {
        id: "gemini",
        display_name: "Google Gemini",
        api_key_env: "GEMINI_API_KEY",
        default_model: "gemini-2.5-flash",
        api_key_required: true,
    },
    ProviderSpec {
        id: "openai",
        display_name: "OpenAI",
        api_key_env: "OPENAI_API_KEY",
        default_model: "gpt-4o-mini",
        api_key_required: true,
    },
    ProviderSpec {
        id: "local",
        display_name: "Ollama (local)",
        api_key_env: "OLLAMA_HOST",
        default_model: "llama3.2",
        api_key_required: false,
    },
];

/// Externally-discovered wasm provider specs. Populated once at startup
/// by [`install_external_providers`]; surfaces through
/// [`effective_providers`] alongside the built-in [`PROVIDERS`] slice.
///
/// `OnceLock<Vec<_>>` (not `RwLock`) because the v0.18.0 lifecycle is
/// strictly "set once at boot, read for the rest of the process". A
/// mutable catalog (for `/plugins reload`) is a Task 11+ extension that
/// would swap this for a `RwLock<Vec<_>>` without changing call-site
/// shapes — every reader already goes through [`effective_providers`].
static EXTERNAL_PROVIDERS: OnceLock<Vec<ProviderSpec>> = OnceLock::new();

/// Install discovered wasm provider specs into the runtime catalog.
///
/// Called once by the TUI bootstrap with the `[exports] provider-id`
/// taken from every successfully-loaded `plugin-provider` plugin. Each
/// `ProviderSpec`'s `id`/`display_name`/`api_key_env`/`default_model`
/// fields are `&'static str`; the caller is responsible for leaking the
/// underlying `String`s into 'static storage before constructing the
/// spec. The savvagent crate's bootstrap path does this with
/// `String::leak` once per plugin.
///
/// Subsequent calls (e.g. a second bootstrap path in tests) are silently
/// ignored — [`OnceLock::set`] returns the previously-stored value as
/// `Err`. Callers that need to verify install success can read it back
/// via [`effective_providers`].
pub fn install_external_providers(specs: Vec<ProviderSpec>) {
    // OnceLock::set returns Err if already set; ignore — we never need
    // to update after the first call.
    let _ = EXTERNAL_PROVIDERS.set(specs);
}

/// Built-in providers followed by any wasm-plugin providers installed at
/// startup. Returned in discovery order so the first match in
/// `iter().find(|s| s.id == …)` chains is deterministic.
///
/// Returns `&'static ProviderSpec` references rather than owned values
/// so callers can compare references and store handles indefinitely —
/// both the built-in slice and the externally-set `OnceLock` storage
/// live for the rest of the process.
pub fn effective_providers() -> Vec<&'static ProviderSpec> {
    let mut v: Vec<&'static ProviderSpec> = PROVIDERS.iter().collect();
    if let Some(ext) = EXTERNAL_PROVIDERS.get() {
        for s in ext {
            v.push(s);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `effective_providers` returns the built-in slice when no external
    /// providers have been installed. (We can't reset `OnceLock` between
    /// tests so this only holds before any other test in the same process
    /// calls `install_external_providers`. The current TUI lifecycle never
    /// calls it in tests — the only call is in the integration test
    /// crate, which gets its own process — so this is a stable assertion.)
    #[test]
    fn effective_providers_includes_builtins() {
        let eff = effective_providers();
        assert!(eff.len() >= PROVIDERS.len());
        // Built-in slice is contained as the prefix.
        for (i, spec) in PROVIDERS.iter().enumerate() {
            assert_eq!(eff[i].id, spec.id);
        }
    }
}
