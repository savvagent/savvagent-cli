//! Parser/validator for `plugin.toml` files.
//!
//! Validation steps (in order):
//! 1. TOML parses.
//! 2. Required fields present (enforced by `serde`).
//! 3. id format is `<lowercase-kebab>.<lowercase-kebab>`.
//! 4. id matches the parent directory name.
//! 5. `savvagent` version range is satisfiable by the current build's WIT
//!    contract version.
//! 6. `[security]` is rejected on non-provider worlds.
//! 7. `[exports] provider-id` is required for `plugin-provider`.
//! 8. `runtime.call-timeout-ms` is clamped to a 300_000 ms ceiling.
//!
//! `world` is one of three known values, enforced by the `PluginWorld`
//! enum's `Deserialize` impl.
//!
//! ## Deferred validation (post-v0.18.0)
//!
//! Cross-checking declared `[exports]` against the actual wasm component
//! exports is **not** performed at load time. A manifest can claim
//! `themes = true` while the underlying wasm exports no theme function,
//! and the discrepancy will not surface until a host call routes to the
//! missing export. The cross-check would need to walk the wasm's
//! component-model export list (via `wasmparser` or
//! `wasmtime::component::Component::component_type`) and reconcile each
//! claim individually; the additional complexity is not justified for
//! the v0.18.0 cut. Tracked for a follow-up release alongside the
//! related host-side hardening work in Task 8.

use std::path::Path;

use serde::Deserialize;

use crate::error::WasmPluginError;

/// WIT contract version the running build implements. Manifests declare a
/// caret range against this; we only accept `^0.18`-prefixed ranges in v1.
///
/// This is **independent** of the workspace `version` in `Cargo.toml`. The
/// workspace can bump from 0.17 → 0.18 → 0.19 without changing the WIT
/// contract version, and vice-versa.
const CURRENT_WIT_VERSION: &str = "0.18";

/// Parsed `plugin.toml` for a single external plugin.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PluginManifest {
    /// Required `[plugin]` table — identity, version, world.
    pub plugin: PluginSection,
    /// Optional `[exports]` table — what the plugin contributes to the
    /// host's UX surface. Defaults to all-empty.
    #[serde(default)]
    pub exports: ExportsSection,
    /// Optional `[security]` table — host-allowlist + keyring-account
    /// allowlist. Provider-world plugins only.
    #[serde(default)]
    pub security: Option<SecuritySection>,
    /// Optional `[runtime]` table — per-call timeout. Defaults to 5s.
    #[serde(default)]
    pub runtime: RuntimeSection,
}

/// `[plugin]` section — identity and version.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PluginSection {
    /// Globally-unique plugin id in `<org>.<name>` form, lowercase
    /// kebab-case in both segments.
    pub id: String,
    /// Human-readable display name. Shown in `/plugins` listings.
    pub name: String,
    /// Plugin's own version (SemVer-ish, not validated by us).
    pub version: String,
    /// Which WIT world this plugin implements.
    pub world: PluginWorld,
    /// One-line description for `/plugins list`. Optional.
    #[serde(default)]
    pub description: String,
    /// Plugin homepage URL. Optional.
    #[serde(default)]
    pub homepage: Option<String>,
    /// SPDX license id. Optional.
    #[serde(default)]
    pub license: Option<String>,
    /// Plugin authors. Optional.
    #[serde(default)]
    pub authors: Vec<String>,
    /// Required savvagent version range — caret-prefixed (e.g. `^0.18`).
    /// Must align with `CURRENT_WIT_VERSION`.
    pub savvagent: String,
    /// Relative path to the wasm binary. Defaults to `plugin.wasm` in
    /// Tasks 4–6 (`None` here means "use convention").
    #[serde(default)]
    pub wasm: Option<String>,
}

