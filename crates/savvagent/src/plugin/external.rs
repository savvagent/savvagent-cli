//! External (wasm-discovered) plugin wiring for the TUI bootstrap.
//!
//! This module composes [`crate::plugin::register_builtins`] with
//! [`savvagent_plugin_wasm::register::register_external`] so the TUI's
//! single entry point — [`register_builtins_with_external`] — returns a
//! [`BuiltinSet`] that already contains both built-in and wasm plugins.
//!
//! ## What this module is responsible for
//!
//! 1. Calling `register_builtins` to get the built-in `BuiltinSet`.
//! 2. Calling `register_external` to discover, trust-check, and
//!    instantiate wasm plugins under the four well-known paths.
//! 3. Extending `BuiltinSet::plugins` with the wasm plugin adapters
//!    so they share the registry's slash/hook/render-slot routing path.
//! 4. Synthesising one [`crate::providers::ProviderSpec`] per wasm
//!    provider plugin and installing them into the runtime catalog via
//!    [`crate::providers::install_external_providers`], so the
//!    `/connect` selector, `/model` picker, and splash banner pick them
//!    up alongside built-in providers.
//!
//! ## What this module is NOT responsible for
//!
//! - Wiring wasm provider clients into the host's `Pool` so `/connect`
//!   on a wasm provider actually opens a turn. That requires
//!   `ProviderRegistration` plumbing on the host side and is a Task 11
//!   follow-up; v0.18.0 surfaces wasm providers in the UI for
//!   discoverability but does not yet make them connectable. The
//!   `WasmProviderClient`s returned by `register_external` are
//!   intentionally dropped here for now (see the inline comment); a
//!   future revision will pipe them into the host pool.
//! - Theme synchronisation between the TUI's active theme and the
//!   `ThemeProvider` passed to wasm adapters. v0.18.0 hands every wasm
//!   plugin an empty theme snapshot (the `current-theme()` host import
//!   returns no entries) and treats that as acceptable for first-cut
//!   theme-consumer plugins. Polish lands in Task 13.

use std::path::Path;

use savvagent_plugin_wasm::host_imports::theme::ThemeProvider;

use crate::plugin::BuiltinSet;
use crate::plugin::builtin;
use crate::providers::{ProviderSpec, install_external_providers};

/// Build a [`BuiltinSet`] containing both the built-in plugins and any
/// wasm plugins discovered under the four well-known paths.
///
/// Argument order mirrors [`crate::plugin::register_builtins`] with two
/// trailing additions:
///
/// - `home_dir`: where to look for `~/.savvagent/plugins/` /
///   `~/.claude/plugins/` and `plugin-trust.toml`. The TUI passes
///   `dirs::home_dir()` here. `None` skips the user-tier discovery
///   entirely (useful in tests that fake out the project tier only).
/// - `theme`: shared theme handle the wasm static/interactive adapters
///   wire into their `current-theme()` host import.
///
/// ## Failure model
///
/// External-plugin failures (discovery errors, trust check rejections,
/// instantiation failures) are logged via `tracing::warn!` and surfaced
/// in the returned `warnings` Vec. They do NOT abort the bootstrap —
/// one broken plugin never breaks startup for everyone else.
///
/// The trust-file load failure path is the one exception: if the trust
/// file is malformed (corrupted TOML), `register_external` returns
/// `Err`. In that case we log the error and proceed with built-ins
/// only — the alternative would be a silent reset of every prior trust
/// decision, which the spec forbids.
pub(crate) async fn register_builtins_with_external(
    trust_levels: builtin::user_slash_commands::TrustMap,
    user_hooks_index: std::sync::Arc<
        tokio::sync::RwLock<crate::plugin::builtin::user_hooks::discovery::HooksIndex>,
    >,
    session_id: String,
    project_root: std::path::PathBuf,
    transcript_path: std::sync::Arc<tokio::sync::RwLock<std::path::PathBuf>>,
    home_dir: Option<&Path>,
    theme: ThemeProvider,
) -> (BuiltinSet, Vec<String>) {
    // Step 1: built-ins.
    let mut set = crate::plugin::register_builtins(
        trust_levels,
        user_hooks_index,
        session_id,
        project_root.clone(),
        transcript_path,
    );

    // Step 2: wasm plugins. Bail out gracefully on hard failures so the
    // TUI can still boot with built-ins only.
    let Some(home) = home_dir else {
        return (set, Vec::new());
    };

    let result =
        match savvagent_plugin_wasm::register::register_external(Some(&project_root), home, theme)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e,
                    "external-plugin discovery failed; continuing with built-ins only");
                return (
                    set,
                    vec![format!("[plugins] external-plugin bootstrap failed: {e}")],
                );
            }
        };

    // Step 3: surface warnings through tracing for log-only consumers.
    // The same Vec is returned so the TUI can `app.push_note` each one
    // once `App` exists.
    for warn in &result.warnings {
        tracing::warn!("{warn}");
    }

    // Step 4: extend the plugin Vec. Wasm adapters implement `Plugin`
    // exactly the same as built-ins; the registry doesn't distinguish.
    set.plugins.extend(result.plugins);

    // Step 5: synthesise ProviderSpec entries for any wasm-provider
    // plugins and install them into the runtime catalog. The
    // wasm-provider client itself is intentionally dropped here — see
    // the module-level note: hooking it into the host pool is a Task
    // 11 follow-up. For v0.18.0 the provider id surfaces in the
    // `/connect` selector as a discoverability cue, but selecting it
    // doesn't yet open a turn.
    if !result.provider_clients.is_empty() {
        let specs: Vec<ProviderSpec> = result
            .provider_clients
            .iter()
            .map(|(id, _client)| synthesize_provider_spec(id))
            .collect();
        install_external_providers(specs);
        // Drop the Arc<WasmProviderClient>s. The compiler would do this
        // anyway; the explicit drop documents that we know these are
        // unused in v0.18.0 and the choice is deliberate (vs. holding
        // them in a global to be wired in by a later task).
        drop(result.provider_clients);
    }

    (set, result.warnings)
}

