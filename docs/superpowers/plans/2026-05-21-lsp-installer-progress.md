# LSP installer — live install-progress UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the user live, per-server feedback while `/lsp` is installing language servers, so selecting several at once no longer looks like the TUI has hung.

**Architecture:** The picker hands off to a new modal `lsp_installer.progress` screen. The screen owns an `Arc<Mutex<ProgressState>>` and a `tokio::spawn`'d driver task that runs the existing `install_binary_entry` / `install_npm_entry` futures sequentially. The driver task's notify-callback writes per-stage progress into the shared state; the screen's `render()` reads from it each frame. The TUI main loop already polls events every 50 ms, so progress updates land in the UI within ~50 ms with no new `Effect` / `HostEvent` / `WorkerMsg` plumbing.

**Tech Stack:** Rust, tokio, `savvagent_plugin::Screen`, existing `internal:lsp-installer` plugin, existing `installer::Downloader` + `NpmRunner` traits.

**Spec:** `docs/superpowers/specs/2026-05-21-lsp-installer-progress-design.md`

---

## File Structure

| File | Role |
| ---- | ---- |
| `crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs` (new) | `ProgressState`, `EntryProgress`, `EntryStatus`, the driver task constructor + lifecycle. Pure data + the one `tokio::spawn` call site. |
| `crates/savvagent/src/plugin/builtin/lsp_installer/progress_screen.rs` (new) | `LspProgressScreen` implementing `savvagent_plugin::Screen`. Renders from `ProgressState`; handles Enter / Esc. |
| `crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs` (modify) | Add new screen to `Contributions::screens`; add `create_screen` arm; add `progress` and `progress_screen` to `pub mod` list. |
| `crates/savvagent/src/plugin/builtin/lsp_installer/screen.rs` (modify) | `Confirm` arm emits `OpenScreen { id: "lsp_installer.progress", args: ScreenArgs::LspInstallProgress { entry_ids } }` instead of `RunSlash { name: "lsp", args: ["__install", …] }`. |
| `crates/savvagent-plugin/src/types.rs` (modify) | Add `ScreenArgs::LspInstallProgress { entry_ids: Vec<String> }` and update `screen_id()`. |

`installer.rs` does **not** change — the existing `notify: impl Fn(InstallProgress)` callback shape is exactly the seam we need.

---

### Task 1: Add `ScreenArgs::LspInstallProgress` to the plugin surface

**Files:**
- Modify: `crates/savvagent-plugin/src/types.rs` — `ScreenArgs` enum + `screen_id()` method.
- Test: same file, `mod tests` at the bottom.

- [ ] **Step 1: Add a failing test for the new variant + screen_id mapping**

Open `crates/savvagent-plugin/src/types.rs`, find the existing `mod tests` block, append:

```rust
#[test]
fn lsp_install_progress_carries_entry_ids() {
    let args = ScreenArgs::LspInstallProgress {
        entry_ids: vec!["rust-analyzer".into(), "pyright".into()],
    };
    match args {
        ScreenArgs::LspInstallProgress { entry_ids } => {
            assert_eq!(entry_ids, vec!["rust-analyzer", "pyright"]);
        }
        _ => panic!("expected LspInstallProgress"),
    }
}

#[test]
fn lsp_install_progress_screen_id_is_lsp_installer_progress() {
    let args = ScreenArgs::LspInstallProgress { entry_ids: vec![] };
    assert_eq!(args.screen_id(), Some("lsp_installer.progress"));
}
```

- [ ] **Step 2: Run the tests; confirm they fail to compile**

Run: `cargo test -p savvagent-plugin --lib lsp_install_progress`

Expected: compile error — `ScreenArgs::LspInstallProgress` does not exist.

- [ ] **Step 3: Add the variant + `screen_id()` arm**

In `crates/savvagent-plugin/src/types.rs`, inside `pub enum ScreenArgs { … }` add the variant (alphabetical-ish — after `LanguagePicker` works):

```rust
    /// Open the LSP-installer progress modal with the given catalog ids.
    /// Emitted by the picker's `Confirm` outcome; the screen owns the
    /// per-id install state and the spawned driver task.
    LspInstallProgress {
        /// Catalog ids the picker confirmed (e.g. `"rust-analyzer"`,
        /// `"pyright"`). Unknown ids surface as `Failed` entries inside
        /// the screen so a typo in the picker doesn't disappear silently.
        entry_ids: Vec<String>,
    },
```

In the `impl ScreenArgs { fn screen_id(&self) -> Option<&'static str> { match self { … } } }` block, add the arm:

