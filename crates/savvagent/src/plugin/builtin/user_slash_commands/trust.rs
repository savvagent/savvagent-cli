//! Loads and saves `~/.savvagent/trusted-projects.json` — the persistent
//! store of "always trust this project's commands" decisions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Trust level for a given project root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustLevel {
    /// User chose "trust always" — persisted to disk.
    Always,
    /// User chose "block shell, allow text-only this session" —
    /// in-memory only; not persisted.
    SessionTextOnly,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileSchema {
    #[serde(default)]
    projects: BTreeMap<String, String>,
}

/// File path the trust store lives at. `home` is the user's home dir
/// (caller supplies it so the function is testable).
pub fn trust_file_path(home: &Path) -> PathBuf {
    home.join(".savvagent").join("trusted-projects.json")
}

/// Load the persisted trust set. Missing file → empty map, no warning.
/// Unreadable or malformed file → empty map plus a warning string.
pub fn load(home: &Path) -> (BTreeMap<PathBuf, TrustLevel>, Option<String>) {
    let path = trust_file_path(home);
    if !path.exists() {
        return (BTreeMap::new(), None);
    }
    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return (BTreeMap::new(), Some(format!("trust file unreadable: {e}"))),
    };
    let parsed: FileSchema = match serde_json::from_str(&contents) {
        Ok(p) => p,
        Err(e) => return (BTreeMap::new(), Some(format!("trust file malformed: {e}"))),
    };
    let mut out = BTreeMap::new();
    for (k, v) in parsed.projects {
        if v == "always" {
            out.insert(PathBuf::from(k), TrustLevel::Always);
        }
    }
    (out, None)
}

/// Persist the `Always` entries to disk. `SessionTextOnly` is skipped
/// (in-memory only).
pub fn save(home: &Path, levels: &BTreeMap<PathBuf, TrustLevel>) -> Result<(), String> {
    let mut schema = FileSchema::default();
    for (k, v) in levels {
        if matches!(v, TrustLevel::Always) {
            if let Some(s) = k.to_str() {
                schema.projects.insert(s.to_string(), "always".into());
            }
        }
    }
    let path = trust_file_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
    }
    let body =
        serde_json::to_string_pretty(&schema).map_err(|e| format!("serialize trust file: {e}"))?;
    std::fs::write(&path, body).map_err(|e| format!("write trust file: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_when_file_absent() {
        let tmp = TempDir::new().unwrap();
        let (m, warn) = load(tmp.path());
        assert!(m.is_empty());
        assert!(warn.is_none());
    }

    #[test]
    fn round_trip_persists_only_always() {
        let tmp = TempDir::new().unwrap();
        let mut input = BTreeMap::new();
        input.insert(PathBuf::from("/proj/a"), TrustLevel::Always);
        input.insert(PathBuf::from("/proj/b"), TrustLevel::SessionTextOnly);
        save(tmp.path(), &input).unwrap();

        let (loaded, warn) = load(tmp.path());
        assert!(warn.is_none());
        assert_eq!(
            loaded.get(&PathBuf::from("/proj/a")),
            Some(&TrustLevel::Always)
        );
        assert!(!loaded.contains_key(&PathBuf::from("/proj/b")));
    }

    #[test]
    fn malformed_file_returns_empty_with_warning() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".savvagent")).unwrap();
        std::fs::write(trust_file_path(tmp.path()), "{ not json").unwrap();
        let (m, warn) = load(tmp.path());
        assert!(m.is_empty());
        assert!(warn.is_some());
    }
}
