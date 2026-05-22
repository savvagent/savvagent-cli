//! `LspProgressScreen` — modal that owns the install-driver task and
//! renders per-entry status from shared [`ProgressState`].
//!
//! See `docs/superpowers/specs/2026-05-21-lsp-installer-progress-design.md`.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine,
};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

use crate::plugin::builtin::lsp_installer::progress::{
    EntryProgress, EntryStatus, ProgressState, initial_state_for_ids,
};

/// Render bridge between the install-driver task and the user. Reads
/// [`ProgressState`] each frame; emits `CloseScreen` (+ summary notes)
/// on user dismiss.
pub struct LspProgressScreen {
    pub(crate) state: Arc<TokioMutex<ProgressState>>,
}

impl LspProgressScreen {
    /// Build the screen and (if there's any work to do) spawn the
    /// install driver. `entry_ids` is the picker's confirmed selection.
    pub fn new(entry_ids: Vec<String>) -> Self {
        let mut initial = initial_state_for_ids(&entry_ids);

        // Resolve the "ambient" install context the same way
        // handle_install does. Any failure here marks the state as
        // finished with a single explanatory failure row and we don't
        // spawn the driver task.
        let target = crate::plugin::builtin::lsp_installer::catalog::Target::current();
        let home = dirs::home_dir();
        let downloader_opt =
            crate::plugin::builtin::lsp_installer::installer::ReqwestDownloader::new();

        let (has_target, has_home) = (target.is_some(), home.is_some());
        let (target, home, downloader) = match (target, home, downloader_opt) {
            (Some(t), Some(h), Some(d)) => (t, h, d),
            _ => {
                let reason = match (has_target, has_home) {
                    (false, _) => "this host's target is not supported by the installer",
                    (_, false) => "could not resolve $HOME",
                    _ => "could not build the HTTP client",
                };
                for entry in initial.entries.iter_mut() {
                    if matches!(entry.status, EntryStatus::Queued) {
                        entry.status = EntryStatus::Failed {
                            reason: reason.to_string(),
                            fatal: false,
                        };
                    }
                }
                initial.finished = true;
                return Self {
                    state: Arc::new(TokioMutex::new(initial)),
                };
            }
        };

        let lsp_bin_root = home.join(".savvagent").join("lsp-bin");
        let lsp_toml = home.join(".savvagent").join("lsp.toml");

        // Resolve queued ids → static catalog refs. (Already-failed
        // entries from initial_state_for_ids stay as they are.)
        let entries: Vec<&'static crate::plugin::builtin::lsp_installer::catalog::CatalogEntry> =
            initial
                .entries
                .iter()
                .filter(|e| matches!(e.status, EntryStatus::Queued))
                .filter_map(|e| {
                    crate::plugin::builtin::lsp_installer::catalog::CATALOG
                        .iter()
                        .find(|c| c.id == e.id)
                })
                .collect();

        // Nothing to do — every selected id was unknown (or none
        // selected). Mark the state finished synchronously BEFORE
        // wrapping in the Arc so the empty-entries path doesn't depend
        // on a tokio runtime being present (this is what
        // `id_is_lsp_installer_progress` exercises as a plain #[test]).
        if entries.is_empty() {
            initial.finished = true;
            return Self {
                state: Arc::new(TokioMutex::new(initial)),
            };
        }

        let state = Arc::new(TokioMutex::new(initial));

        let downloader: Arc<dyn crate::plugin::builtin::lsp_installer::installer::Downloader> =
            Arc::new(downloader);
        let npm: Arc<dyn crate::plugin::builtin::lsp_installer::installer::NpmRunner> =
            Arc::new(crate::plugin::builtin::lsp_installer::installer::SystemNpmRunner);

        // Drop the JoinHandle — the screen polls `state` for progress
        // instead of awaiting the task. The task completes when it
        // sets `state.finished = true`.
        drop(
            crate::plugin::builtin::lsp_installer::progress::spawn_driver(
                entries,
                target,
                lsp_bin_root,
                lsp_toml,
                Arc::clone(&state),
                downloader,
                npm,
            ),
        );

        Self { state }
    }

