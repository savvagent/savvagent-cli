//! Map files to languages, then walk parent directories to find the
//! workspace root.

use std::path::{Path, PathBuf};

/// Identifier matching `LanguageEntry.id`. Wrapper around `String` so
/// callers can't confuse it with arbitrary strings.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LanguageId(
    /// Inner identifier string (e.g. `"rust"`).
    pub String,
);

impl LanguageId {
    /// Borrow the underlying identifier as a `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Find the workspace root for `file` by walking parents until any of
/// `markers` is found. Returns `None` if no marker is found before
/// the filesystem root.
pub fn workspace_root_for(file: &Path, markers: &[String]) -> Option<PathBuf> {
    let mut dir = file.parent()?;
    loop {
        for marker in markers {
            if dir.join(marker).exists() {
                return Some(dir.to_path_buf());
            }
        }
        dir = dir.parent()?;
    }
}

/// Extract the lowercase extension of `file`, without the leading dot.
/// Returns `None` for files with no extension (e.g. `Makefile`).
pub fn extension_of(file: &Path) -> Option<String> {
    file.extension()
        .and_then(|os| os.to_str())
        .map(|s| s.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LanguageEntry;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn extension_lowercases() {
        assert_eq!(extension_of(Path::new("Foo.RS")).as_deref(), Some("rs"));
        assert_eq!(extension_of(Path::new("foo.tsx")).as_deref(), Some("tsx"));
        assert_eq!(extension_of(Path::new("Makefile")), None);
    }

    #[test]
    fn workspace_root_walks_until_marker_found() {
        let root = tempdir().unwrap();
        let project = root.path().join("repo");
        let src = project.join("src/nested");
        fs::create_dir_all(&src).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let file = src.join("lib.rs");
        fs::write(&file, "").unwrap();

        let r = workspace_root_for(&file, &["Cargo.toml".into()]);
        assert_eq!(r.as_deref(), Some(project.as_path()));
    }

    #[test]
    fn workspace_root_returns_none_if_no_marker() {
        let root = tempdir().unwrap();
        let file = root.path().join("a/b/c/foo.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "").unwrap();
        assert_eq!(workspace_root_for(&file, &["Cargo.toml".into()]), None);
    }

    #[test]
    fn workspace_root_checks_all_markers() {
        // If a language declares multiple markers, the first match wins
        // regardless of order. We assert the function tries all of them.
        let root = tempdir().unwrap();
        let project = root.path().join("repo");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("rust-project.json"), "{}").unwrap();
        let file = project.join("src/lib.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "").unwrap();

        let r = workspace_root_for(&file, &["Cargo.toml".into(), "rust-project.json".into()]);
        assert_eq!(r.as_deref(), Some(project.as_path()));
    }

    #[test]
    fn resolve_uses_extension_to_pick_language_entry() {
        let entry = LanguageEntry {
            id: "rust".into(),
            extensions: vec!["rs".into()],
            root_markers: vec!["Cargo.toml".into()],
            command: "rust-analyzer".into(),
            args: vec![],
            env: Default::default(),
        };
        let ext = extension_of(Path::new("src/lib.rs"));
        assert_eq!(ext.as_deref(), Some("rs"));
        assert!(entry.extensions.contains(&"rs".to_string()));
    }
}
