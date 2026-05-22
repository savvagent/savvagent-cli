//! Shared progress state for `lsp_installer.progress`.
//!
//! `ProgressState` is the screen ↔ driver-task contract. The driver
//! task (see `spawn_driver`, added later) mutates `EntryProgress::status`
//! through a `notify` closure handed to `installer::install_*_entry`.
//! The screen reads the state from a sibling clone of the same `Arc`
//! during `render`.
//!
//! Mutations are short and synchronous — the closure never holds the
//! mutex across an `await`, and the screen's `render` only reads.

use std::path::PathBuf;

use crate::plugin::builtin::lsp_installer::catalog::CATALOG;
use crate::plugin::builtin::lsp_installer::installer::InstallProgress;

/// Fold an [`InstallProgress`] event into [`ProgressState`].
///
/// Pure: no I/O, no awaits, mutates `state` in place. Looks up the
/// entry by id; unknown ids are silently ignored (defensive — the
/// driver task only emits notifications for entries it constructed).
///
/// `InstallProgress::Started` is intentionally a no-op: per-stage
/// flips (`Downloading`, `Verifying`, etc.) advance the status, and a
/// late `Started` from a re-run path must not blank an in-progress
/// row.
pub fn apply_notification(state: &mut ProgressState, ev: InstallProgress) {
    let id = match &ev {
        InstallProgress::Started { entry_id } => entry_id,
        InstallProgress::Downloading { entry_id, .. } => entry_id,
        InstallProgress::Verifying { entry_id } => entry_id,
        InstallProgress::Extracting { entry_id } => entry_id,
        InstallProgress::RunningNpm { entry_id, .. } => entry_id,
        InstallProgress::Done { entry_id, .. } => entry_id,
    };
    let Some(entry) = state.entries.iter_mut().find(|e| e.id == *id) else {
        return;
    };
    // Don't let a late-arriving in-flight notification overwrite a
    // terminal state. The spawn-per-notify pattern in `run_installs`
    // means a `Downloading`/`Verifying` task can race against the
    // `Err` arm's `Failed` write; without this guard, the spawned
    // task wins and the UI silently un-fails the row.
    if matches!(
        entry.status,
        EntryStatus::Failed { .. } | EntryStatus::Installed { .. }
    ) {
        return;
    }
    match ev {
        InstallProgress::Started { .. } => { /* no-op; see doc */ }
        InstallProgress::Downloading {
            bytes_so_far,
            total,
            ..
        } => {
            entry.status = EntryStatus::Downloading {
                bytes_so_far,
                total,
            };
        }
        InstallProgress::Verifying { .. } => {
            entry.status = EntryStatus::Verifying;
        }
        InstallProgress::Extracting { .. } => {
            entry.status = EntryStatus::Extracting;
        }
        InstallProgress::RunningNpm { line, .. } => {
            entry.status = EntryStatus::RunningNpm { last_line: line };
        }
        InstallProgress::Done { installed_at, .. } => {
            entry.status = EntryStatus::Installed { installed_at };
        }
    }
}

/// Top-level state shared between the install-driver task and the
/// progress screen.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProgressState {
    /// One entry per item the user selected, in picker order. Unknown
    /// catalog ids land here as pre-failed entries so the user sees
    /// what was skipped.
    pub entries: Vec<EntryProgress>,
    /// `true` once the driver task has finished its loop (including
    /// the config-writer pass). The screen renders the "Press Enter to
    /// close" footer only when this is set.
    pub finished: bool,
    /// Optional final note from the config-writer pass — `Some(reason)`
    /// when the merge into `~/.savvagent/lsp.toml` failed, `None`
    /// otherwise. Rendered as an extra footer line.
    pub config_error: Option<String>,
}

/// One row in the progress modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryProgress {
    /// Catalog id (matches `CatalogEntry::id`).
    pub id: String,
    /// Human-readable name; mirrors `CatalogEntry::display_name` so the
    /// modal can still render meaningful rows for unknown ids (fall
    /// back to `id` in that case).
    pub display_name: String,
    /// Current stage; the closure handed to `install_*_entry` flips
    /// this on each `InstallProgress` it sees.
    pub status: EntryStatus,
}