    fn render_lines(state: &ProgressState) -> Vec<StyledLine> {
        let mut out: Vec<StyledLine> = Vec::new();
        out.push(StyledLine::plain(format!(
            "Installing {} language server(s)…",
            state.entries.len()
        )));
        out.push(StyledLine::plain(""));

        for entry in &state.entries {
            let (glyph, label) = format_entry(entry);
            out.push(StyledLine::plain(format!(
                "  {glyph}  {:<32} {label}",
                entry.display_name
            )));
        }

        out.push(StyledLine::plain(""));
        out.push(StyledLine::plain(summary_line(&state.entries)));

        if state.finished {
            let installed = state
                .entries
                .iter()
                .filter(|e| matches!(e.status, EntryStatus::Installed { .. }))
                .count();
            let failed = state
                .entries
                .iter()
                .filter(|e| matches!(e.status, EntryStatus::Failed { .. }))
                .count();
            out.push(StyledLine::plain(""));
            out.push(StyledLine::plain(format!(
                "All done — {installed} installed, {failed} failed."
            )));
            if installed > 0 {
                out.push(StyledLine::plain(
                    "Press Enter to close. Restart savvagent to pick up the new servers.",
                ));
            } else {
                out.push(StyledLine::plain("Press Enter to close."));
            }
            if let Some(err) = &state.config_error {
                out.push(StyledLine::plain(format!(
                    "Warning: writing lsp.toml failed: {err}"
                )));
            }
        }

        out
    }
}

#[async_trait]
impl Screen for LspProgressScreen {
    fn id(&self) -> String {
        "lsp_installer.progress".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        // try_lock keeps the render hot path non-blocking — if the
        // driver task is mid-write we skip this frame; the next frame
        // will catch up. This matches the convention in
        // ChangelogScreen::render and SelfUpdatePlugin::render_slot.
        let state = match self.state.try_lock() {
            Ok(g) => g,
            Err(_) => return vec![],
        };
        Self::render_lines(&state)
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        let state = self.state.lock().await;
        match key.code {
            KeyCodePortable::Enter if state.finished => {
                Ok(vec![Effect::Stack(close_and_summary(&state))])
            }
            KeyCodePortable::Enter => Ok(vec![]),
            KeyCodePortable::Esc if state.finished => Ok(vec![Effect::CloseScreen]),
            KeyCodePortable::Esc => Ok(vec![Effect::Stack(vec![
                Effect::CloseScreen,
                Effect::PushNote {
                    line: StyledLine::plain(
                        "[lsp-installer] still installing in the background \u{2014} results will appear when done"
                            .to_string(),
                    ),
                },
            ])]),
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            "Esc dismisses (install continues in background).",
        )]
    }
}

fn format_entry(entry: &EntryProgress) -> (&'static str, String) {
    match &entry.status {
        EntryStatus::Queued => ("⋯", "queued".to_string()),
        EntryStatus::Downloading {
            bytes_so_far,
            total,
        } => {
            let label = match total {
                Some(t) => format!(
                    "downloading… {} / {}",
                    human_mb(*bytes_so_far),
                    human_mb(*t)
                ),
                None => format!("downloading… {}", human_mb(*bytes_so_far)),
            };
            ("●", label)
        }
        EntryStatus::Verifying => ("●", "verifying SHA256…".to_string()),
        EntryStatus::Extracting => ("●", "extracting…".to_string()),
        EntryStatus::RunningNpm { last_line } => {
            ("●", format!("running npm…   {}", truncate(last_line, 48)))
        }
        EntryStatus::Installed { .. } => ("✓", "installed".to_string()),
        EntryStatus::Failed {
            reason,
            fatal: true,
        } => ("✗", reason.clone()),
        EntryStatus::Failed {
            reason,
            fatal: false,
        } => ("✗", format!("failed: {reason}")),
    }
}