```rust
            ScreenArgs::LspInstallProgress { .. } => Some("lsp_installer.progress"),
```

- [ ] **Step 4: Run the tests; confirm they pass**

Run: `cargo test -p savvagent-plugin --lib lsp_install_progress`

Expected: 2 tests pass.

- [ ] **Step 5: Run the rest of the plugin crate's tests to confirm no regression**

Run: `cargo test -p savvagent-plugin`

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-plugin/src/types.rs
git commit -m "feat(savvagent-plugin): add ScreenArgs::LspInstallProgress variant for the LSP install-progress modal"
```

---

### Task 2: Define `ProgressState`, `EntryStatus`, and pure state-mutation helpers

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs`
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs` — add `pub mod progress;`
- Test: same `progress.rs` file, `#[cfg(test)] mod tests`.

- [ ] **Step 1: Create the file with the type definitions**

Create `crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs`:

```rust
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
}
```

- [ ] **Step 2: Register the module in `mod.rs`**

Open `crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs`. After the existing `pub mod catalog;` etc. block (around line 7-11), add:

```rust
pub mod progress;
```

(Keep alphabetical / declaration order tidy — `progress` after `picker`.)

- [ ] **Step 3: Run the new tests**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress`

Expected: 2 tests pass.

- [ ] **Step 4: Run the whole savvagent crate's tests to confirm no regression**

Run: `cargo test -p savvagent`

Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs
git commit -m "feat(internal:lsp-installer): ProgressState + EntryStatus types for the progress modal"
```

---

### Task 3: Pure helper to apply an `InstallProgress` to `ProgressState`

This step keeps the state-transition logic separate from the spawned-task glue so it can be tested without any async.

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs` — add `apply_notification` + tests.

- [ ] **Step 1: Add failing tests for `apply_notification`**

Append to `crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs` (inside the existing `#[cfg(test)] mod tests` block):

```rust
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
```

- [ ] **Step 2: Run the tests; confirm they fail to compile**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress`

Expected: compile error — `apply_notification` not in scope.

- [ ] **Step 3: Implement `apply_notification`**

Add to the top-level of `progress.rs` (above the `#[cfg(test)] mod tests` block):

```rust
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
    match ev {
        InstallProgress::Started { .. } => { /* no-op; see doc */ }
        InstallProgress::Downloading {
            bytes_so_far, total, ..
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
```

- [ ] **Step 4: Run the tests; confirm they pass**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress`

Expected: all `progress` tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs
git commit -m "feat(internal:lsp-installer): apply_notification folds InstallProgress into ProgressState"
```

---

### Task 4: Resolve catalog ids → initial `ProgressState`

The screen's constructor needs to turn the `Vec<String>` from `ScreenArgs::LspInstallProgress` into a populated `ProgressState`, marking unknown ids as `Failed { fatal: false }`.

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs` — add `initial_state_for_ids` + tests.

- [ ] **Step 1: Add failing tests**

Append inside `#[cfg(test)] mod tests`:

```rust
    use crate::plugin::builtin::lsp_installer::catalog::CATALOG;

    #[test]
    fn initial_state_for_known_id_is_queued() {
        let known = CATALOG[0].id.to_string();
        let state = initial_state_for_ids(&[known.clone()]);
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
```

- [ ] **Step 2: Run the tests; confirm compile failure**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress::tests::initial_state`

Expected: compile error — `initial_state_for_ids` not defined.

- [ ] **Step 3: Implement `initial_state_for_ids`**

Add to `progress.rs` (above the `#[cfg(test)] mod tests` block):

```rust
use crate::plugin::builtin::lsp_installer::catalog::CATALOG;

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
```

- [ ] **Step 4: Run the tests; confirm they pass**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress`

Expected: all `progress` tests pass (including the three new ones).

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs
git commit -m "feat(internal:lsp-installer): initial_state_for_ids resolves catalog ids into ProgressState"
```

---

### Task 5: Driver task — run installs sequentially against stubs

The driver task drives `install_binary_entry` / `install_npm_entry` for each known entry in `ProgressState`, updating the shared state through `apply_notification`. The function takes both the install path (so tests can use a tempdir) and the trait objects (so tests can substitute stubs).

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs` — add `run_installs` + tests.

- [ ] **Step 1: Add a failing happy-path test**

Append inside `#[cfg(test)] mod tests`:

