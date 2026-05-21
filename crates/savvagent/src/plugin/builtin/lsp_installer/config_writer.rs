//! Merge installed entries into `~/.savvagent/lsp.toml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::plugin::builtin::lsp_installer::catalog::{CatalogEntry, CommandTemplate};
use crate::plugin::builtin::lsp_installer::installer::InstallOutcome;

/// A single `[[language]]` table in `lsp.toml`. Mirrors
/// `tool_lsp::config::LanguageEntry` field-for-field, declared here so
/// savvagent doesn't need a dep on tool_lsp (and so this writer is
/// resilient if the tool_lsp shape evolves — we'd update this mirror
/// at the same time).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LanguageEntry {
    /// Stable id (matches `LspEntryTemplate::id` and tool_lsp's lookup
    /// key).
    pub id: String,
    /// File extensions (no leading dot).
    pub extensions: Vec<String>,
    /// Root marker filenames.
    pub root_markers: Vec<String>,
    /// Executable to launch (resolved via `$PATH` unless absolute).
    pub command: String,
    /// Arguments passed to `command`.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables for the spawned language server.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// In-memory shape of `lsp.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LspConfig {
    /// One entry per configured language. Order in the file is
    /// preserved by [`merge_into_user_config`]: existing entries stay
    /// in place; new ones append at the bottom.
    #[serde(default, rename = "language")]
    pub languages: Vec<LanguageEntry>,
}

/// Read `path` (treating ENOENT as empty), upsert each
/// (catalog-entry, install-outcome) pair into the resulting [`LspConfig`],
/// then write back atomically (write-to-temp + rename).
///
/// Upsert semantics:
/// - An existing entry with the same `id` is **replaced wholesale**
///   (matching tool_lsp's repo-replaces-global behaviour, applied
///   here to installer-vs-user).
/// - A new entry is appended at the bottom.
/// - Unrelated entries (different ids) are preserved in their original
///   order.
///
/// The `command` field is `outcome.installed_at` when the catalog
/// template's `command` is [`CommandTemplate::Installed`], otherwise the
/// literal string from [`CommandTemplate::Literal`] (typical for npm
/// entries where npm puts the binary on `$PATH`).
pub async fn merge_into_user_config(
    path: &Path,
    upserts: &[(&CatalogEntry, &InstallOutcome)],
) -> std::io::Result<()> {
    let mut cfg = match tokio::fs::read_to_string(path).await {
        Ok(text) => toml::from_str::<LspConfig>(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => LspConfig::default(),
        Err(e) => return Err(e),
    };

    for (entry, outcome) in upserts {
        let tmpl = entry.lsp_entry;
        let command = match tmpl.command {
            CommandTemplate::Installed => outcome.installed_at.to_string_lossy().into_owned(),
            CommandTemplate::Literal(s) => s.to_string(),
        };
        let new_entry = LanguageEntry {
            id: tmpl.id.to_string(),
            extensions: tmpl.extensions.iter().map(|s| (*s).to_string()).collect(),
            root_markers: tmpl.root_markers.iter().map(|s| (*s).to_string()).collect(),
            command,
            args: tmpl.args.iter().map(|s| (*s).to_string()).collect(),
            env: std::collections::HashMap::new(),
        };
        if let Some(existing) = cfg.languages.iter_mut().find(|l| l.id == new_entry.id) {
            *existing = new_entry;
        } else {
            cfg.languages.push(new_entry);
        }
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let body = toml::to_string_pretty(&cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_atomic(path, body.as_bytes()).await
}

async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = PathBuf::from(dir).join(format!(".lsp.toml.savvagent.{}.tmp", std::process::id()));
    tokio::fs::write(&tmp, bytes).await?;
    // If rename fails (cross-device, EACCES, Windows lock), the temp
    // file would otherwise linger forever. Best-effort cleanup before
    // propagating the original error.
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::builtin::lsp_installer::catalog::{InstallMethod, LspEntryTemplate, Target};

    fn fake_binary_entry() -> CatalogEntry {
        CatalogEntry {
            id: "fakelsp",
            display_name: "fakelsp",
            language_label: "fake",
            version: "1.0.0",
            method: InstallMethod::BinaryDownload {
                urls: &[(Target::LinuxX86_64Gnu, "https://example.test/x.gz", "0")],
                binary_path: "fakelsp",
            },
            lsp_entry: LspEntryTemplate {
                id: "fake",
                extensions: &["fake"],
                root_markers: &["fake.toml"],
                command: CommandTemplate::Installed,
                args: &[],
            },
        }
    }

    #[tokio::test]
    async fn creates_file_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("subdir").join("lsp.toml");
        let entry = fake_binary_entry();
        let outcome = InstallOutcome {
            entry_id: entry.id.into(),
            installed_at: PathBuf::from("/opt/fakelsp/fakelsp"),
        };
        merge_into_user_config(&path, &[(&entry, &outcome)])
            .await
            .unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("[[language]]"));
        assert!(written.contains("id = \"fake\""));
        assert!(written.contains("command = \"/opt/fakelsp/fakelsp\""));
    }

    #[tokio::test]
    async fn upsert_replaces_existing_entry_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lsp.toml");
        std::fs::write(
            &path,
            r#"
[[language]]
id = "fake"
extensions = ["old"]
root_markers = ["old.toml"]
command = "/old/path"
args = ["--old"]
"#,
        )
        .unwrap();
        let entry = fake_binary_entry();
        let outcome = InstallOutcome {
            entry_id: entry.id.into(),
            installed_at: PathBuf::from("/new/path/fakelsp"),
        };
        merge_into_user_config(&path, &[(&entry, &outcome)])
            .await
            .unwrap();
        let cfg: LspConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.languages.len(), 1, "must replace, not append");
        let only = &cfg.languages[0];
        assert_eq!(only.extensions, vec!["fake".to_string()]);
        assert_eq!(only.command, "/new/path/fakelsp");
        assert!(only.args.is_empty(), "args must be replaced, not merged");
    }

    #[tokio::test]
    async fn upsert_preserves_unrelated_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lsp.toml");
        std::fs::write(
            &path,
            r#"
[[language]]
id = "go"
extensions = ["go"]
root_markers = ["go.mod"]
command = "gopls"
"#,
        )
        .unwrap();
        let entry = fake_binary_entry();
        let outcome = InstallOutcome {
            entry_id: entry.id.into(),
            installed_at: PathBuf::from("/opt/fakelsp/fakelsp"),
        };
        merge_into_user_config(&path, &[(&entry, &outcome)])
            .await
            .unwrap();
        let cfg: LspConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.languages.len(), 2);
        let ids: Vec<&str> = cfg.languages.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"go"));
        assert!(ids.contains(&"fake"));
    }

    #[tokio::test]
    async fn literal_command_template_is_passed_through() {
        let mut entry = fake_binary_entry();
        entry.lsp_entry = LspEntryTemplate {
            id: "fake",
            extensions: &["fake"],
            root_markers: &["fake.toml"],
            command: CommandTemplate::Literal("fakelsp-on-path"),
            args: &["--stdio"],
        };
        let outcome = InstallOutcome {
            entry_id: entry.id.into(),
            installed_at: PathBuf::from("/unused"),
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lsp.toml");
        merge_into_user_config(&path, &[(&entry, &outcome)])
            .await
            .unwrap();
        let cfg: LspConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.languages[0].command, "fakelsp-on-path");
        assert_eq!(cfg.languages[0].args, vec!["--stdio".to_string()]);
    }
}