/// Synthesise a [`ProviderSpec`] for one wasm-discovered provider id.
///
/// The wasm-side `provider-id` is a `String` from `plugin.toml`; the
/// existing `ProviderSpec` shape uses `&'static str` for every text
/// field (so the rest of the TUI can hold `Option<&'static ProviderSpec>`
/// without lifetime parameters). To bridge the two, we leak the
/// per-plugin strings into 'static storage via `Box::leak` exactly
/// once at startup. The leak is bounded by the number of plugins
/// installed at boot, never grows after `install_external_providers`
/// returns, and matches the rest of the TUI's "string constants live
/// forever" assumption.
fn synthesize_provider_spec(provider_id: &str) -> ProviderSpec {
    // Display name = capitalised id with hyphens turned into spaces.
    // Plugin authors can override this later via a manifest field
    // (Task 13 follow-up); for the v0.18.0 cut we just want something
    // legible in the selector.
    let display = format!(
        "{} (wasm plugin)",
        provider_id.chars().next().map_or(String::new(), |c| {
            let mut s = c.to_uppercase().to_string();
            s.push_str(&provider_id[c.len_utf8()..]);
            s
        })
    );

    // `String::leak` is the canonical Rust idiom for taking a String to
    // `&'static str` (stable since Rust 1.72). The Box of bytes lives
    // for the rest of the process — exactly what the `&'static str`
    // shape needs.
    let id_static: &'static str = Box::leak(provider_id.to_string().into_boxed_str());
    let display_static: &'static str = Box::leak(display.into_boxed_str());

    ProviderSpec {
        id: id_static,
        display_name: display_static,
        // Wasm providers manage their own credentials inside the wasm
        // sandbox (via the keyring host import); the API-key prompt
        // doesn't apply. We still pass a placeholder env-var hint so the
        // existing `/connect` paths don't NPE on an empty string, but
        // `api_key_required = false` short-circuits the prompt.
        api_key_env: "",
        // The wasm plugin's `list-models` export is the source of truth
        // for available models; this default is only used when the
        // selector opens before any `list-models` call. Empty is fine
        // — the runtime treats it as "no preference".
        default_model: "",
        api_key_required: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthesised spec round-trips its id and produces a sensible
    /// display name. Confirms the `Box::leak` + display-name formatter
    /// works for typical inputs.
    #[test]
    fn synthesize_provider_spec_round_trips_id() {
        let spec = synthesize_provider_spec("acme-llm");
        assert_eq!(spec.id, "acme-llm");
        assert_eq!(spec.display_name, "Acme-llm (wasm plugin)");
        assert!(!spec.api_key_required);
        assert_eq!(spec.api_key_env, "");
    }

    /// Empty id doesn't panic and produces a recognisable display name.
    #[test]
    fn synthesize_provider_spec_handles_empty_id() {
        let spec = synthesize_provider_spec("");
        assert_eq!(spec.id, "");
        // Empty id → display starts with " (wasm plugin)" — odd but
        // total. Manifest validation guarantees we never see this in
        // practice; the test just locks down "no panic on weird input".
        assert!(spec.display_name.ends_with("(wasm plugin)"));
    }
}