```rust
    use crate::plugin::builtin::lsp_installer::catalog::Target;
    use crate::plugin::builtin::lsp_installer::installer::{
        Downloader, InstallError, NpmRunner,
    };
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
    fn fake_binary_entry(id: &'static str) -> (&'static crate::plugin::builtin::lsp_installer::catalog::CatalogEntry, bytes::Bytes) {
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
        let url: &'static str = Box::leak(
            format!("https://example.test/{id}.gz").into_boxed_str(),
        );
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
                EntryProgress { id: "fake-a".into(), display_name: "fake-a".into(), status: EntryStatus::Queued },
                EntryProgress { id: "fake-b".into(), display_name: "fake-b".into(), status: EntryStatus::Queued },
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
```

- [ ] **Step 2: Run the test; confirm compile failure**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress::tests::run_installs_marks_each_entry_installed`

Expected: compile error — `run_installs` not defined.

- [ ] **Step 3: Implement `run_installs`**

Add to `progress.rs` (above the test module):

```rust
use crate::plugin::builtin::lsp_installer::catalog::{CatalogEntry, InstallMethod, Target};
use crate::plugin::builtin::lsp_installer::installer::{
    self, Downloader, InstallError, NpmRunner,
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
        let entry_id = entry.id.to_string();
        let notify = move |progress: installer::InstallProgress| {
            // Try-lock: blocking on a tokio Mutex outside an async
            // context isn't viable. Use `blocking_lock` since the
            // callback is invoked from inside the install future
            // which is itself running on tokio — but the *callback*
            // is sync. `blocking_lock` would deadlock on a single-
            // threaded runtime. The pragmatic choice: spawn a tiny
            // task. Cheap, never holds a lock across await.
            //
            // (See progress.rs design note.)
            let s = Arc::clone(&state_for_notify);
            tokio::spawn(async move {
                let mut guard = s.lock().await;
                apply_notification(&mut *guard, progress);
            });
            let _ = entry_id; // silence unused if logging is removed
        };

        let result = match entry.method {
            InstallMethod::BinaryDownload { .. } => {
                installer::install_binary_entry(
                    entry,
                    target,
                    &lsp_bin_root,
                    downloader,
                    notify,
                )
                .await
            }
            InstallMethod::NpmGlobal { .. } => {
                installer::install_npm_entry(entry, npm, notify).await
            }
        };

        match result {
            Ok(_outcome) => {
                // `InstallProgress::Done` already set the status to
                // `Installed`; nothing more to do here.
            }
            Err(InstallError::ChecksumMismatch { reason: _reason, .. }) => {
                let mut guard = state.lock().await;
                if let Some(slot) = guard.entries.iter_mut().find(|e| e.id == entry.id) {
                    slot.status = EntryStatus::Failed {
                        reason: "SHA256 mismatch".into(),
                        fatal: true,
                    };
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
                }
            }
        }
    }
}
```

> **Note on the spawn-per-notify pattern:** the install futures call `notify` synchronously from inside their own async context. The cleanest, deadlock-free way to mutate a tokio `Mutex` from a sync closure is to spawn a one-shot task that acquires the lock. The cost (a single spawn per stage) is negligible compared to the I/O the closure measures.

A subtle correctness note: `InstallError::ChecksumMismatch`'s real shape is `{ entry_id, expected, actual }` (no `reason` field). The destructuring pattern `Err(InstallError::ChecksumMismatch { reason: _reason, .. })` above is **wrong** — fix it to:

```rust
            Err(InstallError::ChecksumMismatch { .. }) => {
```

(Apply that correction in the same step.)

- [ ] **Step 4: Run the test; confirm pass**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress::tests::run_installs_marks_each_entry_installed`

Expected: pass. The "Installed" check should succeed for both entries.

- [ ] **Step 5: Add failing tests for the fatal + non-fatal paths**

Append inside `#[cfg(test)] mod tests`:

```rust
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
                EntryProgress { id: "fake-a".into(), display_name: "fake-a".into(), status: EntryStatus::Queued },
                EntryProgress { id: "fake-b".into(), display_name: "fake-b".into(), status: EntryStatus::Queued },
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
    async fn run_installs_non_fatal_error_continues_to_next_entry() {
        let (entry_a, archive_a) = fake_binary_entry("fake-a");
        let (entry_b, _archive_b) = fake_binary_entry("fake-b");

        // entry_a fails (download error), entry_b succeeds. We achieve
        // that by using a downloader that returns Err for any url
        // containing "fake-a" and the good archive for "fake-b".
        struct Mixed { good: bytes::Bytes }
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
                EntryProgress { id: "fake-a".into(), display_name: "fake-a".into(), status: EntryStatus::Queued },
                EntryProgress { id: "fake-b".into(), display_name: "fake-b".into(), status: EntryStatus::Queued },
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
```

- [ ] **Step 6: Run all `run_installs` tests; confirm pass**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress::tests::run_installs`

Expected: 3 tests pass.

- [ ] **Step 7: Confirm the whole crate still compiles & tests pass**

Run: `cargo test -p savvagent --lib`

Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs
git commit -m "feat(internal:lsp-installer): run_installs drives the per-entry install loop against shared state"
```

---

### Task 6: Driver-task spawn + finish flag + config-writer pass

This wraps `run_installs` with the final pieces: it collects `Installed` outcomes, calls `config_writer::merge_into_user_config`, captures any error in `state.config_error`, and sets `state.finished = true`.

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs` — add `spawn_driver` + tests.

- [ ] **Step 1: Add a failing test for finish + config-writer success path**

Append inside `#[cfg(test)] mod tests`:

```rust
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
```

- [ ] **Step 2: Run the test; confirm compile failure**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress::tests::spawn_driver_finishes_and_writes_config`

Expected: compile error — `spawn_driver` not defined.

- [ ] **Step 3: Implement `spawn_driver`**

Add to `progress.rs` (above the test module):

```rust
use crate::plugin::builtin::lsp_installer::config_writer;
use crate::plugin::builtin::lsp_installer::installer::InstallOutcome;

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
```

- [ ] **Step 4: Run the test; confirm pass**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress::tests::spawn_driver_finishes_and_writes_config`

Expected: pass.

- [ ] **Step 5: Run the whole crate to confirm no regressions**

Run: `cargo test -p savvagent`

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs
git commit -m "feat(internal:lsp-installer): spawn_driver orchestrates run_installs + config-writer + finished flag"
```

---

### Task 7: `LspProgressScreen` skeleton — construct, hold state, basic render

The screen owns the `Arc<Mutex<ProgressState>>` and the (unused after-spawn) `JoinHandle`. Its `new()` decides whether to spawn at all — if every id resolved to `Failed { fatal: false }` (all unknown), there's nothing to install and the screen opens already-finished.

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/lsp_installer/progress_screen.rs`
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs` — add `pub mod progress_screen;`
- Test: in `progress_screen.rs` `#[cfg(test)] mod tests`.

- [ ] **Step 1: Create the file with the skeleton + a failing test**

Create `crates/savvagent/src/plugin/builtin/lsp_installer/progress_screen.rs`:

```rust
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
    EntryStatus, ProgressState, initial_state_for_ids,
};

/// Render bridge between the install-driver task and the user. Reads
/// [`ProgressState`] each frame; emits `CloseScreen` (+ summary notes)
/// on user dismiss.
pub struct LspProgressScreen {
    state: Arc<TokioMutex<ProgressState>>,
}

impl LspProgressScreen {
    /// Build the screen and (if there's any work to do) spawn the
    /// install driver. `entry_ids` is the picker's confirmed selection.
    pub fn new(entry_ids: Vec<String>) -> Self {
        let state = Arc::new(TokioMutex::new(initial_state_for_ids(&entry_ids)));
        // Driver-task spawn is added in a later task; for now the
        // screen only renders the initial state.
        Self { state }
    }
}

#[async_trait]
impl Screen for LspProgressScreen {
    fn id(&self) -> String {
        "lsp_installer.progress".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        // Acquire the lock with `blocking_lock` — render() is sync,
        // and the writer side never holds the lock across an await,
        // so contention windows are sub-microsecond. Acceptable.
        let state = self.state.blocking_lock();
        let mut lines: Vec<StyledLine> = Vec::new();
        lines.push(StyledLine::plain(format!(
            "Installing {} language server(s)…",
            state.entries.len()
        )));
        lines.push(StyledLine::plain(""));
        for entry in &state.entries {
            lines.push(StyledLine::plain(format!(
                "  {} {}",
                glyph_for(&entry.status),
                entry.display_name
            )));
        }
        lines
    }

    async fn on_key(&mut self, _key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        // Filled in in the next task.
        Ok(vec![])
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            "Esc dismisses (install continues in background).",
        )]
    }
}

fn glyph_for(status: &EntryStatus) -> &'static str {
    match status {
        EntryStatus::Queued => "..",
        EntryStatus::Downloading { .. }
        | EntryStatus::Verifying
        | EntryStatus::Extracting
        | EntryStatus::RunningNpm { .. } => "..",
        EntryStatus::Installed { .. } => "OK",
        EntryStatus::Failed { .. } => "!!",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_lsp_installer_progress() {
        let s = LspProgressScreen::new(vec![]);
        assert_eq!(s.id(), "lsp_installer.progress");
    }

    #[test]
    fn render_lists_an_entry_per_id() {
        // Use real catalog ids so they don't get pre-failed.
        use crate::plugin::builtin::lsp_installer::catalog::CATALOG;
        let id_a = CATALOG[0].id.to_string();
        let id_b = CATALOG[1].id.to_string();
        let s = LspProgressScreen::new(vec![id_a.clone(), id_b.clone()]);
        let lines = s.render(Region {
            width: 80,
            height: 24,
        });
        // Header + blank + 2 entries == at least 4 lines.
        assert!(lines.len() >= 4);
        let body = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.contains(&id_a));
        assert!(body.contains(&id_b));
    }
}
```

- [ ] **Step 2: Register the module in `mod.rs`**

Open `crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs`. Add to the module declarations:

```rust
pub mod progress_screen;
```

- [ ] **Step 3: Run the tests; confirm pass**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress_screen`

Expected: 2 tests pass.

- [ ] **Step 4: Confirm the whole crate still compiles**

Run: `cargo build -p savvagent`

Expected: success.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/progress_screen.rs crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs
git commit -m "feat(internal:lsp-installer): LspProgressScreen skeleton + basic render"
```

---

### Task 8: Render — rich per-stage formatting + summary footer

Replace the minimal render with one that matches the spec's per-stage labels and shows the summary footer (`K of N done · M in progress · Q queued`).

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/progress_screen.rs` — rewrite `render` + helpers + tests.

- [ ] **Step 1: Add failing tests for the formatted render output**

Replace the existing `render_lists_an_entry_per_id` test and append new ones inside `#[cfg(test)] mod tests`:

```rust
    use crate::plugin::builtin::lsp_installer::progress::{EntryProgress, ProgressState};
    use std::path::PathBuf;

    fn screen_with_state(state: ProgressState) -> LspProgressScreen {
        LspProgressScreen {
            state: Arc::new(TokioMutex::new(state)),
        }
    }

    fn rendered(s: &LspProgressScreen) -> String {
        s.render(Region {
            width: 80,
            height: 24,
        })
        .iter()
        .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
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
```

- [ ] **Step 2: Run the new tests; confirm they fail**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress_screen`

Expected: several assertion failures (the minimal renderer doesn't emit the spec strings).

- [ ] **Step 3: Implement the formatted render**

Replace the existing `fn render` and `fn glyph_for` in `progress_screen.rs` with:

```rust
impl LspProgressScreen {
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
            out.push(StyledLine::plain(
                "Press Enter to close. Restart savvagent to pick up the new servers.",
            ));
            if let Some(err) = &state.config_error {
                out.push(StyledLine::plain(format!(
                    "Warning: writing lsp.toml failed: {err}"
                )));
            }
        }

        out
    }
}

