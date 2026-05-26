//! Manages `~/.savvagent/plugin-trust.toml` — the per-user trust ledger
//! for external plugins.
//!
//! Trust unit: SHA-256 over the plugin's full directory tree
//! (`plugin.toml`, `plugin.wasm`, any `assets/*` file), with filenames
//! sorted UTF-8 ascending and the path **and** content both folded into
//! the digest. A rename without a content change is therefore a hash
//! change, by design.
//!
//! Security note: [`TrustFile::save`] writes the ledger **non-atomically**
//! via `std::fs::write`. If the process is killed mid-write the file ends
//! up truncated or partial, and the next [`TrustFile::load`] will fail with
//! `WasmPluginError::Manifest`. This is acceptable for the v0.18.0 cut
//! (per spec §7 non-goals) — a future hardening PR can adopt a
//! tempfile + rename pattern.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::WasmPluginError;

/// In-memory representation of `plugin-trust.toml`. Backed by a `BTreeMap`
/// so serialization order is deterministic (helps with diff review and
/// merge-conflict-free dotfile sync).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TrustFile {
    /// Per-plugin trust records, keyed by plugin id.
    #[serde(default)]
    pub plugins: BTreeMap<String, TrustRecord>,
}

/// One row in the ledger — what the user trusts and at what content
/// fingerprint.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TrustRecord {
    /// `true` when the user has explicitly trusted this id+hash pair.
    /// A revoked record is removed entirely, not flipped to `false`.
    pub trusted: bool,
    /// SHA-256 hex over the plugin directory tree — see [`tree_hash`].
    pub sha256_tree: String,
    /// Unix timestamp (seconds) when the user trusted this version.
    pub trusted_at: u64,
    /// Optional URL the plugin was installed from. Surfaces in
    /// `/plugins list` and informs the integrity-warning UI.
    pub source_url: Option<String>,
    /// Non-empty when the plugin is currently disabled (manually or by
    /// the Task 8 three-strikes auto-disable). An empty string means the
    /// record is in its normal enabled state.
    #[serde(default)]
    pub disabled_reason: String,
}

impl TrustFile {
    /// Load the trust file from `<home_dir>/.savvagent/plugin-trust.toml`.
    ///
    /// Returns `Ok(TrustFile::default())` if the file does not exist —
    /// fresh installs have no ledger. Malformed TOML, on the other hand,
    /// is **propagated as an error**: we will not silently produce an
    /// empty ledger from a corrupted file, because that would silently
    /// reset every prior trust decision.
    pub fn load(home_dir: &Path) -> Result<Self, WasmPluginError> {
        let path = home_dir.join(".savvagent/plugin-trust.toml");
        if !path.exists() {
            return Ok(TrustFile::default());
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| WasmPluginError::Io(path.clone(), e))?;
        toml::from_str(&text).map_err(|e| WasmPluginError::Manifest(path, e.to_string()))
    }

