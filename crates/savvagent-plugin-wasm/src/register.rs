//! Discovery → trust check → instantiation → adapter wrapping for external
//! plugins.
//!
//! [`register_external`] walks the four discovery paths, consults the trust
//! ledger, and for every (trusted, hash-matching) candidate constructs the
//! adapter for the declared world. The TUI bootstrap consumes the returned
//! [`RegisterResult`]:
//!
//! - `plugins` slot in to `BuiltinSet::plugins` so static + interactive
//!   wasm plugins are indistinguishable from built-ins in the
//!   `PluginRegistry`.
//! - `provider_clients` carries the `(provider-id, Arc<WasmProviderClient>)`
//!   pairs the TUI needs to splice into the legacy `PROVIDERS` slice via
//!   [`crate::register::install_external_providers`]-side helpers (the
//!   savvagent crate owns that surface; this module just yields the data).
//! - `warnings` collects best-effort, human-readable diagnostics (one per
//!   skipped plugin: untrusted, hash-mismatch, disabled, or instantiation
//!   failure). Discovery's own warnings are appended verbatim so the caller
//!   gets one combined channel.
//!
//! ## Failure model — never bubble up partial failure
//!
//! Any per-plugin failure (instantiation, trust evaluation, manifest
//! re-parse, hash failure) becomes a `warnings` entry and the plugin is
//! skipped. The function only returns `Err` when a fundamental layer
//! fails — typically loading the trust file itself (malformed TOML, etc).
//! That distinction matches the TUI's risk model: a single broken plugin
//! must never break startup for everyone else.

use std::path::Path;
use std::sync::Arc;

use savvagent_plugin::Plugin;

use crate::adapter::provider::WasmProviderClient;
use crate::adapter::{InteractiveAdapter, StaticAdapter};
use crate::discovery::discover;
use crate::error::WasmPluginError;
use crate::host_imports::theme::ThemeProvider;
use crate::manifest::PluginWorld;
use crate::trust::{TrustCheck, TrustFile, tree_hash};

/// What [`register_external`] hands back to the TUI bootstrap.
///
/// `plugins` and `provider_clients` are independent channels because they
/// terminate in two different host surfaces:
///
/// - `plugins` lands in the `PluginRegistry` (slash / hook / render-slot
///   dispatch) and is opaque past the `Box<dyn Plugin>` boundary.
/// - `provider_clients` lands in the TUI's `PROVIDERS` extender + a future
///   `host.add_provider` flow (Tasks 10/11). `WasmProviderClient` is
///   wrapped in `Arc` so the TUI can hand out as many `dyn ProviderClient`
///   handles as the host pool requires without re-instantiating.
pub struct RegisterResult {
    /// Adapter-wrapped static + interactive plugins, ready to extend the
    /// `BuiltinSet::plugins` vec.
    pub plugins: Vec<Box<dyn Plugin>>,
    /// `(provider-id, client)` pairs for every successfully-loaded
    /// `plugin-provider` plugin. The `provider-id` is taken from
    /// `[exports] provider-id` in `plugin.toml` (manifest validation has
    /// already ensured it is present for the provider world).
    pub provider_clients: Vec<(String, Arc<WasmProviderClient>)>,
    /// Best-effort, human-readable diagnostics. The TUI routes each line
    /// to `tracing::warn!` and/or `app.push_note` at startup so the user
    /// can debug skipped plugins.
    pub warnings: Vec<String>,
}