fn summary_line(entries: &[EntryProgress]) -> String {
    let total = entries.len();
    let done = entries
        .iter()
        .filter(|e| {
            matches!(
                e.status,
                EntryStatus::Installed { .. } | EntryStatus::Failed { .. }
            )
        })
        .count();
    let in_progress = entries
        .iter()
        .filter(|e| {
            matches!(
                e.status,
                EntryStatus::Downloading { .. }
                    | EntryStatus::Verifying
                    | EntryStatus::Extracting
                    | EntryStatus::RunningNpm { .. }
            )
        })
        .count();
    let queued = entries
        .iter()
        .filter(|e| matches!(e.status, EntryStatus::Queued))
        .count();
    format!("{done} of {total} done · {in_progress} in progress · {queued} queued")
}

fn human_mb(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    format!("{mb:.1} MB")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn close_and_summary(state: &ProgressState) -> Vec<Effect> {
    let mut effs: Vec<Effect> = Vec::with_capacity(1 + state.entries.len() + 2);
    effs.push(Effect::CloseScreen);
    for entry in &state.entries {
        let line = match &entry.status {
            EntryStatus::Installed { installed_at } => format!(
                "[lsp-installer] {}: installed at {}",
                entry.id,
                installed_at.display()
            ),
            EntryStatus::Failed { reason, fatal } => {
                let prefix = if *fatal { "batch aborted" } else { "failed" };
                format!("[lsp-installer] {}: {prefix} \u{2014} {reason}", entry.id)
            }
            other => format!(
                "[lsp-installer] {}: ended in unexpected state {other:?}",
                entry.id
            ),
        };
        effs.push(Effect::PushNote {
            line: StyledLine::plain(line),
        });
    }
    if let Some(err) = &state.config_error {
        effs.push(Effect::PushNote {
            line: StyledLine::plain(format!(
                "[lsp-installer] warning: writing lsp.toml failed: {err}"
            )),
        });
    }
    let installed_count = state
        .entries
        .iter()
        .filter(|e| matches!(e.status, EntryStatus::Installed { .. }))
        .count();
    let trailing = if installed_count > 0 {
        "[lsp-installer] done \u{2014} restart savvagent to pick up the new servers"
    } else {
        "[lsp-installer] done"
    };
    effs.push(Effect::PushNote {
        line: StyledLine::plain(trailing.to_string()),
    });
    effs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::builtin::lsp_installer::progress::{EntryProgress, ProgressState};
    use std::path::PathBuf;

    fn screen_with_state(state: ProgressState) -> LspProgressScreen {
        LspProgressScreen {
            state: Arc::new(TokioMutex::new(state)),
        }
    }

    fn rendered(s: &LspProgressScreen) -> String {
        s.render(Region {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        })
        .iter()
        .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
    }

    #[test]
    fn id_is_lsp_installer_progress() {
        let s = LspProgressScreen::new(vec![]);
        assert_eq!(s.id(), "lsp_installer.progress");
    }

    #[tokio::test]
    async fn unknown_only_selection_finishes_immediately() {
        let s = LspProgressScreen::new(vec!["totally-fake-id".to_string()]);
        // With the empty-entries fix in `new`, `finished` is set
        // synchronously before the Arc is built. No tokio task to await.
        let g = s.state.lock().await;
        assert!(
            g.finished,
            "unknown-only selection must finish synchronously"
        );
        assert!(
            matches!(g.entries[0].status, EntryStatus::Failed { .. }),
            "unknown id must land as Failed, got {:?}",
            g.entries[0].status
        );
    }

    #[test]
    fn render_shows_queued_label() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "rust-analyzer".into(),
                display_name: "rust-analyzer".into(),
                status: EntryStatus::Queued,
            }],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("rust-analyzer"));
        assert!(out.contains("queued"));
    }

    #[test]
    fn render_shows_downloading_with_bytes_and_total() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "x".into(),
                display_name: "x".into(),
                status: EntryStatus::Downloading {
                    bytes_so_far: 12 * 1024 * 1024 + 400 * 1024,
                    total: Some(28 * 1024 * 1024),
                },
            }],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("downloading"));
        // Spec format: "downloading… 12.4 MB / 28.0 MB"
        assert!(out.contains("MB"));
        assert!(out.contains("/"));
    }

    #[test]
    fn render_shows_downloading_without_total_when_unknown() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "x".into(),
                display_name: "x".into(),
                status: EntryStatus::Downloading {
                    bytes_so_far: 1024 * 1024,
                    total: None,
                },
            }],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("downloading"));
        assert!(out.contains("MB"));
        assert!(
            !out.contains(" / "),
            "no slash when total is unknown, got:\n{out}"
        );
    }

    #[test]
    fn render_shows_running_npm_with_last_line() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "x".into(),
                display_name: "x".into(),
                status: EntryStatus::RunningNpm {
                    last_line: "added 5 packages in 3s".into(),
                },
            }],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("running npm"));
        assert!(out.contains("added 5 packages in 3s"));
    }

    #[test]
    fn render_shows_installed_and_failed() {
        let s = screen_with_state(ProgressState {
            entries: vec![
                EntryProgress {
                    id: "ok".into(),
                    display_name: "ok".into(),
                    status: EntryStatus::Installed {
                        installed_at: PathBuf::from("/tmp/ok"),
                    },
                },
                EntryProgress {
                    id: "bad".into(),
                    display_name: "bad".into(),
                    status: EntryStatus::Failed {
                        reason: "network down".into(),
                        fatal: false,
                    },
                },
            ],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("installed"));
        assert!(out.contains("failed"));
        assert!(out.contains("network down"));
    }

    #[test]
    fn render_summary_counts_by_status() {
        let s = screen_with_state(ProgressState {
            entries: vec![
                EntryProgress {
                    id: "a".into(),
                    display_name: "a".into(),
                    status: EntryStatus::Installed {
                        installed_at: PathBuf::from("/x"),
                    },
                },
                EntryProgress {
                    id: "b".into(),
                    display_name: "b".into(),
                    status: EntryStatus::Verifying,
                },
                EntryProgress {
                    id: "c".into(),
                    display_name: "c".into(),
                    status: EntryStatus::Queued,
                },
                EntryProgress {
                    id: "d".into(),
                    display_name: "d".into(),
                    status: EntryStatus::Queued,
                },
            ],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        // Spec format: "1 of 4 done · 1 in progress · 2 queued"
        assert!(out.contains("1 of 4 done"));
        assert!(out.contains("1 in progress"));
        assert!(out.contains("2 queued"));
    }

    #[test]
    fn render_finished_footer_shows_press_enter() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "a".into(),
                display_name: "a".into(),
                status: EntryStatus::Installed {
                    installed_at: PathBuf::from("/x"),
                },
            }],
            finished: true,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("All done"));
        assert!(out.contains("Press Enter"));
        assert!(out.contains("Restart savvagent"));
    }

    #[test]
    fn render_finished_footer_shows_config_error_when_set() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "a".into(),
                display_name: "a".into(),
                status: EntryStatus::Installed {
                    installed_at: PathBuf::from("/x"),
                },
            }],
            finished: true,
            config_error: Some("disk full".into()),
        });
        let out = rendered(&s);
        assert!(out.contains("lsp.toml"));
        assert!(out.contains("disk full"));
    }

    use savvagent_plugin::KeyMods;

    fn key(code: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code,
            modifiers: KeyMods::default(),
        }
    }

    #[tokio::test]
    async fn enter_while_unfinished_is_a_noop() {
        let mut s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "a".into(),
                display_name: "a".into(),
                status: EntryStatus::Verifying,
            }],
            finished: false,
            config_error: None,
        });
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        assert!(
            effs.is_empty(),
            "Enter pre-finish must not close, got {effs:?}"
        );
    }

    #[tokio::test]
    async fn enter_after_finish_closes_and_emits_summary_notes() {
        let mut s = screen_with_state(ProgressState {
            entries: vec![
                EntryProgress {
                    id: "ok".into(),
                    display_name: "ok".into(),
                    status: EntryStatus::Installed {
                        installed_at: PathBuf::from("/tmp/ok"),
                    },
                },
                EntryProgress {
                    id: "bad".into(),
                    display_name: "bad".into(),
                    status: EntryStatus::Failed {
                        reason: "boom".into(),
                        fatal: false,
                    },
                },
            ],
            finished: true,
            config_error: None,
        });
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        // Expect a Stack starting with CloseScreen followed by at least
        // one PushNote per entry.
        match &effs[..] {
            [Effect::Stack(children)] => {
                assert!(matches!(children[0], Effect::CloseScreen));
                assert!(
                    children
                        .iter()
                        .skip(1)
                        .all(|e| matches!(e, Effect::PushNote { .. })),
                    "every post-close effect must be a PushNote, got {children:?}"
                );
                let texts: String = children
                    .iter()
                    .filter_map(|e| match e {
                        Effect::PushNote { line } => Some(
                            line.spans
                                .iter()
                                .map(|s| s.text.as_str())
                                .collect::<String>(),
                        ),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(texts.contains("ok"));
                assert!(texts.contains("bad"));
                assert!(texts.contains("/tmp/ok"));
                assert!(texts.contains("boom"));
            }
            other => panic!("expected single Stack, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn esc_during_install_dismisses_with_background_note() {
        let mut s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "a".into(),
                display_name: "a".into(),
                status: EntryStatus::Downloading {
                    bytes_so_far: 100,
                    total: None,
                },
            }],
            finished: false,
            config_error: None,
        });
        let effs = s.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        match &effs[..] {
            [Effect::Stack(children)] => {
                assert!(matches!(children[0], Effect::CloseScreen));
                let note_text: String = children
                    .iter()
                    .filter_map(|e| match e {
                        Effect::PushNote { line } => Some(
                            line.spans
                                .iter()
                                .map(|s| s.text.as_str())
                                .collect::<String>(),
                        ),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(note_text.to_lowercase().contains("background"));
            }
            other => panic!("expected single Stack, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn esc_after_finish_just_closes() {
        let mut s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "a".into(),
                display_name: "a".into(),
                status: EntryStatus::Installed {
                    installed_at: PathBuf::from("/tmp/a"),
                },
            }],
            finished: true,
            config_error: None,
        });
        let effs = s.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        // No need for the background note when nothing's running.
        // Either a bare CloseScreen or a Stack with CloseScreen first
        // is acceptable; assert there is no PushNote saying "background".
        let body = format!("{effs:?}");
        assert!(
            !body.to_lowercase().contains("background"),
            "no background note after finish, got {effs:?}"
        );
        // Make sure the screen does close.
        assert!(body.contains("CloseScreen"));
    }

    #[test]
    fn render_shows_fatal_checksum_failure_label() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "ra".into(),
                display_name: "ra".into(),
                status: EntryStatus::Failed {
                    reason: "SHA256 mismatch — batch aborted".into(),
                    fatal: true,
                },
            }],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("SHA256 mismatch"));
        assert!(out.contains("batch aborted"));
        assert!(
            !out.contains("failed:"),
            "fatal arm must not use 'failed:' prefix, got:\n{out}"
        );
    }

    #[test]
    fn render_finished_footer_omits_restart_when_nothing_installed() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "a".into(),
                display_name: "a".into(),
                status: EntryStatus::Failed {
                    reason: "network down".into(),
                    fatal: false,
                },
            }],
            finished: true,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("Press Enter to close."));
        assert!(
            !out.contains("Restart savvagent"),
            "no restart suggestion when zero installs succeeded, got:\n{out}"
        );
    }

    #[tokio::test]
    async fn close_and_summary_omits_restart_when_nothing_installed() {
        let mut s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "a".into(),
                display_name: "a".into(),
                status: EntryStatus::Failed {
                    reason: "boom".into(),
                    fatal: false,
                },
            }],
            finished: true,
            config_error: None,
        });
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        let body = format!("{effs:?}");
        assert!(
            !body.contains("restart savvagent"),
            "no restart suggestion when zero installs succeeded, got {body}"
        );
    }

    #[test]
    fn render_shows_downstream_batch_abort_label() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "pyright".into(),
                display_name: "pyright".into(),
                status: EntryStatus::Failed {
                    reason: "batch aborted after SHA mismatch".into(),
                    fatal: true,
                },
            }],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("batch aborted after SHA mismatch"));
        assert!(
            !out.contains("failed:"),
            "downstream-aborted entry must not use 'failed:' prefix, got:\n{out}"
        );
    }
}