fn format_entry(entry: &EntryProgress) -> (&'static str, String) {
    match &entry.status {
        EntryStatus::Queued => ("..", "queued".to_string()),
        EntryStatus::Downloading { bytes_so_far, total } => {
            let label = match total {
                Some(t) => format!(
                    "downloading… {} / {}",
                    human_mb(*bytes_so_far),
                    human_mb(*t)
                ),
                None => format!("downloading… {}", human_mb(*bytes_so_far)),
            };
            ("..", label)
        }
        EntryStatus::Verifying => ("..", "verifying SHA256…".to_string()),
        EntryStatus::Extracting => ("..", "extracting…".to_string()),
        EntryStatus::RunningNpm { last_line } => (
            "..",
            format!("running npm…   {}", truncate(last_line, 48)),
        ),
        EntryStatus::Installed { .. } => ("OK", "installed".to_string()),
        EntryStatus::Failed { reason, .. } => ("!!", format!("failed: {reason}")),
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
```

Then rewrite the trait `render` method to delegate to `render_lines`:

```rust
    fn render(&self, _region: Region) -> Vec<StyledLine> {
        let state = self.state.blocking_lock();
        Self::render_lines(&state)
    }
```

Delete the now-unused `glyph_for` helper.

- [ ] **Step 4: Run the tests; confirm they pass**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress_screen`

Expected: all render tests pass.

- [ ] **Step 5: Run clippy on the worktree's stable toolchain**

Run: `rustup run stable cargo clippy -p savvagent --lib -- -D warnings`

Expected: success.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/progress_screen.rs
git commit -m "feat(internal:lsp-installer): rich per-stage render formatting + summary footer"
```

---

### Task 9: `on_key` — Enter (when finished) closes + emits summary notes; Esc dismisses with continue-in-background note

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/progress_screen.rs` — fill in `on_key` + tests.

- [ ] **Step 1: Add failing tests**

Append inside `#[cfg(test)] mod tests`:

```rust
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
        assert!(effs.is_empty(), "Enter pre-finish must not close, got {effs:?}");
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
                    children.iter().skip(1).all(|e| matches!(e, Effect::PushNote { .. })),
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
```

- [ ] **Step 2: Run the tests; confirm failures**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress_screen`

Expected: 4 new tests fail (current `on_key` returns empty Vec).

- [ ] **Step 3: Implement `on_key`**

Replace the existing `on_key` method in `progress_screen.rs` with:

```rust
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
                        "[lsp-installer] still installing in the background — results will appear when done"
                            .to_string(),
                    ),
                },
            ])]),
            _ => Ok(vec![]),
        }
    }
