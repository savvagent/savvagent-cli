//! Parsing for `~/.savvagent/lsp.toml` (global) and `<repo>/.savvagent/lsp.toml`
//! (per-repo override).
//!
//! Schema (one entry per supported language):
//!
//! ```toml
//! [[language]]
//! id = "rust"
//! extensions = ["rs"]
//! root_markers = ["Cargo.toml", "rust-project.json"]
//! command = "rust-analyzer"
//! args = []
//! env = { RUST_LOG = "warn" }   # optional
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// In-memory, merged view of the global + per-repo `lsp.toml` files.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LspConfig {
    /// Configured languages, keyed by `LanguageEntry.id` after merge.
    /// Repo-level entries fully replace global entries with the same `id`.
    #[serde(default, rename = "language")]
    pub languages: Vec<LanguageEntry>,
}

/// One language-server configuration.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LanguageEntry {
    /// Stable id used to look this entry up (e.g. `"rust"`, `"typescript"`).
    pub id: String,
    /// File extensions (no leading dot) handled by this language server.
    pub extensions: Vec<String>,
    /// Filenames whose presence marks the project root for this language.
    pub root_markers: Vec<String>,
    /// Executable to launch (resolved via `$PATH`).
    pub command: String,
    /// Arguments passed to `command`.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables for the spawned language server.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl LspConfig {
    /// Load the global + per-repo files and merge. Per-repo entries
    /// replace global entries with the same `id`. Missing files are
    /// treated as empty configs (no error).
    pub fn load(global_path: &Path, repo_path: Option<&Path>) -> anyhow::Result<Self> {
        let global = load_one(global_path)?;
        let repo = match repo_path {
            Some(p) => load_one(p)?,
            None => LspConfig::default(),
        };
        Ok(merge(global, repo))
    }

    /// Look up a language entry by id. Linear scan; the config is
    /// expected to hold ≤ ~20 entries.
    pub fn language(&self, id: &str) -> Option<&LanguageEntry> {
        self.languages.iter().find(|e| e.id == id)
    }

    /// Map a file extension (no leading dot) to a language id.
    pub fn language_for_extension(&self, ext: &str) -> Option<&LanguageEntry> {
        self.languages
            .iter()
            .find(|e| e.extensions.iter().any(|x| x == ext))
    }
}

fn load_one(path: &Path) -> anyhow::Result<LspConfig> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(toml::from_str::<LspConfig>(&text)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LspConfig::default()),
        Err(e) => Err(anyhow::anyhow!("reading {}: {e}", path.display())),
    }
}

fn merge(global: LspConfig, repo: LspConfig) -> LspConfig {
    // Index global entries by id, then let each repo entry overwrite.
    let mut by_id: HashMap<String, LanguageEntry> = global
        .languages
        .into_iter()
        .map(|e| (e.id.clone(), e))
        .collect();
    for entry in repo.languages {
        by_id.insert(entry.id.clone(), entry);
    }
    let mut languages: Vec<LanguageEntry> = by_id.into_values().collect();
    languages.sort_by(|a, b| a.id.cmp(&b.id));
    LspConfig { languages }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_toml(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn parses_one_language_entry() {
        let global = write_toml(
            r#"
[[language]]
id = "rust"
extensions = ["rs"]
root_markers = ["Cargo.toml"]
command = "rust-analyzer"
"#,
        );
        let cfg = LspConfig::load(global.path(), None).unwrap();
        assert_eq!(cfg.languages.len(), 1);
        assert_eq!(cfg.languages[0].id, "rust");
        assert_eq!(cfg.languages[0].command, "rust-analyzer");
        assert!(cfg.languages[0].args.is_empty());
    }

    #[test]
    fn repo_entry_replaces_global_by_id() {
        let global = write_toml(
            r#"
[[language]]
id = "rust"
extensions = ["rs"]
root_markers = ["Cargo.toml"]
command = "rust-analyzer"
args = ["--log-file=/tmp/global.log"]
"#,
        );
        let repo = write_toml(
            r#"
[[language]]
id = "rust"
extensions = ["rs"]
root_markers = ["Cargo.toml", "rust-project.json"]
command = "rust-analyzer"
args = []
"#,
        );
        let cfg = LspConfig::load(global.path(), Some(repo.path())).unwrap();
        let rust = cfg.language("rust").unwrap();
        assert_eq!(
            rust.root_markers,
            vec!["Cargo.toml".to_string(), "rust-project.json".to_string()],
            "repo entry must fully replace the global entry (no per-field merge)"
        );
        assert!(
            rust.args.is_empty(),
            "repo's empty args must replace global's --log-file"
        );
    }

    #[test]
    fn missing_files_yield_empty_config() {
        let cfg = LspConfig::load(Path::new("/no/such/path.toml"), None).unwrap();
        assert!(cfg.languages.is_empty());
    }

    #[test]
    fn language_for_extension_finds_match() {
        let global = write_toml(
            r#"
[[language]]
id = "typescript"
extensions = ["ts", "tsx", "mts", "cts"]
root_markers = ["tsconfig.json", "package.json"]
command = "typescript-language-server"
args = ["--stdio"]
"#,
        );
        let cfg = LspConfig::load(global.path(), None).unwrap();
        assert_eq!(
            cfg.language_for_extension("tsx").map(|e| e.id.as_str()),
            Some("typescript")
        );
        assert_eq!(cfg.language_for_extension("mjs"), None);
    }

    #[test]
    fn env_map_parses() {
        let global = write_toml(
            r#"
[[language]]
id = "rust"
extensions = ["rs"]
root_markers = ["Cargo.toml"]
command = "rust-analyzer"
env = { RUST_LOG = "warn" }
"#,
        );
        let cfg = LspConfig::load(global.path(), None).unwrap();
        assert_eq!(
            cfg.languages[0].env.get("RUST_LOG").map(String::as_str),
            Some("warn")
        );
    }

    #[test]
    fn missing_required_field_errors() {
        let global = write_toml(
            r#"
[[language]]
id = "rust"
"#,
        );
        let err = LspConfig::load(global.path(), None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("missing field") || msg.contains("invalid type"),
            "error must surface the missing-field reason: {msg}"
        );
    }
}