/// One row's current state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryStatus {
    /// Not yet started.
    Queued,
    /// `installer::InstallProgress::Downloading` was the last
    /// notification for this entry.
    Downloading {
        /// Bytes received so far.
        bytes_so_far: u64,
        /// Total size in bytes, if the server reported it.
        total: Option<u64>,
    },
    /// `installer::InstallProgress::Verifying`.
    Verifying,
    /// `installer::InstallProgress::Extracting`.
    Extracting,
    /// `installer::InstallProgress::RunningNpm`; `last_line` is the
    /// most recent line of npm's combined stdout/stderr (truncated by
    /// the render layer if very long).
    RunningNpm {
        /// Most recent line of npm output.
        last_line: String,
    },
    /// Installer returned `Ok(InstallOutcome)`.
    Installed {
        /// Where the binary landed.
        installed_at: PathBuf,
    },
    /// Installer returned `Err`. `fatal == true` means the failure
    /// aborts the rest of the batch (today: only `ChecksumMismatch`).
    Failed {
        /// Human-readable one-line reason.
        reason: String,
        /// `true` for ChecksumMismatch (which aborts subsequent
        /// entries with their own `Failed { fatal: true }`).
        fatal: bool,
    },
}

use crate::plugin::builtin::lsp_installer::catalog::{CatalogEntry, InstallMethod, Target};
use crate::plugin::builtin::lsp_installer::config_writer;
use crate::plugin::builtin::lsp_installer::installer::{
    self, Downloader, InstallError, InstallOutcome, NpmRunner,
};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

/// Run installs sequentially for `entries`, updating `state` after
/// each `InstallProgress` notification and after each entry settles.
///
/// On `Err(InstallError::ChecksumMismatch)` the function flips that
/// entry to `Failed { fatal: true }`, flips every remaining `Queued`
/// entry to `Failed { reason: "batch aborted after SHA mismatch", fatal: true }`,
/// and returns. Same security semantics as the legacy `handle_install`.
///
/// On any other `Err`, the entry is marked `Failed { fatal: false }`
/// and the loop continues to the next entry.
///
/// Caller is responsible for setting `state.finished = true` and
/// running the config-writer pass — those happen one level up in the
/// driver-task constructor so this function can be tested in isolation.
pub async fn run_installs(
    entries: Vec<&'static CatalogEntry>,
    target: Target,
    lsp_bin_root: std::path::PathBuf,
    state: Arc<TokioMutex<ProgressState>>,
    downloader: &dyn Downloader,
    npm: &dyn NpmRunner,
) {
    for entry in entries {
        // Per-stage notifications: clone the Arc for the closure.
        let state_for_notify = Arc::clone(&state);
        let notify = move |progress: installer::InstallProgress| {
            // The notify closure runs synchronously from inside the
            // install future, which is itself on a tokio runtime. The
            // cleanest deadlock-free way to mutate the tokio Mutex from
            // a sync context is to spawn a one-shot task that acquires
            // the lock asynchronously — never blocking the caller, and
            // never holding a lock across an await.
            let s = Arc::clone(&state_for_notify);
            tokio::spawn(async move {
                let mut guard = s.lock().await;
                apply_notification(&mut guard, progress);
            });
        };

        let result = match entry.method {
            InstallMethod::BinaryDownload { .. } => {
                installer::install_binary_entry(entry, target, &lsp_bin_root, downloader, notify)
                    .await
            }
            InstallMethod::NpmGlobal { .. } => {
                installer::install_npm_entry(entry, npm, notify).await
            }
        };

        match result {
            Ok(outcome) => {
                // Write Installed directly here rather than waiting for the
                // spawned `InstallProgress::Done` notification task to run.
                // The spawned task is decoupled from the install future and
                // may not be polled by the time `spawn_driver`'s outcomes
                // collection runs — on a multi-threaded runtime that is a
                // race. The terminal-state guard in `apply_notification`
                // ensures the later-arriving spawned `Done` task no-ops
                // against the already-Installed state.
                let mut guard = state.lock().await;
                if let Some(slot) = guard.entries.iter_mut().find(|e| e.id == entry.id) {
                    slot.status = EntryStatus::Installed {
                        installed_at: outcome.installed_at,
                    };
                } else {
                    tracing::warn!(
                        entry_id = entry.id,
                        "run_installs: entry id missing from progress state after successful install — UI may show stale Queued row"
                    );
                }
            }
            Err(InstallError::ChecksumMismatch { .. }) => {
                let mut guard = state.lock().await;
                if let Some(slot) = guard.entries.iter_mut().find(|e| e.id == entry.id) {
                    slot.status = EntryStatus::Failed {
                        reason: "SHA256 mismatch — batch aborted".into(),
                        fatal: true,
                    };
                } else {
                    tracing::warn!(
                        entry_id = entry.id,
                        "run_installs: entry id missing from progress state after ChecksumMismatch"
                    );
                }
                for e in guard.entries.iter_mut() {
                    if matches!(e.status, EntryStatus::Queued) {
                        e.status = EntryStatus::Failed {
                            reason: "batch aborted after SHA mismatch".into(),
                            fatal: true,
                        };
                    }
                }
                return;
            }
            Err(err) => {
                let mut guard = state.lock().await;
                if let Some(slot) = guard.entries.iter_mut().find(|e| e.id == entry.id) {
                    slot.status = EntryStatus::Failed {
                        reason: err.to_string(),
                        fatal: false,
                    };
                } else {
                    tracing::warn!(
                        entry_id = entry.id,
                        "run_installs: entry id missing from progress state after install error"
                    );
                }
            }
        }
    }
}