```

Then add the helper near the bottom of the file (above `#[cfg(test)] mod tests`):

```rust
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
                format!("[lsp-installer] {}: {prefix} — {reason}", entry.id)
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
    effs.push(Effect::PushNote {
        line: StyledLine::plain(
            "[lsp-installer] done — restart savvagent to pick up the new servers".to_string(),
        ),
    });
    effs
}
```

- [ ] **Step 4: Run the tests; confirm pass**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress_screen`

Expected: all `on_key` + render tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/progress_screen.rs
git commit -m "feat(internal:lsp-installer): progress screen Enter closes + emits summary, Esc dismisses with background note"
```

---

### Task 10: Wire `spawn_driver` into `LspProgressScreen::new`

The screen now constructs and spawns the driver when there's work to do. We construct the dependencies (`Target`, `~/.savvagent/lsp-bin`, `~/.savvagent/lsp.toml`, `ReqwestDownloader`, `SystemNpmRunner`) the same way `handle_install` does today — preserving the same error-handling semantics (pre-mark state with a single error entry; skip spawn).

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/progress_screen.rs` — fill in the body of `LspProgressScreen::new`.
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs` — make sure `spawn_driver` is `pub`.

- [ ] **Step 1: Implement the spawn inside `new`**

Replace the body of `LspProgressScreen::new` in `progress_screen.rs` with:

```rust
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

        let (target, home, downloader) = match (target, home, downloader_opt) {
            (Some(t), Some(h), Some(d)) => (t, h, d),
            _ => {
                let reason = match (target.is_some(), home.is_some()) {
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

        let state = Arc::new(TokioMutex::new(initial));

        if entries.is_empty() {
            // Nothing to do — every selected id was unknown. Mark the
            // state finished so the user can press Enter to dismiss.
            let state_finish = Arc::clone(&state);
            tokio::spawn(async move {
                let mut g = state_finish.lock().await;
                g.finished = true;
            });
            return Self { state };
        }

        let downloader: Arc<dyn crate::plugin::builtin::lsp_installer::installer::Downloader> =
            Arc::new(downloader);
        let npm: Arc<dyn crate::plugin::builtin::lsp_installer::installer::NpmRunner> =
            Arc::new(crate::plugin::builtin::lsp_installer::installer::SystemNpmRunner);

        // Drop the JoinHandle — the screen polls `state` for progress
        // instead of awaiting the task. The task completes when it
        // sets `state.finished = true`.
        let _ = crate::plugin::builtin::lsp_installer::progress::spawn_driver(
            entries,
            target,
            lsp_bin_root,
            lsp_toml,
            Arc::clone(&state),
            downloader,
            npm,
        );

        Self { state }
    }
```

- [ ] **Step 2: Add a test for the unknown-ids-only path**

Append inside `#[cfg(test)] mod tests`:

```rust
    #[tokio::test]
    async fn unknown_only_selection_finishes_immediately() {
        let s = LspProgressScreen::new(vec!["totally-fake-id".to_string()]);
        // Give the spawned 'finish' task a tick to run.
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            let g = s.state.lock().await;
            if g.finished {
                return;
            }
        }
        panic!("state should have flipped to finished but did not");
    }
```

- [ ] **Step 3: Run all progress-screen tests; confirm pass**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress_screen`

Expected: pass. (No network calls — every known catalog id requires real downloads, but we don't pass any known ids here.)

- [ ] **Step 4: Run the whole crate to confirm no regressions**

Run: `cargo test -p savvagent`

Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/progress_screen.rs
git commit -m "feat(internal:lsp-installer): wire spawn_driver into LspProgressScreen::new"
```

---