    /// Persist the trust file to `<home_dir>/.savvagent/plugin-trust.toml`,
    /// creating intermediate directories as needed.
    ///
    /// **Non-atomic** — see the module-level doc.
    pub fn save(&self, home_dir: &Path) -> Result<(), WasmPluginError> {
        let path = home_dir.join(".savvagent/plugin-trust.toml");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| WasmPluginError::Io(parent.to_path_buf(), e))?;
        }
        let text = toml::to_string(self)
            .map_err(|e| WasmPluginError::Manifest(path.clone(), e.to_string()))?;
        std::fs::write(&path, text).map_err(|e| WasmPluginError::Io(path, e))?;
        Ok(())
    }

    /// Inspect the ledger entry for `id` against the freshly-computed
    /// `current_tree_hash`.
    ///
    /// Decision order (each gate short-circuits the rest):
    /// 1. Missing record         → [`TrustCheck::Untrusted`]
    /// 2. `disabled_reason` set  → [`TrustCheck::Disabled`]
    /// 3. `trusted == false`     → [`TrustCheck::Untrusted`]
    /// 4. Hash mismatch          → [`TrustCheck::HashMismatch`]
    /// 5. Otherwise              → [`TrustCheck::Ok`]
    ///
    /// Note step 2 fires **before** step 3, so a disabled trusted plugin
    /// shows up as `Disabled`, not `Untrusted` — that's the correct UX:
    /// the user already trusted it once and just needs to re-enable.
    pub fn check(&self, id: &str, current_tree_hash: &str) -> TrustCheck {
        let Some(rec) = self.plugins.get(id) else {
            return TrustCheck::Untrusted;
        };
        if !rec.disabled_reason.is_empty() {
            return TrustCheck::Disabled(rec.disabled_reason.clone());
        }
        if !rec.trusted {
            return TrustCheck::Untrusted;
        }
        if rec.sha256_tree != current_tree_hash {
            return TrustCheck::HashMismatch {
                stored: rec.sha256_tree.clone(),
                actual: current_tree_hash.to_string(),
            };
        }
        TrustCheck::Ok
    }

    /// Record an explicit user trust decision. Overwrites any prior entry
    /// for `id` (e.g. when retrusting after a hash change).
    pub fn trust(&mut self, id: &str, tree_hash: String, source_url: Option<String>) {
        self.plugins.insert(
            id.to_string(),
            TrustRecord {
                trusted: true,
                sha256_tree: tree_hash,
                trusted_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                source_url,
                disabled_reason: String::new(),
            },
        );
    }

    /// Remove the trust record for `id` entirely. The next [`check`] for
    /// this id will return [`TrustCheck::Untrusted`].
    ///
    /// [`check`]: TrustFile::check
    pub fn revoke(&mut self, id: &str) {
        self.plugins.remove(id);
    }

    /// Mark the plugin as disabled without removing its trust record.
    /// Used by manual `/plugins disable` (Task 11) and the Task 8
    /// auto-disable path. No-op if the id has no record.
    pub fn set_disabled(&mut self, id: &str, reason: &str) {
        if let Some(rec) = self.plugins.get_mut(id) {
            rec.disabled_reason = reason.to_string();
        }
    }

    /// Clear the disabled flag on an existing record. No-op if the id
    /// has no record.
    pub fn clear_disabled(&mut self, id: &str) {
        if let Some(rec) = self.plugins.get_mut(id) {
            rec.disabled_reason.clear();
        }
    }
}

/// Outcome of [`TrustFile::check`]. The `Ok` variant is the only one the
/// host should consider safe to instantiate; the others all surface to the
/// user via the plugin-manager UX.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustCheck {
    /// Plugin is trusted and on-disk content matches the recorded hash.
    Ok,
    /// No trust record exists, or `trusted = false`.
    Untrusted,
    /// Trust record exists but the on-disk hash differs.
    HashMismatch {
        /// SHA-256 hex previously recorded in the ledger.
        stored: String,
        /// SHA-256 hex freshly computed by [`tree_hash`].
        actual: String,
    },
    /// Trust record exists with a non-empty `disabled_reason`.
    Disabled(String),
}

