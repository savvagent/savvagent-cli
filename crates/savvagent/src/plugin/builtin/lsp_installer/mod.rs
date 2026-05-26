//! `internal:lsp-installer` — `/lsp` slash command, multi-select picker,
//! and one-shot LSP-binary installer.
//!
//! See `docs/superpowers/specs/2026-05-20-lsp-installer-design.md` and
//! `docs/superpowers/plans/2026-05-20-lsp-installer.md`.

pub mod catalog;
pub mod config_writer;
pub mod installer;
pub mod picker;
pub mod progress;
pub mod progress_screen;
pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec, StyledLine,
};

use screen::LspPickerScreen;

/// Plugin instance exposing `/lsp` and the picker screen.
pub struct LspInstallerPlugin;

impl LspInstallerPlugin {
    /// Construct a new `LspInstallerPlugin`. Stateless; multiple
    /// instances would behave identically (the catalog is `'static`).
    pub fn new() -> Self {
        Self
    }
}

impl Default for LspInstallerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

const PLUGIN_ID: &str = "internal:lsp-installer";

#[async_trait]
impl Plugin for LspInstallerPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "lsp".into(),
            summary: "Install language servers".into(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        contributions.screens = vec![
            ScreenSpec {
                id: "lsp_installer.picker".into(),
                layout: ScreenLayout::CenteredModal {
                    width_pct: 80,
                    height_pct: 80,
                    title: Some("Install language servers".into()),
                },
            },
            ScreenSpec {
                id: "lsp_installer.progress".into(),
                layout: ScreenLayout::CenteredModal {
                    width_pct: 70,
                    height_pct: 60,
                    title: Some("Installing language servers".into()),
                },
            },
        ];

        Manifest {
            id: PluginId::new(PLUGIN_ID).expect("valid built-in id"),
            name: "LSP installer".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Install curated language-server binaries via /lsp".into(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        if name != "lsp" {
            return Err(PluginError::SlashNotHandled(name.into()));
        }
        match args.first().map(String::as_str) {
            None => Ok(vec![Effect::OpenScreen {
                id: "lsp_installer.picker".into(),
                args: ScreenArgs::None,
            }]),
            Some("__install") => self.handle_install(args[1..].to_vec()).await,
            Some(other) => Ok(vec![Effect::PushNote {
                line: StyledLine::plain(format!(
                    "/lsp: unknown sub-command `{other}` — run `/lsp` with no args to open the picker"
                )),
            }]),
        }
    }

    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        match id {
            "lsp_installer.picker" => Ok(Box::new(LspPickerScreen::new())),
            "lsp_installer.progress" => {
                let entry_ids = match args {
                    ScreenArgs::LspInstallProgress { entry_ids } => entry_ids,
                    other => {
                        return Err(PluginError::ScreenNotFound(format!(
                            "lsp_installer.progress: expected ScreenArgs::LspInstallProgress, got {other:?}"
                        )));
                    }
                };
                Ok(Box::new(
                    crate::plugin::builtin::lsp_installer::progress_screen::LspProgressScreen::new(
                        entry_ids,
                    ),
                ))
            }
            other => Err(PluginError::ScreenNotFound(other.into())),
        }
    }
}