### Task 11: Register the new screen in the plugin manifest + `create_screen`

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs` — append a `ScreenSpec`, add a `create_screen` arm.

- [ ] **Step 1: Add a failing test for the new manifest entry**

Open `crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs`, find the `#[cfg(test)] mod tests` block, append:

```rust
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
                ScreenArgs::LspInstallProgress {
                    entry_ids: vec![],
                },
            )
            .expect("create_screen must accept the progress id");
        assert_eq!(screen.id(), "lsp_installer.progress");
    }
```

- [ ] **Step 2: Run the tests; confirm they fail**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::tests::manifest_advertises_progress_screen plugin::builtin::lsp_installer::tests::create_screen_returns_progress_screen`

Expected: failures — the manifest doesn't list the screen and `create_screen` doesn't know the id.

- [ ] **Step 3: Add the manifest entry + dispatch arm**

In `mod.rs`, find the `contributions.screens = vec![ ScreenSpec { id: "lsp_installer.picker", … } ]` block and extend it:

```rust
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
```

Then extend `create_screen`:

```rust
    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        match id {
            "lsp_installer.picker" => Ok(Box::new(LspPickerScreen::new())),
            "lsp_installer.progress" => {
                let entry_ids = match args {
                    ScreenArgs::LspInstallProgress { entry_ids } => entry_ids,
                    ScreenArgs::None => Vec::new(),
                    other => {
                        return Err(PluginError::ScreenNotFound(format!(
                            "lsp_installer.progress: unexpected ScreenArgs {other:?}"
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
```

- [ ] **Step 4: Run the tests; confirm pass**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer`

Expected: all pass (including the two new tests).

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs
git commit -m "feat(internal:lsp-installer): manifest entry + create_screen arm for lsp_installer.progress"
```

---

### Task 12: Picker `Confirm` opens the progress screen instead of `RunSlash __install`

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/screen.rs` — `Confirm` arm + tests.

- [ ] **Step 1: Update the existing `Confirm`-emits-RunSlash test, and add an OpenScreen one**

Open `crates/savvagent/src/plugin/builtin/lsp_installer/screen.rs`, find the `enter_with_one_selection_emits_runslash_install` test (~line 173) and replace it with:

```rust
    #[tokio::test]
    async fn enter_with_one_selection_emits_openscreen_progress() {
        let mut s = LspPickerScreen::new();
        s.on_key(key(KeyCodePortable::Char(' '))).await.unwrap();
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match &effs[..] {
            [Effect::Stack(children)] => {
                assert!(matches!(children[0], Effect::CloseScreen));
                match &children[1] {
                    Effect::OpenScreen { id, args } => {
                        assert_eq!(id, "lsp_installer.progress");
                        match args {
                            savvagent_plugin::ScreenArgs::LspInstallProgress { entry_ids } => {
                                assert_eq!(entry_ids.len(), 1, "exactly one id");
                            }
                            other => panic!("expected LspInstallProgress args, got {other:?}"),
                        }
                    }
                    other => panic!("expected OpenScreen, got {other:?}"),
                }
            }
            other => panic!("expected single Stack, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run the test; confirm failure**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::screen::tests::enter_with_one_selection_emits_openscreen_progress`

Expected: failure — the picker still emits `RunSlash`.

- [ ] **Step 3: Update the `Confirm` arm**

In `screen.rs`, find the `MultiSelectOutcome::Confirm(items)` arm of `on_key` (~line 98) and replace:

```rust
            MultiSelectOutcome::Confirm(items) => {
                if items.is_empty() {
                    return Ok(vec![Effect::CloseScreen]);
                }
                let entry_ids: Vec<String> = items.iter().map(|e| e.id.to_string()).collect();
                Ok(vec![Effect::Stack(vec![
                    Effect::CloseScreen,
                    Effect::OpenScreen {
                        id: "lsp_installer.progress".into(),
                        args: savvagent_plugin::ScreenArgs::LspInstallProgress { entry_ids },
                    },
                ])])
            }
```

(Old code used `Effect::RunSlash { name: "lsp", args: vec!["__install", …] }`; that path stays callable from `handle_slash("lsp", ["__install", …])` for external dispatchers — only the picker stops using it.)

- [ ] **Step 4: Run the test; confirm pass**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::screen`

Expected: all picker tests pass (the zero-selection-closes test still works).

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/screen.rs
git commit -m "feat(internal:lsp-installer): picker Confirm opens the progress modal instead of RunSlash __install"
```

---

### Task 13: End-to-end smoke test — picker → progress modal against a local HTTP server

This integration test boots the plugin, simulates a picker Confirm, opens the progress screen with a real spawned driver, and waits for `state.finished` against a local HTTP fixture (same shape as `installer::smoke_local_http_install`).

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs` — add an `#[cfg(test)] async fn smoke_end_to_end` test that drives `spawn_driver` against a real catalog-shaped entry served from a loopback listener.

- [ ] **Step 1: Add the failing test**

Append inside `#[cfg(test)] mod tests` in `progress.rs`:

```rust
    /// Serve a gzipped fixture over loopback, build a real
    /// `CatalogEntry` pointing at it, and run `spawn_driver` end-to-end
    /// against the production `ReqwestDownloader`. Asserts that:
    ///   - `state.finished` flips true,
    ///   - the entry lands in `Installed`,
    ///   - `lsp.toml` was written and parses.
    #[tokio::test]
    async fn smoke_spawn_driver_end_to_end() {
        use sha2::{Digest, Sha256};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;
        use crate::plugin::builtin::lsp_installer::catalog::{
            CatalogEntry, CommandTemplate, InstallMethod, LspEntryTemplate, Target,
        };
        use crate::plugin::builtin::lsp_installer::installer::ReqwestDownloader;

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
        let urls: &'static [(Target, &'static str, &'static str)] =
            Box::leak(Box::new([(Target::current().expect("host target"), url_static, sha_static)]));

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
```

- [ ] **Step 2: Run the smoke test**

Run: `cargo test -p savvagent --lib plugin::builtin::lsp_installer::progress::tests::smoke_spawn_driver_end_to_end`

Expected: pass.

- [ ] **Step 3: Run the full workspace test suite**

Run: `cargo test --workspace`

Expected: all pass.

- [ ] **Step 4: Run clippy on stable for parity with CI**

Run: `rustup run stable cargo clippy --workspace --all-targets -- -D warnings`

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/progress.rs
git commit -m "test(internal:lsp-installer): end-to-end smoke for the progress driver against a local HTTP server"
```

---

### Task 14: Manual TUI verification + final formatting

The above tasks ship working code with strong test coverage, but the spec's UX claim ("user sees per-server stages mid-install") is a TUI behaviour the test suite does not exercise directly. This task is a manual sanity check.

**Files:** none.

- [ ] **Step 1: Build the workspace**

Run: `cargo build`

Expected: success.

- [ ] **Step 2: Run the TUI against a real catalog entry**

Run: `cargo run -p savvagent`

In the TUI, type `/lsp`, select one or two language servers with Space, press Enter. The progress modal should open. For each selected server you should see status flip through `queued → downloading… X.X MB / Y.Y MB → verifying SHA256… → extracting… → installed` (or `running npm…` for npm entries). Once all entries settle, the footer should change to `All done — N installed, M failed. Press Enter to close.`.

If you have time, hit Esc mid-install in a second run to verify the "still installing in the background" note lands in the conversation log.

- [ ] **Step 3: If anything in the manual run looks wrong**

Capture what you saw, identify the responsible task, and circle back. Don't paper over with adjustments to the rendered strings — fix the underlying state transition.

- [ ] **Step 4: Run `cargo fmt` on the stable toolchain**

Run: `rustup run stable cargo fmt --all`

Expected: no diff (or only whitespace adjustments). If there is a diff, stage and amend the most recent commit:

```bash
git add -A
git commit --amend --no-edit
```

- [ ] **Step 5: Final test pass**

Run: `cargo test --workspace && rustup run stable cargo clippy --workspace --all-targets -- -D warnings`

Expected: both clean.

---

## Wrap-up checklist

- [ ] All tasks (1–14) committed.
- [ ] `cargo test --workspace` passes.
- [ ] `rustup run stable cargo clippy --workspace --all-targets -- -D warnings` is clean.
- [ ] `rustup run stable cargo fmt --all -- --check` is clean.
- [ ] Manual TUI run shows live per-server progress for at least one binary-download entry and (if `npm` is on `$PATH`) one npm entry.
- [ ] CHANGELOG entry under "Unreleased" added for this feature (one line: "live install-progress modal for `/lsp`").
- [ ] PR description references the spec at `docs/superpowers/specs/2026-05-21-lsp-installer-progress-design.md`.

## Out of scope (do not implement)

- Parallel installs (sequential stays).
- Real cancellation (Esc continues the spawned task in the background).
- Removing the `__install` slash sub-command (it stays callable for now).
- A generic "Progress" widget abstraction for other plugins.
- Persisting / restoring an in-flight install across `/lsp` re-opens.
- Plain-ASCII glyph fallback for non-Unicode terminals.