/// Compute the SHA-256 over an entire plugin directory tree.
///
/// Files are visited in `walkdir` order, then sorted ascending by
/// `PathBuf::cmp` (which compares the raw OS-string bytes). On
/// Linux/macOS this is byte-order; on Windows it's UTF-16 order, but the
/// trust file is per-user-per-machine, so the platform-local order is
/// stable across runs on the same host.
///
/// Each file contributes `b"path:" + relpath + "\nsize:" + len + "\n" +
/// bytes + "\n"` to the digest, so a rename (without a content change),
/// a content edit, an addition, and a deletion each produce a different
/// hash — exactly what a trust anchor needs.
pub fn tree_hash(plugin_dir: &Path) -> Result<String, WasmPluginError> {
    // Fail-closed on any walk error. A silently-skipped unreadable file
    // would let an attacker temporarily chmod 000 a plugin file, induce
    // a "trust this hash" prompt, then restore perms — the recorded
    // hash would not include the unreadable file, opening a trust-bypass.
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(plugin_dir) {
        let entry = entry.map_err(|e| {
            let path = e
                .path()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| plugin_dir.to_path_buf());
            let io = e.into_io_error().unwrap_or_else(|| {
                std::io::Error::other("walkdir error with no underlying io::Error")
            });
            WasmPluginError::Io(path, io)
        })?;
        if entry.file_type().is_file() {
            files.push(entry.into_path());
        }
    }
    files.sort();

    let mut hasher = Sha256::new();
    for file in files {
        let rel = file.strip_prefix(plugin_dir).unwrap_or(&file);
        let rel_str = rel.to_string_lossy();
        hasher.update(b"path:");
        hasher.update(rel_str.as_bytes());
        hasher.update(b"\n");
        let bytes = std::fs::read(&file).map_err(|e| WasmPluginError::Io(file.clone(), e))?;
        hasher.update(b"size:");
        hasher.update(bytes.len().to_le_bytes());
        hasher.update(b"\n");
        hasher.update(&bytes);
        hasher.update(b"\n");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_hash_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("plugin.toml"), b"a").unwrap();
        std::fs::write(dir.join("plugin.wasm"), b"b").unwrap();
        let h1 = tree_hash(dir).unwrap();
        let h2 = tree_hash(dir).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn tree_hash_detects_change() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("plugin.toml"), b"a").unwrap();
        std::fs::write(dir.join("plugin.wasm"), b"b").unwrap();
        let h1 = tree_hash(dir).unwrap();
        std::fs::write(dir.join("plugin.wasm"), b"c").unwrap();
        let h2 = tree_hash(dir).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn tree_hash_detects_addition_and_deletion() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("plugin.toml"), b"a").unwrap();
        let h1 = tree_hash(dir).unwrap();
        std::fs::write(dir.join("extra"), b"x").unwrap();
        let h2 = tree_hash(dir).unwrap();
        assert_ne!(h1, h2, "added file must change hash");
        std::fs::remove_file(dir.join("extra")).unwrap();
        let h3 = tree_hash(dir).unwrap();
        assert_eq!(h1, h3, "removing the file must restore the original hash");
    }

    #[test]
    fn trust_lifecycle() {
        let tmp = tempfile::tempdir().unwrap();
        let mut tf = TrustFile::default();
        tf.trust("acme.demo", "abc123".into(), Some("https://x".into()));
        tf.save(tmp.path()).unwrap();
        let loaded = TrustFile::load(tmp.path()).unwrap();
        assert_eq!(loaded.check("acme.demo", "abc123"), TrustCheck::Ok);
        assert_eq!(
            loaded.check("acme.demo", "xyz"),
            TrustCheck::HashMismatch {
                stored: "abc123".into(),
                actual: "xyz".into(),
            }
        );
        assert_eq!(loaded.check("acme.other", "abc123"), TrustCheck::Untrusted);
    }

    #[test]
    fn disabled_record_is_disabled() {
        let mut tf = TrustFile::default();
        tf.trust("acme.demo", "abc123".into(), None);
        tf.set_disabled("acme.demo", "repeated-traps");
        match tf.check("acme.demo", "abc123") {
            TrustCheck::Disabled(reason) => assert_eq!(reason, "repeated-traps"),
            other => panic!("expected disabled, got {other:?}"),
        }
    }

    #[test]
    fn clear_disabled_restores_ok() {
        let mut tf = TrustFile::default();
        tf.trust("acme.demo", "abc123".into(), None);
        tf.set_disabled("acme.demo", "repeated-traps");
        tf.clear_disabled("acme.demo");
        assert_eq!(tf.check("acme.demo", "abc123"), TrustCheck::Ok);
    }

    #[test]
    fn revoke_removes_record() {
        let mut tf = TrustFile::default();
        tf.trust("acme.demo", "abc123".into(), None);
        tf.revoke("acme.demo");
        assert_eq!(tf.check("acme.demo", "abc123"), TrustCheck::Untrusted);
    }

    #[test]
    fn malformed_trust_file_errors_not_silently_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".savvagent/plugin-trust.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this isn't toml = ::").unwrap();
        let err = TrustFile::load(tmp.path()).unwrap_err();
        assert!(
            matches!(err, WasmPluginError::Manifest(..)),
            "malformed trust file must surface as Manifest error, got {err:?}"
        );
    }
}