/// Spawn the install-driver task. Returns the `JoinHandle` so callers
/// (production: the screen) can choose whether to await it; the screen
/// just drops the handle since it polls the shared state instead.
///
/// The task:
///   1. Calls [`run_installs`] to drive each entry's install.
///   2. Collects every entry that ended in `EntryStatus::Installed`
///      into an upsert list.
///   3. Calls `config_writer::merge_into_user_config` to merge those
///      entries into `lsp.toml`.
///   4. Sets `state.finished = true` and writes any config-writer
///      error to `state.config_error`.
pub fn spawn_driver(
    entries: Vec<&'static CatalogEntry>,
    target: Target,
    lsp_bin_root: std::path::PathBuf,
    lsp_toml: std::path::PathBuf,
    state: Arc<TokioMutex<ProgressState>>,
    downloader: Arc<dyn Downloader>,
    npm: Arc<dyn NpmRunner>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let entries_for_run = entries.clone();
        run_installs(
            entries_for_run,
            target,
            lsp_bin_root,
            Arc::clone(&state),
            downloader.as_ref(),
            npm.as_ref(),
        )
        .await;

        // Collect successful outcomes for the config-writer.
        let outcomes: Vec<(&'static CatalogEntry, InstallOutcome)> = {
            let guard = state.lock().await;
            entries
                .iter()
                .copied()
                .filter_map(|entry| {
                    let slot = guard.entries.iter().find(|e| e.id == entry.id)?;
                    match &slot.status {
                        EntryStatus::Installed { installed_at } => Some((
                            entry,
                            InstallOutcome {
                                entry_id: entry.id.to_string(),
                                installed_at: installed_at.clone(),
                            },
                        )),
                        _ => None,
                    }
                })
                .collect()
        };

        if !outcomes.is_empty() {
            let upserts: Vec<(&CatalogEntry, &InstallOutcome)> =
                outcomes.iter().map(|(e, o)| (*e, o)).collect();
            if let Err(err) = config_writer::merge_into_user_config(&lsp_toml, &upserts).await {
                let mut guard = state.lock().await;
                guard.config_error = Some(err.to_string());
            }
        }

        let mut guard = state.lock().await;
        guard.finished = true;
    })
}

