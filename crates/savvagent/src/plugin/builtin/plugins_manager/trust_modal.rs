//! `plugins.trust-modal` — confirm an external-plugin install.
//!
//! Pushed by `install::install` once the staged plugin tree is on disk
//! and hashed. The user sees the id, display name, version, source URL,
//! and the first 16 hex characters of the SHA-256 tree hash, then chooses:
//!
//! - **Enter** → move the staged directory into
//!   `~/.savvagent/plugins/<id>/`, write a [`TrustFile`] record, and
//!   `PushNote` a success message.
//! - **Esc**   → delete the staged directory and `PushNote` a cancel
//!   message.
//!
//! The screen owns the staging directory while it's open — neither the
//! installer nor the runtime cleans it up — so closing the screen via
//! either path is the only way the tempdir disappears. (An OS-level
//! tempdir reaper will eventually clean up an orphaned staging dir if
//! the process is killed mid-prompt.)

use std::path::PathBuf;

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};
use savvagent_plugin_wasm::trust::TrustFile;

/// Per-open instance of the trust-prompt modal.
pub(crate) struct PluginsTrustModal {
    id: String,
    name: String,
    version: String,
    source_url: String,
    hash: String,
    staging_dir: PathBuf,
    home_dir: PathBuf,
}

impl PluginsTrustModal {
    /// Build a modal from the install payload.
    pub(crate) fn new(
        id: String,
        name: String,
        version: String,
        source_url: String,
        hash: String,
        staging_dir: PathBuf,
        home_dir: PathBuf,
    ) -> Self {
        Self {
            id,
            name,
            version,
            source_url,
            hash,
            staging_dir,
            home_dir,
        }
    }

    /// Confirm — move staging dir to `~/.savvagent/plugins/<id>/`, write
    /// trust record, and emit a success note.
    fn confirm(&mut self) -> Vec<Effect> {
        let dest_parent = self.home_dir.join(".savvagent/plugins");
        if let Err(e) = std::fs::create_dir_all(&dest_parent) {
            return vec![
                Effect::CloseScreen,
                push_note(format!(
                    "/plugins install: could not create {}: {e}",
                    dest_parent.display()
                )),
            ];
        }
        let dest = dest_parent.join(&self.id);
        // If the destination already exists, reject rather than clobbering
        // someone else's install. The user can `/plugins remove` first.
        if dest.exists() {
            // Clean up staging since we're aborting.
            let _ = std::fs::remove_dir_all(&self.staging_dir);
            return vec![
                Effect::CloseScreen,
                push_note(format!(
                    "/plugins install: {} already installed; use /plugins remove first",
                    self.id
                )),
            ];
        }
        // Move staging → dest. `rename` is atomic on the same filesystem
        // (the OS tempdir is typically the same FS as $HOME on Linux/macOS);
        // if the rename fails (e.g. cross-device on a Linux box where
        // /tmp is a separate mount), fall back to a copy + delete.
        if let Err(rename_err) = std::fs::rename(&self.staging_dir, &dest) {
            if let Err(copy_err) = copy_dir_recursive(&self.staging_dir, &dest) {
                return vec![
                    Effect::CloseScreen,
                    push_note(format!(
                        "/plugins install: move {} → {} failed ({rename_err}; copy fallback: {copy_err})",
                        self.staging_dir.display(),
                        dest.display(),
                    )),
                ];
            }
            let _ = std::fs::remove_dir_all(&self.staging_dir);
        }

        // Write trust record. Failure here means the bytes are on disk
        // but the user hasn't been recorded as trusting them — that's OK,
        // the next launch will surface them as untrusted and ask again.
        let mut tf = match TrustFile::load(&self.home_dir) {
            Ok(t) => t,
            Err(e) => {
                return vec![
                    Effect::CloseScreen,
                    push_note(format!(
                        "/plugins install: copied bytes but could not load trust file ({e}); next launch will prompt again",
                    )),
                ];
            }
        };
        tf.trust(&self.id, self.hash.clone(), Some(self.source_url.clone()));
        if let Err(e) = tf.save(&self.home_dir) {
            return vec![
                Effect::CloseScreen,
                push_note(format!(
                    "/plugins install: copied bytes but could not save trust file ({e}); next launch will prompt again",
                )),
            ];
        }

        vec![
            Effect::CloseScreen,
            push_note(format!(
                "/plugins install: {} installed; restart savvagent to load it",
                self.id
            )),
        ]
    }

    /// Cancel — delete the staging directory and emit a cancel note.
    fn cancel(&mut self) -> Vec<Effect> {
        let _ = std::fs::remove_dir_all(&self.staging_dir);
        vec![
            Effect::CloseScreen,
            push_note(format!("/plugins install: {} cancelled", self.id)),
        ]
    }
}

#[async_trait]
impl Screen for PluginsTrustModal {
    fn id(&self) -> String {
        "plugins.trust-modal".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        let short_hash: String = self.hash.chars().take(16).collect();
        let warn = StyledLine {
            spans: vec![StyledSpan {
                text: rust_i18n::t!("picker.plugins-trust-modal.warning").to_string(),
                fg: Some(ThemeColor::Warning),
                bg: None,
                modifiers: TextMods {
                    bold: true,
                    ..Default::default()
                },
            }],
        };
        vec![
            warn,
            StyledLine::plain(""),
            StyledLine::plain(format!("  id      : {}", self.id)),
            StyledLine::plain(format!("  name    : {}", self.name)),
            StyledLine::plain(format!("  version : {}", self.version)),
            StyledLine::plain(format!("  source  : {}", self.source_url)),
            StyledLine::plain(format!("  hash    : {short_hash}…")),
            StyledLine::plain(format!("  staging : {}", self.staging_dir.display())),
            StyledLine::plain(""),
            StyledLine::plain(rust_i18n::t!("picker.plugins-trust-modal.tips").to_string()),
        ]
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            KeyCodePortable::Enter => Ok(self.confirm()),
            KeyCodePortable::Esc => Ok(self.cancel()),
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            rust_i18n::t!("picker.plugins-trust-modal.tips").to_string(),
        )]
    }
}

