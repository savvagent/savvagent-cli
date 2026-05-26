//! Error types for the wasm runtime — covers discovery, manifest parsing,
//! trust validation, and runtime adapter errors.
//!
//! Tasks 4–6 will extend this enum with capability-specific variants
//! (network, keyring, draw); the variants here are the ones the discovery
//! pipeline (Task 3) and the trust ledger need.

use std::path::PathBuf;
use thiserror::Error;

/// All failures the wasm-plugin runtime can produce. Stored as enum
/// variants per error class so callers can pattern-match.
///
/// The `Wasmtime` variant wraps `anyhow::Error` because `wasmtime`'s public
/// API itself returns `anyhow::Error` from `Instance::call_async` and
/// component instantiation; bubbling those via `?` is the path-of-least
/// resistance for the adapter glue in Tasks 4–6.
#[derive(Debug, Error)]
pub enum WasmPluginError {
    /// `plugin.toml` parsed but failed semantic validation, or TOML itself
    /// was malformed. The `PathBuf` is the manifest file the error refers
    /// to; the `String` is a human-readable reason.
    #[error("manifest at {0:?}: {1}")]
    Manifest(PathBuf, String),

    /// Filesystem I/O failure — `read_to_string`, `write`, `create_dir_all`,
    /// `read` of a plugin file, etc.
    #[error("io error at {0:?}: {1}")]
    Io(PathBuf, std::io::Error),

    /// Plugin id failed the `<org>.<name>` lowercase-kebab format check.
    #[error("plugin id '{0}' is invalid: {1}")]
    InvalidId(String, String),

    /// `plugin.toml` declared a `world` outside the three known values.
    #[error("plugin world '{0}' is not one of plugin-static|plugin-interactive|plugin-provider")]
    InvalidWorld(String),

    /// Manifest's declared exports do not line up with what the wasm
    /// component actually exports. Reserved for Tasks 4–6; included here
    /// to keep the discriminants stable across the sub-project.
    #[error("plugin {0} declares exports {1:?} but wasm exports {2:?}")]
    ExportMismatch(String, Vec<String>, Vec<String>),

    /// Plugin's required `savvagent` version range is unsatisfiable by the
    /// running build.
    #[error("plugin {0} requires savvagent {1} but this build provides {2}")]
    VersionMismatch(String, String, String),

    /// `tree_hash` of the plugin directory does not match the recorded
    /// `sha256_tree` in `plugin-trust.toml`. Tasks 6+ surface this to the
    /// user before instantiation.
    #[error("plugin {0} hash mismatch: stored={1} actual={2}")]
    HashMismatch(String, String, String),

    /// Plugin has no entry in `plugin-trust.toml`, or has `trusted = false`.
    #[error("plugin {0} is not trusted; run /plugins trust {0}")]
    Untrusted(String),

    /// Plugin is recorded in the trust ledger but `disabled_reason` is
    /// non-empty (e.g. three-strikes auto-disable from Task 8).
    #[error("plugin {0} is disabled: {1}")]
    Disabled(String, String),

    /// Any error bubbled up from `wasmtime` — instantiation failure, trap,
    /// component-link error.
    #[error("wasmtime: {0}")]
    Wasmtime(#[from] anyhow::Error),

    /// A capability host function refused a request — e.g. a `net.fetch`
    /// to a host not in `[security] allowed-hosts`.
    #[error("capability denied: {0}")]
    CapabilityDenied(String),
}