/// Discover, trust-check, and instantiate every external plugin under the
/// four well-known paths.
///
/// Arguments mirror [`crate::discovery::discover`]:
/// - `project_root`: where to look for `<project>/.savvagent/plugins/`
///   and `<project>/.claude/plugins/`. `None` skips the project tier
///   (useful for headless smoke tests).
/// - `home_dir`: where to look for `~/.savvagent/plugins/` and
///   `~/.claude/plugins/`, and also where `plugin-trust.toml` lives.
/// - `theme`: shared theme handle the static + interactive adapters wire
///   into their `current-theme()` host import.
///
/// ## Trust-ledger save
///
/// When discovery surfaces a `HashMismatch` we call
/// [`TrustFile::revoke`] so a future `/plugins trust <id>` re-applies
/// from scratch — and then save the trust file at the end. Save errors
/// are swallowed (logged via `warnings`) to keep the bootstrap path
/// total: a non-writable HOME shouldn't break the TUI's startup.
pub async fn register_external(
    project_root: Option<&Path>,
    home_dir: &Path,
    theme: ThemeProvider,
) -> Result<RegisterResult, WasmPluginError> {
    let discovery = discover(project_root, Some(home_dir));
    let mut trust = TrustFile::load(home_dir)?;
    let mut plugins: Vec<Box<dyn Plugin>> = Vec::new();
    let mut provider_clients: Vec<(String, Arc<WasmProviderClient>)> = Vec::new();
    let mut warnings = discovery.warnings;
    let mut trust_dirty = false;

    for dp in discovery.plugins {
        // Compute the current tree-hash up front; trust evaluation needs
        // it. A `tree_hash` failure (unreadable file) skips the plugin
        // with a warning rather than failing the whole bootstrap.
        let hash = match tree_hash(&dp.dir) {
            Ok(h) => h,
            Err(e) => {
                warnings.push(format!(
                    "[plugins] {}: tree_hash failed: {e}",
                    dp.manifest.plugin.id
                ));
                continue;
            }
        };

        match trust.check(&dp.manifest.plugin.id, &hash) {
            TrustCheck::Ok => {}
            TrustCheck::Untrusted => {
                warnings.push(format!(
                    "[plugins] {} is untrusted; run /plugins trust {} to enable it",
                    dp.manifest.plugin.id, dp.manifest.plugin.id
                ));
                continue;
            }
            TrustCheck::HashMismatch { stored, actual } => {
                // Revoke so the user sees an untrusted state on the next
                // restart rather than a stale ledger entry. The save at
                // the end of this function persists the change.
                trust.revoke(&dp.manifest.plugin.id);
                trust_dirty = true;
                warnings.push(format!(
                    "[plugins] {} hash mismatch (stored={stored}, actual={actual}); \
                     trust revoked — re-trust via /plugins trust {}",
                    dp.manifest.plugin.id, dp.manifest.plugin.id
                ));
                continue;
            }
            TrustCheck::Disabled(reason) => {
                warnings.push(format!(
                    "[plugins] {} disabled: {reason}",
                    dp.manifest.plugin.id
                ));
                continue;
            }
        }

        let dm = Arc::new(dp.manifest.clone());
        match dp.manifest.plugin.world {
            PluginWorld::PluginStatic => match StaticAdapter::new(dm, &dp.dir, theme.clone()).await
            {
                Ok(adapter) => plugins.push(Box::new(adapter)),
                Err(e) => warnings.push(format!(
                    "[plugins] {}: static-adapter init failed: {e}",
                    dp.manifest.plugin.id
                )),
            },
            PluginWorld::PluginInteractive => {
                match InteractiveAdapter::new(dm, &dp.dir, theme.clone()).await {
                    Ok(adapter) => plugins.push(Box::new(adapter)),
                    Err(e) => warnings.push(format!(
                        "[plugins] {}: interactive-adapter init failed: {e}",
                        dp.manifest.plugin.id
                    )),
                }
            }
            PluginWorld::PluginProvider => {
                // Manifest validation already guarantees provider_id is
                // Some(_) for the provider world; treat absence here as
                // a programming error in manifest.rs and skip with a
                // warning rather than panicking.
                let Some(provider_id) = dp.manifest.exports.provider_id.clone() else {
                    warnings.push(format!(
                        "[plugins] {}: provider-world manifest missing exports.provider-id \
                         (manifest validation should have rejected this)",
                        dp.manifest.plugin.id
                    ));
                    continue;
                };
                match WasmProviderClient::new(dm, &dp.dir).await {
                    Ok(client) => provider_clients.push((provider_id, Arc::new(client))),
                    Err(e) => warnings.push(format!(
                        "[plugins] {}: provider-adapter init failed: {e}",
                        dp.manifest.plugin.id
                    )),
                }
            }
        }
    }

    // Persist the trust ledger if any HashMismatch revoked an entry.
    // Save failures become warnings (so a read-only HOME doesn't break
    // startup); next launch will re-discover the mismatch and try again.
    if trust_dirty && let Err(e) = trust.save(home_dir) {
        warnings.push(format!("[plugins] trust file save failed: {e}"));
    }

    Ok(RegisterResult {
        plugins,
        provider_clients,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Discovery with no project root + an empty HOME finds nothing and
    /// emits zero warnings. Confirms the function is total against the
    /// trivial input.
    #[tokio::test]
    async fn register_external_empty_home_is_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let theme = crate::host_imports::theme::provider(Vec::new());
        let result = register_external(None, tmp.path(), theme).await.unwrap();
        assert!(result.plugins.is_empty());
        assert!(result.provider_clients.is_empty());
        assert!(result.warnings.is_empty());
    }

    /// A plugin on disk without a trust entry should be skipped with a
    /// warning, not instantiated. Uses a fabricated plugin dir (no real
    /// wasm needed — trust check fails before `StaticAdapter::new`).
    #[tokio::test]
    async fn register_external_untrusted_skipped_with_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = tmp.path().join(".savvagent/plugins/acme.demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#,
        )
        .unwrap();
        // No `plugin.wasm` and no trust entry — trust check fires first
        // and skips before we ever try to load the wasm.
        let theme = crate::host_imports::theme::provider(Vec::new());
        let result = register_external(None, tmp.path(), theme).await.unwrap();
        assert!(result.plugins.is_empty());
        assert_eq!(result.warnings.len(), 1);
        assert!(
            result.warnings[0].contains("untrusted"),
            "expected 'untrusted' warning, got: {}",
            result.warnings[0]
        );
    }

    /// A plugin with a stale trust entry triggers HashMismatch, which
    /// revokes the trust record and emits a warning. Subsequent
    /// `TrustFile::load` should show the entry removed.
    #[tokio::test]
    async fn register_external_hash_mismatch_revokes_trust() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = tmp.path().join(".savvagent/plugins/acme.demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#,
        )
        .unwrap();

        // Pre-trust with a hash that does not match the on-disk tree.
        let mut tf = TrustFile::default();
        tf.trust("acme.demo", "bogus-hash".into(), None);
        tf.save(tmp.path()).unwrap();

        let theme = crate::host_imports::theme::provider(Vec::new());
        let result = register_external(None, tmp.path(), theme).await.unwrap();
        assert!(result.plugins.is_empty());
        assert_eq!(result.warnings.len(), 1);
        assert!(
            result.warnings[0].contains("hash mismatch"),
            "expected 'hash mismatch' warning, got: {}",
            result.warnings[0]
        );
        // The revoke landed on disk.
        let reloaded = TrustFile::load(tmp.path()).unwrap();
        assert!(
            !reloaded.plugins.contains_key("acme.demo"),
            "trust record must be removed after HashMismatch"
        );
    }
}