/// Which WIT world the plugin implements.
///
/// Serialized as kebab-case in TOML (`plugin-static`, `plugin-interactive`,
/// `plugin-provider`); `serde` rejects any other value at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginWorld {
    /// Pure data plugin — slash commands, themes, hooks. No UI thread.
    PluginStatic,
    /// Owns UI surface — modals, screens, render slots, keybindings.
    PluginInteractive,
    /// Extends `PROVIDERS` with a new model provider over the SPP wire
    /// protocol.
    PluginProvider,
}

/// `[exports]` section — declares what UX/integration surfaces the plugin
/// contributes. Field-by-field defaults to empty so plugins only declare
/// what they actually export.
///
/// TOML keys are kebab-case (`slash-commands`, `provider-id`, …); the
/// `rename_all` attribute maps them to Rust's snake_case fields.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct ExportsSection {
    /// Slash commands like `["lint", "format"]` (no leading `/`).
    #[serde(default)]
    pub slash_commands: Vec<String>,
    /// Hook event names like `["PreToolUse", "Stop"]`.
    #[serde(default)]
    pub hooks: Vec<String>,
    /// Screen ids the plugin registers.
    #[serde(default)]
    pub screens: Vec<String>,
    /// Render-slot ids the plugin contributes to.
    #[serde(default)]
    pub render_slots: Vec<String>,
    /// Keybinding ids the plugin registers.
    #[serde(default)]
    pub keybindings: Vec<String>,
    /// Whether the plugin ships theme(s).
    #[serde(default)]
    pub themes: bool,
    /// Required for `plugin-provider` world: the provider id this plugin
    /// adds to `PROVIDERS` (e.g. `"acme-llm"`).
    #[serde(default)]
    pub provider_id: Option<String>,
}

/// `[security]` section — host-network + keyring allowlists. Only valid for
/// `plugin-provider` world.
///
/// TOML keys are kebab-case; see [`ExportsSection`] for the rationale.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct SecuritySection {
    /// Hostnames the plugin is allowed to reach via the `net` capability.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// OS-keyring account names the plugin is allowed to read.
    #[serde(default)]
    pub keyring_accounts: Vec<String>,
}

/// `[runtime]` section — per-call resource limits.
///
/// TOML keys are kebab-case; see [`ExportsSection`] for the rationale.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct RuntimeSection {
    /// Per-call timeout in milliseconds. Clamped at load time to a
    /// 300_000 ms (5-minute) ceiling.
    #[serde(default = "default_call_timeout")]
    pub call_timeout_ms: u32,
}

impl Default for RuntimeSection {
    fn default() -> Self {
        Self {
            call_timeout_ms: 5_000,
        }
    }
}

fn default_call_timeout() -> u32 {
    5_000
}