fn push_note(text: impl Into<String>) -> Effect {
    Effect::PushNote {
        line: StyledLine::plain(text.into()),
    }
}

/// Best-effort recursive copy used when `std::fs::rename` fails with
/// cross-device. Walks the source tree depth-first, mirroring file
/// contents and creating directories as needed. Used only by
/// [`PluginsTrustModal::confirm`]; not exposed publicly.
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::KeyMods;

    fn key(code: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code,
            modifiers: KeyMods::default(),
        }
    }

    fn stage_fake_plugin(staging: &std::path::Path) {
        std::fs::create_dir_all(staging).unwrap();
        std::fs::write(staging.join("plugin.toml"), b"x = 1").unwrap();
        std::fs::write(staging.join("plugin.wasm"), b"\0asm").unwrap();
    }

    #[tokio::test]
    async fn confirm_moves_staging_writes_trust_and_closes() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let staging = tmp.path().join("staging-acme.demo");
        stage_fake_plugin(&staging);

        let mut modal = PluginsTrustModal::new(
            "acme.demo".into(),
            "Acme Demo".into(),
            "0.1.0".into(),
            "https://example.com/plugin.toml".into(),
            "abc123def456789012345".into(),
            staging.clone(),
            home.to_path_buf(),
        );
        let effs = modal.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
        // Staging directory must be gone and the plugin must be in $HOME.
        assert!(!staging.exists(), "staging should be moved/removed");
        let installed_dir = home.join(".savvagent/plugins/acme.demo");
        assert!(installed_dir.is_dir(), "installed dir missing");
        assert!(installed_dir.join("plugin.toml").is_file());
        assert!(installed_dir.join("plugin.wasm").is_file());
        // Trust record written.
        let tf = TrustFile::load(home).unwrap();
        let rec = tf
            .plugins
            .get("acme.demo")
            .expect("trust record for acme.demo");
        assert!(rec.trusted);
        assert_eq!(rec.sha256_tree, "abc123def456789012345");
        assert_eq!(
            rec.source_url.as_deref(),
            Some("https://example.com/plugin.toml")
        );
    }

    #[tokio::test]
    async fn cancel_removes_staging_and_writes_no_trust() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let staging = tmp.path().join("staging-acme.demo");
        stage_fake_plugin(&staging);

        let mut modal = PluginsTrustModal::new(
            "acme.demo".into(),
            "Acme Demo".into(),
            "0.1.0".into(),
            "https://example.com/plugin.toml".into(),
            "abc123".into(),
            staging.clone(),
            home.to_path_buf(),
        );
        let effs = modal.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
        assert!(!staging.exists(), "staging should be deleted");
        assert!(
            !home.join(".savvagent/plugins/acme.demo").exists(),
            "no install should happen on cancel"
        );
        let tf = TrustFile::load(home).unwrap();
        assert!(
            !tf.plugins.contains_key("acme.demo"),
            "no trust record on cancel"
        );
    }

    #[tokio::test]
    async fn confirm_rejects_existing_install() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let staging = tmp.path().join("staging-acme.demo");
        stage_fake_plugin(&staging);
        // Pre-create the destination.
        std::fs::create_dir_all(home.join(".savvagent/plugins/acme.demo")).unwrap();

        let mut modal = PluginsTrustModal::new(
            "acme.demo".into(),
            "Acme Demo".into(),
            "0.1.0".into(),
            "https://example.com/plugin.toml".into(),
            "abc123".into(),
            staging.clone(),
            home.to_path_buf(),
        );
        let effs = modal.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
        // Staging cleaned up; no trust record written; PushNote complains.
        assert!(!staging.exists());
        let tf = TrustFile::load(home).unwrap();
        assert!(!tf.plugins.contains_key("acme.demo"));
        match &effs[1] {
            Effect::PushNote { line } => {
                let joined: String = line.spans.iter().map(|s| s.text.clone()).collect();
                assert!(
                    joined.contains("already installed"),
                    "expected 'already installed' note, got: {joined}"
                );
            }
            other => panic!("expected PushNote, got {other:?}"),
        }
    }

    #[test]
    fn render_includes_truncated_hash_and_url() {
        let modal = PluginsTrustModal::new(
            "acme.demo".into(),
            "Acme Demo".into(),
            "0.1.0".into(),
            "https://example.com/plugin.toml".into(),
            "0123456789abcdef0123456789abcdef".into(),
            PathBuf::from("/tmp/staging"),
            PathBuf::from("/home/test"),
        );
        let lines = modal.render(Region {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        // First 16 chars of the hash, followed by an ellipsis.
        assert!(
            joined.contains("0123456789abcdef…"),
            "expected truncated hash, got: {joined}"
        );
        assert!(joined.contains("https://example.com/plugin.toml"));
        assert!(joined.contains("acme.demo"));
    }
}