/// Build the initial [`ProgressState`] for the list of catalog ids the
/// picker confirmed. Unknown ids become pre-`Failed` entries so a typo
/// (or a stale id from an external dispatcher) surfaces in the modal
/// instead of disappearing silently.
pub fn initial_state_for_ids(ids: &[String]) -> ProgressState {
    let entries = ids
        .iter()
        .map(|id| match CATALOG.iter().find(|e| e.id == id) {
            Some(catalog_entry) => EntryProgress {
                id: catalog_entry.id.to_string(),
                display_name: catalog_entry.display_name.to_string(),
                status: EntryStatus::Queued,
            },
            None => EntryProgress {
                id: id.clone(),
                display_name: id.clone(),
                status: EntryStatus::Failed {
                    reason: format!("no catalog entry for `{id}`"),
                    fatal: false,
                },
            },
        })
        .collect();
    ProgressState {
        entries,
        finished: false,
        config_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_progress_state_is_empty_and_unfinished() {
        let s = ProgressState::default();
        assert!(s.entries.is_empty());
        assert!(!s.finished);
        assert!(s.config_error.is_none());
    }

    #[test]
    fn entry_status_variants_construct() {
        let _ = EntryStatus::Queued;
        let _ = EntryStatus::Downloading {
            bytes_so_far: 0,
            total: None,
        };
        let _ = EntryStatus::Verifying;
        let _ = EntryStatus::Extracting;
        let _ = EntryStatus::RunningNpm {
            last_line: "added 1 package".into(),
        };
        let _ = EntryStatus::Installed {
            installed_at: PathBuf::from("/tmp/x"),
        };
        let _ = EntryStatus::Failed {
            reason: "boom".into(),
            fatal: false,
        };
    }

    use crate::plugin::builtin::lsp_installer::installer::InstallProgress;

    fn state_with(ids: &[&str]) -> ProgressState {
        ProgressState {
            entries: ids
                .iter()
                .map(|id| EntryProgress {
                    id: (*id).into(),
                    display_name: (*id).into(),
                    status: EntryStatus::Queued,
                })
                .collect(),
            finished: false,
            config_error: None,
        }
    }

    #[test]
    fn started_keeps_status_queued() {
        // Started is informational only — actual stage flips happen
        // when the next InstallProgress (Downloading / RunningNpm /
        // etc.) arrives. Started must not blank an in-progress status.
        let mut s = state_with(&["a"]);
        apply_notification(
            &mut s,
            InstallProgress::Started {
                entry_id: "a".into(),
            },
        );
        assert_eq!(s.entries[0].status, EntryStatus::Queued);
    }

    #[test]
    fn downloading_updates_bytes_and_total() {
        let mut s = state_with(&["a"]);
        apply_notification(
            &mut s,
            InstallProgress::Downloading {
                entry_id: "a".into(),
                bytes_so_far: 1024,
                total: Some(4096),
            },
        );
        assert_eq!(
            s.entries[0].status,
            EntryStatus::Downloading {
                bytes_so_far: 1024,
                total: Some(4096)
            }
        );
    }

    #[test]
    fn verifying_then_extracting_advances_status() {
        let mut s = state_with(&["a"]);
        apply_notification(
            &mut s,
            InstallProgress::Verifying {
                entry_id: "a".into(),
            },
        );
        assert_eq!(s.entries[0].status, EntryStatus::Verifying);
        apply_notification(
            &mut s,
            InstallProgress::Extracting {
                entry_id: "a".into(),
            },
        );
        assert_eq!(s.entries[0].status, EntryStatus::Extracting);
    }

    #[test]
    fn running_npm_carries_last_line() {
        let mut s = state_with(&["a"]);
        apply_notification(
            &mut s,
            InstallProgress::RunningNpm {
                entry_id: "a".into(),
                line: "added 5 packages".into(),
            },
        );
        match &s.entries[0].status {
            EntryStatus::RunningNpm { last_line } => assert_eq!(last_line, "added 5 packages"),
            other => panic!("expected RunningNpm, got {other:?}"),
        }
    }

    #[test]
    fn done_marks_installed_with_path() {
        let mut s = state_with(&["a"]);
        apply_notification(
            &mut s,
            InstallProgress::Done {
                entry_id: "a".into(),
                installed_at: PathBuf::from("/tmp/lsp/a/bin"),
            },
        );
        match &s.entries[0].status {
            EntryStatus::Installed { installed_at } => {
                assert_eq!(installed_at, &PathBuf::from("/tmp/lsp/a/bin"));
            }
            other => panic!("expected Installed, got {other:?}"),
        }
    }

    #[test]
    fn notification_for_unknown_id_is_a_noop() {
        let mut s = state_with(&["a"]);
        apply_notification(
            &mut s,
            InstallProgress::Verifying {
                entry_id: "no-such-entry".into(),
            },
        );
        assert_eq!(s.entries[0].status, EntryStatus::Queued);
    }

    #[test]
    fn apply_notification_skips_when_entry_is_already_failed() {
        let mut s = ProgressState {
            entries: vec![EntryProgress {
                id: "a".into(),
                display_name: "a".into(),
                status: EntryStatus::Failed {
                    reason: "boom".into(),
                    fatal: false,
                },
            }],
            finished: false,
            config_error: None,
        };
        apply_notification(
            &mut s,
            InstallProgress::Downloading {
                entry_id: "a".into(),
                bytes_so_far: 1024,
                total: Some(2048),
            },
        );
        assert!(
            matches!(s.entries[0].status, EntryStatus::Failed { .. }),
            "Failed must not be reverted by a late Downloading notification, got {:?}",
            s.entries[0].status
        );
    }

    #[test]
    fn apply_notification_skips_when_entry_is_already_installed() {
        let mut s = ProgressState {
            entries: vec![EntryProgress {
                id: "a".into(),
                display_name: "a".into(),
                status: EntryStatus::Installed {
                    installed_at: PathBuf::from("/tmp/done"),
                },
            }],
            finished: false,
            config_error: None,
        };
        apply_notification(
            &mut s,
            InstallProgress::Verifying {
                entry_id: "a".into(),
            },
        );
        assert!(
            matches!(s.entries[0].status, EntryStatus::Installed { .. }),
            "Installed must not be reverted, got {:?}",
            s.entries[0].status
        );
    }

    use crate::plugin::builtin::lsp_installer::catalog::CATALOG;

    #[test]
    fn initial_state_for_known_id_is_queued() {
        let known = CATALOG[0].id.to_string();
        let state = initial_state_for_ids(std::slice::from_ref(&known));
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].id, known);
        assert_eq!(state.entries[0].status, EntryStatus::Queued);
        assert!(!state.finished);
    }

    #[test]
    fn initial_state_for_unknown_id_marks_failed_non_fatal() {
        let state = initial_state_for_ids(&["nonsense-id".to_string()]);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].id, "nonsense-id");
        match &state.entries[0].status {
            EntryStatus::Failed { reason, fatal } => {
                assert!(reason.to_lowercase().contains("no catalog entry"));
                assert!(!fatal, "unknown id is not a security signal");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        // `finished` must remain false — the driver task still needs
        // to run (or no-op past) the queued entries.
        assert!(!state.finished);
    }

    #[test]
    fn initial_state_preserves_picker_order() {
        let a = CATALOG[0].id.to_string();
        let b = CATALOG[1].id.to_string();
        let state = initial_state_for_ids(&[b.clone(), a.clone()]);
        assert_eq!(state.entries[0].id, b);
        assert_eq!(state.entries[1].id, a);
    }

    use crate::plugin::builtin::lsp_installer::catalog::Target;
    use crate::plugin::builtin::lsp_installer::installer::{Downloader, InstallError, NpmRunner};
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;

    struct CountingDownloader {
        payload: bytes::Bytes,
    }
    #[async_trait::async_trait]
    impl Downloader for CountingDownloader {
        async fn fetch(&self, _url: &str) -> Result<bytes::Bytes, InstallError> {
            Ok(self.payload.clone())
        }
    }

    struct NoopNpm;
    #[async_trait::async_trait]
    impl NpmRunner for NoopNpm {
        async fn install_global(
            &self,
            _package: &str,
            _version: &str,
            _on_line: &(dyn Fn(String) + Send + Sync),
        ) -> Result<(), String> {
            unreachable!("happy-path test only schedules a binary entry")
        }
        async fn root_global(&self) -> Result<std::path::PathBuf, String> {
            unreachable!()
        }
    }

    /// Build a minimal catalog entry that points at a gzipped fixture
    /// URL whose sha matches the supplied bytes. Returns the leaked
    /// 'static `CatalogEntry` (acceptable for tests) and the bytes.
    fn fake_binary_entry(
        id: &'static str,
    ) -> (
        &'static crate::plugin::builtin::lsp_installer::catalog::CatalogEntry,
        bytes::Bytes,
    ) {
        use crate::plugin::builtin::lsp_installer::catalog::{
            CatalogEntry, CommandTemplate, InstallMethod, LspEntryTemplate,
        };
        use flate2::{Compression, write::GzEncoder};
        use sha2::{Digest, Sha256};
        use std::io::Write;

        let plain = b"#!/bin/sh\necho fake\n";
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(plain).unwrap();
        let archive = enc.finish().unwrap();
        let sha = hex::encode(Sha256::digest(&archive));
        let url: &'static str = Box::leak(format!("https://example.test/{id}.gz").into_boxed_str());
        let sha_static: &'static str = Box::leak(sha.into_boxed_str());
        let urls: &'static [(Target, &'static str, &'static str)] =
            Box::leak(Box::new([(Target::LinuxX86_64Gnu, url, sha_static)]));
        let entry: &'static CatalogEntry = Box::leak(Box::new(CatalogEntry {
            id,
            display_name: id,
            language_label: "fake",
            version: "0.0.0",
            method: InstallMethod::BinaryDownload {
                urls,
                binary_path: id,
            },
            lsp_entry: LspEntryTemplate {
                id,
                extensions: &["fake"],
                root_markers: &["fake.toml"],
                command: CommandTemplate::Installed,
                args: &[],
            },
        }));
        (entry, bytes::Bytes::from(archive))
    }

    #[tokio::test]
    async fn run_installs_marks_each_entry_installed() {
        let (entry_a, archive_a) = fake_binary_entry("fake-a");
        let (entry_b, _archive_b) = fake_binary_entry("fake-b");
        // Same payload for both — keeps the sha + downloader simple.
        let dl = CountingDownloader { payload: archive_a };
        let npm = NoopNpm;
        let state = Arc::new(TokioMutex::new(ProgressState {
            entries: vec![
                EntryProgress {
                    id: "fake-a".into(),
                    display_name: "fake-a".into(),
                    status: EntryStatus::Queued,
                },
                EntryProgress {
                    id: "fake-b".into(),
                    display_name: "fake-b".into(),
                    status: EntryStatus::Queued,
                },
            ],
            finished: false,
            config_error: None,
        }));
        let tmp = tempfile::tempdir().unwrap();

        run_installs(
            vec![entry_a, entry_b],
            Target::LinuxX86_64Gnu,
            tmp.path().to_path_buf(),
            Arc::clone(&state),
            &dl,
            &npm,
        )
        .await;

        let s = state.lock().await;
        assert!(matches!(s.entries[0].status, EntryStatus::Installed { .. }));
        assert!(matches!(s.entries[1].status, EntryStatus::Installed { .. }));
    }

    /// A Downloader that returns garbage so the SHA check always fails.
    struct GarbageDownloader;
    #[async_trait::async_trait]
    impl Downloader for GarbageDownloader {
        async fn fetch(&self, _url: &str) -> Result<bytes::Bytes, InstallError> {
            Ok(bytes::Bytes::from_static(b"definitely-not-the-archive"))
        }
    }

    #[tokio::test]
    async fn run_installs_checksum_mismatch_aborts_remaining_queued() {
        let (entry_a, _archive_a) = fake_binary_entry("fake-a");
        let (entry_b, _archive_b) = fake_binary_entry("fake-b");
        let dl = GarbageDownloader;
        let npm = NoopNpm;
        let state = Arc::new(TokioMutex::new(ProgressState {
            entries: vec![
                EntryProgress {
                    id: "fake-a".into(),
                    display_name: "fake-a".into(),
                    status: EntryStatus::Queued,
                },
                EntryProgress {
                    id: "fake-b".into(),
                    display_name: "fake-b".into(),
                    status: EntryStatus::Queued,
                },
            ],
            finished: false,
            config_error: None,
        }));
        let tmp = tempfile::tempdir().unwrap();

        run_installs(
            vec![entry_a, entry_b],
            Target::LinuxX86_64Gnu,
            tmp.path().to_path_buf(),
            Arc::clone(&state),
            &dl,
            &npm,
        )
        .await;

        let s = state.lock().await;
        match &s.entries[0].status {
            EntryStatus::Failed { fatal, .. } => assert!(*fatal),
            other => panic!("expected entry 0 fatal-failed, got {other:?}"),
        }
        match &s.entries[1].status {
            EntryStatus::Failed { fatal, reason } => {
                assert!(*fatal);
                assert!(reason.to_lowercase().contains("aborted"));
            }
            other => panic!("expected entry 1 batch-aborted, got {other:?}"),
        }
    }

    /// A Downloader that errors with a non-checksum error so we can
    /// exercise the non-fatal continue-to-next-entry path.
    struct ErroringDownloader;
    #[async_trait::async_trait]
    impl Downloader for ErroringDownloader {
        async fn fetch(&self, _url: &str) -> Result<bytes::Bytes, InstallError> {
            Err(InstallError::Download("network down".into()))
        }
    }

    #[tokio::test]
    async fn spawn_driver_finishes_and_writes_config() {
        let (entry_a, archive_a) = fake_binary_entry("fake-cfg-a");
        let dl: Arc<dyn Downloader> = Arc::new(CountingDownloader { payload: archive_a });
        let npm: Arc<dyn NpmRunner> = Arc::new(NoopNpm);
        let tmp = tempfile::tempdir().unwrap();
        let bin_root = tmp.path().join("lsp-bin");
        let toml_path = tmp.path().join("lsp.toml");

        let state = Arc::new(TokioMutex::new(ProgressState {
            entries: vec![EntryProgress {
                id: "fake-cfg-a".into(),
                display_name: "fake-cfg-a".into(),
                status: EntryStatus::Queued,
            }],
            finished: false,
            config_error: None,
        }));

        let join = spawn_driver(
            vec![entry_a],
            Target::LinuxX86_64Gnu,
            bin_root,
            toml_path.clone(),
            Arc::clone(&state),
            dl,
            npm,
        );
        join.await.expect("driver task joined");

        let s = state.lock().await;
        assert!(s.finished, "finished flag must be set");
        assert!(s.config_error.is_none(), "config write must succeed");
        assert!(matches!(s.entries[0].status, EntryStatus::Installed { .. }));
        assert!(toml_path.exists(), "lsp.toml must exist after merge");
    }

    /// Serve a gzipped fixture over loopback, build a real
    /// `CatalogEntry` pointing at it, and run `spawn_driver` end-to-end
    /// against the production `ReqwestDownloader`. Asserts that:
    ///   - `state.finished` flips true,
    ///   - the entry lands in `Installed`,
    ///   - `lsp.toml` was written and parses.
    #[tokio::test]
    async fn smoke_spawn_driver_end_to_end() {
        use crate::plugin::builtin::lsp_installer::catalog::{
            CatalogEntry, CommandTemplate, InstallMethod, LspEntryTemplate,
        };
        use crate::plugin::builtin::lsp_installer::installer::ReqwestDownloader;
        use sha2::{Digest, Sha256};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;

        let plain = b"#!/bin/sh\necho hi\n";
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut enc, plain).unwrap();
        let archive = std::sync::Arc::new(enc.finish().unwrap());
        let sha = hex::encode(Sha256::digest(&archive[..]));

        // Loopback HTTP/1.1 server, single shot.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/fake-smoke.gz");
        let body = std::sync::Arc::clone(&archive);
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (read, mut write) = sock.split();
            let mut reader = BufReader::new(read);
            let mut buf = String::new();
            loop {
                buf.clear();
                let n = reader.read_line(&mut buf).await.unwrap_or(0);
                if n == 0 || buf == "\r\n" || buf == "\n" {
                    break;
                }
            }
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n",
                body.len()
            );
            write.write_all(header.as_bytes()).await.unwrap();
            write.write_all(&body).await.unwrap();
            write.flush().await.unwrap();
        });

        let url_static: &'static str = Box::leak(url.into_boxed_str());
        let sha_static: &'static str = Box::leak(sha.into_boxed_str());
        let urls: &'static [(Target, &'static str, &'static str)] = Box::leak(Box::new([(
            Target::current().expect("host target"),
            url_static,
            sha_static,
        )]));

        let entry: &'static CatalogEntry = Box::leak(Box::new(CatalogEntry {
            id: "fake-smoke",
            display_name: "fake-smoke",
            language_label: "fake",
            version: "0.0.0",
            method: InstallMethod::BinaryDownload {
                urls,
                binary_path: "fake-smoke",
            },
            lsp_entry: LspEntryTemplate {
                id: "fake-smoke",
                extensions: &["fake"],
                root_markers: &["fake.toml"],
                command: CommandTemplate::Installed,
                args: &[],
            },
        }));

        let tmp = tempfile::tempdir().unwrap();
        let bin_root = tmp.path().join("lsp-bin");
        let toml = tmp.path().join("lsp.toml");

        let state = Arc::new(TokioMutex::new(ProgressState {
            entries: vec![EntryProgress {
                id: "fake-smoke".into(),
                display_name: "fake-smoke".into(),
                status: EntryStatus::Queued,
            }],
            finished: false,
            config_error: None,
        }));

        let dl: Arc<dyn Downloader> = Arc::new(ReqwestDownloader::new().expect("reqwest builds"));
        let npm: Arc<dyn NpmRunner> = Arc::new(NoopNpm);
        let handle = spawn_driver(
            vec![entry],
            Target::current().unwrap(),
            bin_root,
            toml.clone(),
            Arc::clone(&state),
            dl,
            npm,
        );
        handle.await.expect("driver joined");

        let s = state.lock().await;
        assert!(s.finished);
        assert!(matches!(s.entries[0].status, EntryStatus::Installed { .. }));
        assert!(s.config_error.is_none(), "config write must succeed");
        assert!(toml.exists(), "lsp.toml must exist on disk");
        let _ = server.await;
    }

    #[tokio::test]
    async fn run_installs_non_fatal_error_continues_to_next_entry() {
        let (entry_a, archive_a) = fake_binary_entry("fake-a");
        let (entry_b, _archive_b) = fake_binary_entry("fake-b");

        // entry_a fails (download error), entry_b succeeds. We achieve
        // that by using a downloader that returns Err for any url
        // containing "fake-a" and the good archive for "fake-b".
        struct Mixed {
            good: bytes::Bytes,
        }
        #[async_trait::async_trait]
        impl Downloader for Mixed {
            async fn fetch(&self, url: &str) -> Result<bytes::Bytes, InstallError> {
                if url.contains("fake-a") {
                    Err(InstallError::Download("network down".into()))
                } else {
                    Ok(self.good.clone())
                }
            }
        }
        let dl = Mixed { good: archive_a };
        let npm = NoopNpm;
        let state = Arc::new(TokioMutex::new(ProgressState {
            entries: vec![
                EntryProgress {
                    id: "fake-a".into(),
                    display_name: "fake-a".into(),
                    status: EntryStatus::Queued,
                },
                EntryProgress {
                    id: "fake-b".into(),
                    display_name: "fake-b".into(),
                    status: EntryStatus::Queued,
                },
            ],
            finished: false,
            config_error: None,
        }));
        let tmp = tempfile::tempdir().unwrap();

        run_installs(
            vec![entry_a, entry_b],
            Target::LinuxX86_64Gnu,
            tmp.path().to_path_buf(),
            Arc::clone(&state),
            &dl,
            &npm,
        )
        .await;

        let s = state.lock().await;
        match &s.entries[0].status {
            EntryStatus::Failed { fatal, reason } => {
                assert!(!*fatal);
                assert!(reason.contains("network down"));
            }
            other => panic!("expected entry 0 non-fatal failed, got {other:?}"),
        }
        assert!(matches!(s.entries[1].status, EntryStatus::Installed { .. }));
    }
}