impl PluginManifest {
    /// Parse + validate a `plugin.toml` at `path`.
    ///
    /// `expected_id` is the directory name the discovery walker found this
    /// manifest under; we require the manifest's own `id` field to match
    /// (catches typos and id-renames).
    pub fn load(path: &Path, expected_id: &str) -> Result<Self, WasmPluginError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| WasmPluginError::Io(path.to_path_buf(), e))?;
        let m: PluginManifest = toml::from_str(&text)
            .map_err(|e| WasmPluginError::Manifest(path.to_path_buf(), e.to_string()))?;

        validate_id(&m.plugin.id)
            .map_err(|reason| WasmPluginError::InvalidId(m.plugin.id.clone(), reason))?;

        if m.plugin.id != expected_id {
            return Err(WasmPluginError::Manifest(
                path.to_path_buf(),
                format!(
                    "id '{}' does not match directory '{expected_id}'",
                    m.plugin.id
                ),
            ));
        }

        validate_version_range(&m.plugin.savvagent).map_err(|reason| {
            WasmPluginError::VersionMismatch(
                m.plugin.id.clone(),
                m.plugin.savvagent.clone(),
                reason,
            )
        })?;

        // [security] is provider-world only.
        if m.security.is_some() && !matches!(m.plugin.world, PluginWorld::PluginProvider) {
            return Err(WasmPluginError::Manifest(
                path.to_path_buf(),
                "[security] is only valid for plugin-provider world".into(),
            ));
        }

        // Provider plugins must declare provider_id.
        if matches!(m.plugin.world, PluginWorld::PluginProvider) && m.exports.provider_id.is_none()
        {
            return Err(WasmPluginError::Manifest(
                path.to_path_buf(),
                "plugin-provider must set [exports] provider-id".into(),
            ));
        }

        // Cap call timeout at 300s.
        let runtime = RuntimeSection {
            call_timeout_ms: m.runtime.call_timeout_ms.min(300_000),
        };

        Ok(PluginManifest { runtime, ..m })
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    let parts: Vec<&str> = id.split('.').collect();
    if parts.len() != 2 {
        return Err("must be <org>.<name> (exactly one dot)".into());
    }
    for part in &parts {
        if part.is_empty() {
            return Err("segments must be non-empty".into());
        }
        if !part
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!("segment '{part}' must be lowercase kebab-case"));
        }
        if part.starts_with('-') || part.ends_with('-') {
            return Err(format!("segment '{part}' must not start/end with '-'"));
        }
    }
    Ok(())
}