impl LspInstallerPlugin {
    /// Run installs for the catalog ids supplied by the picker's
    /// `Confirm` outcome. Sequential awaits; each entry's progress is
    /// logged via `tracing` and a terminal note is pushed for every
    /// success/failure. After all entries complete, the lsp.toml entries
    /// are merged in one write.
    ///
    /// Sequential is intentional for v1: streaming-while-awaiting requires
    /// either new `Effect` plumbing (HostEvent for progress) or an mpsc
    /// channel into the runtime — both out of scope for the first cut.
    async fn handle_install(&self, ids: Vec<String>) -> Result<Vec<Effect>, PluginError> {
        if ids.is_empty() {
            return Ok(vec![]);
        }

        let mut effs: Vec<Effect> = Vec::new();
        let target = match catalog::Target::current() {
            Some(t) => t,
            None => {
                effs.push(push_note(
                    "/lsp: this host's target is not supported by the installer",
                ));
                return Ok(effs);
            }
        };
        let lsp_bin_root = match dirs::home_dir() {
            Some(home) => home.join(".savvagent").join("lsp-bin"),
            None => {
                effs.push(push_note("/lsp: could not resolve $HOME; install aborted"));
                return Ok(effs);
            }
        };
        let lsp_toml = lsp_bin_root
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("lsp.toml");

        // Resolve ids → catalog entries up front; surface unknowns
        // synchronously so a typo doesn't disappear into the install
        // log.
        let mut entries: Vec<&'static catalog::CatalogEntry> = Vec::new();
        for id in &ids {
            match catalog::CATALOG.iter().find(|e| e.id == id) {
                Some(e) => entries.push(e),
                None => effs.push(push_note(format!(
                    "[lsp-installer] skipped: no catalog entry for `{id}`"
                ))),
            }
        }
        if entries.is_empty() {
            return Ok(effs);
        }

        effs.push(push_note(format!(
            "[lsp-installer] installing {} server(s)…",
            entries.len()
        )));

        let downloader = match installer::ReqwestDownloader::new() {
            Some(d) => d,
            None => {
                effs.push(push_note(
                    "[lsp-installer] failed to build HTTP client; install aborted",
                ));
                return Ok(effs);
            }
        };
        let npm = installer::SystemNpmRunner;
        let mut outcomes: Vec<(&'static catalog::CatalogEntry, installer::InstallOutcome)> =
            Vec::new();

        for entry in entries {
            let result = match entry.method {
                catalog::InstallMethod::BinaryDownload { .. } => {
                    installer::install_binary_entry(
                        entry,
                        target,
                        &lsp_bin_root,
                        &downloader,
                        |progress| tracing::info!(?progress, "lsp install"),
                    )
                    .await
                }
                catalog::InstallMethod::NpmGlobal { .. } => {
                    if installer::detect_npm().is_none() {
                        effs.push(push_note(format!(
                            "[lsp-installer] {}: npm not found on $PATH — install Node.js from https://nodejs.org and re-run /lsp",
                            entry.id
                        )));
                        continue;
                    }
                    installer::install_npm_entry(entry, &npm, |progress| {
                        tracing::info!(?progress, "lsp install")
                    })
                    .await
                }
            };
            match result {
                Ok(outcome) => {
                    effs.push(push_note(format!(
                        "[lsp-installer] {}: installed at {}",
                        entry.id,
                        outcome.installed_at.display()
                    )));
                    outcomes.push((entry, outcome));
                }
                Err(installer::InstallError::ChecksumMismatch {
                    entry_id,
                    expected,
                    actual,
                }) => {
                    // Hard stop. A SHA mismatch means either the catalog
                    // is out of date or someone is serving us a different
                    // binary than we expect — security signal, not a
                    // continuable error. Abort the batch before any
                    // remaining entries (which would share the same
                    // network/upstream-trust assumption) execute.
                    effs.push(push_note(format!(
                        "[lsp-installer] {entry_id}: SHA256 mismatch — expected {expected}, got {actual}. Batch aborted; refresh the catalog or report this if it persists."
                    )));
                    return Ok(effs);
                }
                Err(e) => effs.push(push_note(format!(
                    "[lsp-installer] {}: failed — {e}",
                    entry.id
                ))),
            }
        }

        if !outcomes.is_empty() {
            let upserts: Vec<(&catalog::CatalogEntry, &installer::InstallOutcome)> =
                outcomes.iter().map(|(e, o)| (*e, o)).collect();
            if let Err(e) = config_writer::merge_into_user_config(&lsp_toml, &upserts).await {
                effs.push(push_note(format!(
                    "[lsp-installer] config write to {} failed: {e}",
                    lsp_toml.display()
                )));
            } else {
                effs.push(push_note(format!(
                    "[lsp-installer] wrote {} entr{} to {}",
                    outcomes.len(),
                    if outcomes.len() == 1 { "y" } else { "ies" },
                    lsp_toml.display()
                )));
            }
        }

        effs.push(push_note(
            "[lsp-installer] done — restart savvagent to pick up the new servers",
        ));
        Ok(effs)
    }
}

fn push_note(text: impl Into<String>) -> Effect {
    Effect::PushNote {
        line: StyledLine::plain(text.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lsp_with_no_args_opens_picker() {
        let mut p = LspInstallerPlugin::new();
        let effs = p.handle_slash("lsp", vec![]).await.unwrap();
        match &effs[..] {
            [Effect::OpenScreen { id, .. }] => assert_eq!(id, "lsp_installer.picker"),
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_subcommand_pushes_help_note() {
        let mut p = LspInstallerPlugin::new();
        let effs = p.handle_slash("lsp", vec!["bogus".into()]).await.unwrap();
        assert!(matches!(effs.as_slice(), [Effect::PushNote { .. }]));
    }

    #[tokio::test]
    async fn unrelated_slash_returns_slash_not_handled() {
        let mut p = LspInstallerPlugin::new();
        let err = p.handle_slash("not-lsp", vec![]).await.unwrap_err();
        assert!(matches!(err, PluginError::SlashNotHandled(_)));
    }

    #[tokio::test]
    async fn install_with_no_ids_emits_no_effects() {
        let mut p = LspInstallerPlugin::new();
        let effs = p
            .handle_slash("lsp", vec!["__install".into()])
            .await
            .unwrap();
        assert!(effs.is_empty(), "no-op for empty id list");
    }

    #[tokio::test]
    async fn install_with_unknown_id_pushes_skipped_note() {
        let mut p = LspInstallerPlugin::new();
        let effs = p
            .handle_slash("lsp", vec!["__install".into(), "no-such-server".into()])
            .await
            .unwrap();
        assert!(
            effs.iter()
                .any(|e| matches!(e, Effect::PushNote { line } if line
                .spans
                .iter()
                .any(|s| s.text.contains("no-such-server")))),
            "expected a PushNote mentioning the unknown id, got {effs:?}"
        );
    }

    #[test]
    fn manifest_advertises_slash_and_screen() {
        let p = LspInstallerPlugin::new();
        let m = p.manifest();
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == "lsp")
        );
        assert!(
            m.contributions
                .screens
                .iter()
                .any(|s| s.id == "lsp_installer.picker")
        );
    }

    #[test]
    fn manifest_advertises_progress_screen() {
        let p = LspInstallerPlugin::new();
        let m = p.manifest();
        assert!(
            m.contributions
                .screens
                .iter()
                .any(|s| s.id == "lsp_installer.progress"),
            "manifest must list the progress screen, got {:?}",
            m.contributions.screens
        );
    }

    #[test]
    fn create_screen_returns_progress_screen() {
        use savvagent_plugin::ScreenArgs;
        let p = LspInstallerPlugin::new();
        let screen = p
            .create_screen(
                "lsp_installer.progress",
                ScreenArgs::LspInstallProgress { entry_ids: vec![] },
            )
            .expect("create_screen must accept the progress id");
        assert_eq!(screen.id(), "lsp_installer.progress");
    }

    #[test]
    fn create_screen_rejects_wrong_args_variant() {
        use savvagent_plugin::ScreenArgs;
        let p = LspInstallerPlugin::new();
        match p.create_screen("lsp_installer.progress", ScreenArgs::None) {
            Err(err) => {
                let msg = format!("{err:?}");
                assert!(
                    msg.contains("LspInstallProgress"),
                    "error must name the expected variant, got {msg}"
                );
            }
            Ok(_) => panic!("None args must be rejected"),
        }
    }
}