fn validate_version_range(range: &str) -> Result<(), String> {
    // Accept caret ranges only in v1: "^0.18", "^0.18.0", "^0.18.1".
    let stripped = range
        .strip_prefix('^')
        .ok_or("must be caret-prefixed (e.g. ^0.18)")?;
    // Match `CURRENT_WIT_VERSION` either exactly, or followed by `.`
    // (a patch segment). Reject hyphenless continuations like `"0.189"`
    // that would otherwise satisfy `starts_with("0.18")`.
    let exact = stripped == CURRENT_WIT_VERSION;
    let with_patch = stripped
        .strip_prefix(CURRENT_WIT_VERSION)
        .is_some_and(|tail| tail.starts_with('.'));
    if !exact && !with_patch {
        return Err(format!(
            "requires {stripped} but build provides {CURRENT_WIT_VERSION}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_manifest(s: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{s}").unwrap();
        f
    }

    #[test]
    fn valid_static_manifest_parses() {
        let f = write_manifest(
            r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"

[exports]
slash-commands = ["demo"]
themes = false
"#,
        );
        let m = PluginManifest::load(f.path(), "acme.demo").unwrap();
        assert_eq!(m.plugin.world, PluginWorld::PluginStatic);
        assert_eq!(m.exports.slash_commands, vec!["demo".to_string()]);
    }

    #[test]
    fn id_mismatch_rejected() {
        let f = write_manifest(
            r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#,
        );
        let e = PluginManifest::load(f.path(), "acme.other").unwrap_err();
        assert!(matches!(e, WasmPluginError::Manifest(_, _)));
    }

    #[test]
    fn security_on_static_rejected() {
        let f = write_manifest(
            r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"

[security]
allowed-hosts = ["example.com"]
"#,
        );
        let e = PluginManifest::load(f.path(), "acme.demo").unwrap_err();
        assert!(matches!(e, WasmPluginError::Manifest(_, _)));
    }

    #[test]
    fn provider_without_provider_id_rejected() {
        let f = write_manifest(
            r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-provider"
savvagent = "^0.18"
"#,
        );
        let e = PluginManifest::load(f.path(), "acme.demo").unwrap_err();
        assert!(matches!(e, WasmPluginError::Manifest(_, _)));
    }

    #[test]
    fn provider_with_provider_id_accepted() {
        let f = write_manifest(
            r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-provider"
savvagent = "^0.18"

[exports]
provider-id = "acme-llm"

[security]
allowed-hosts = ["api.example.com"]
"#,
        );
        let m = PluginManifest::load(f.path(), "acme.demo").unwrap();
        assert_eq!(m.plugin.world, PluginWorld::PluginProvider);
        assert_eq!(m.exports.provider_id.as_deref(), Some("acme-llm"));
        assert!(m.security.is_some());
    }

    #[test]
    fn version_range_mismatch_rejected() {
        let f = write_manifest(
            r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.17"
"#,
        );
        let e = PluginManifest::load(f.path(), "acme.demo").unwrap_err();
        assert!(matches!(e, WasmPluginError::VersionMismatch(..)));
    }

    #[test]
    fn missing_caret_rejected() {
        let f = write_manifest(
            r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "0.18"
"#,
        );
        let e = PluginManifest::load(f.path(), "acme.demo").unwrap_err();
        assert!(matches!(e, WasmPluginError::VersionMismatch(..)));
    }

    #[test]
    fn hyphenless_version_continuation_rejected() {
        // `"^0.189"` previously slipped through because `"0.189"` starts with
        // `"0.18"`. Must reject because 0.189 is not the 0.18 line.
        for bad in ["^0.180", "^0.189", "^0.1899", "^0.18a"] {
            let toml = format!(
                r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "{bad}"
"#
            );
            let f = write_manifest(&toml);
            let e = PluginManifest::load(f.path(), "acme.demo").unwrap_err();
            assert!(
                matches!(e, WasmPluginError::VersionMismatch(..)),
                "expected VersionMismatch for '{bad}', got {e:?}"
            );
        }
    }

    #[test]
    fn exact_and_patch_version_accepted() {
        for ok in ["^0.18", "^0.18.0", "^0.18.99"] {
            let toml = format!(
                r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "{ok}"
"#
            );
            let f = write_manifest(&toml);
            PluginManifest::load(f.path(), "acme.demo")
                .unwrap_or_else(|e| panic!("'{ok}' should be accepted: {e:?}"));
        }
    }

    #[test]
    fn id_format_rejected() {
        for bad in ["acme", "Acme.demo", "acme.", ".demo", "a.b.c", "acme.de_mo"] {
            let toml = format!(
                r#"
[plugin]
id = "{bad}"
name = "x"
version = "0"
world = "plugin-static"
savvagent = "^0.18"
"#
            );
            let f = write_manifest(&toml);
            let e = PluginManifest::load(f.path(), bad).unwrap_err();
            assert!(
                matches!(
                    e,
                    WasmPluginError::InvalidId(..) | WasmPluginError::Manifest(..)
                ),
                "expected invalid-id error for '{bad}', got {e:?}"
            );
        }
    }

    #[test]
    fn id_with_leading_dash_rejected() {
        let f = write_manifest(
            r#"
[plugin]
id = "-acme.demo"
name = "x"
version = "0"
world = "plugin-static"
savvagent = "^0.18"
"#,
        );
        let e = PluginManifest::load(f.path(), "-acme.demo").unwrap_err();
        assert!(matches!(e, WasmPluginError::InvalidId(..)));
    }

    #[test]
    fn call_timeout_capped() {
        let f = write_manifest(
            r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"

[runtime]
call-timeout-ms = 9999999
"#,
        );
        let m = PluginManifest::load(f.path(), "acme.demo").unwrap();
        assert_eq!(m.runtime.call_timeout_ms, 300_000);
    }

    #[test]
    fn invalid_world_rejected() {
        // `serde` rejects unknown variants at parse time, surfacing as a
        // TOML/serde error — i.e. `Manifest`, not `InvalidWorld`. The
        // `InvalidWorld` variant is reserved for places that synthesize a
        // world value from a string at runtime (Tasks 4–6).
        let f = write_manifest(
            r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-bogus"
savvagent = "^0.18"
"#,
        );
        let e = PluginManifest::load(f.path(), "acme.demo").unwrap_err();
        assert!(matches!(e, WasmPluginError::Manifest(..)));
    }
}
