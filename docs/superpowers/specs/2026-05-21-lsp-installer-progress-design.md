# LSP installer — live install-progress UX

Status: draft for review · 2026-05-21
Owner: savvagent-rs · `internal:lsp-installer`
Follow-up to: `docs/superpowers/specs/2026-05-20-lsp-installer-design.md`

## Goal

Give the user live, per-server feedback while `/lsp` is running so that
selecting "several" servers does not look like the TUI has hung. The v1
installer (PR #93) batched every `Effect::PushNote` and flushed them
only *after* every install finished; with four or five servers selected
that is a multi-minute blank wait.

## Non-goals

- Parallel installs. v1 sequencing stays.
- True cancellation. Esc dismisses the modal; the spawned task runs to
  completion in the background. A real cancel token is a future change.
- Retry / resume on failure. A failed entry stays failed; the user
  re-runs `/lsp` to retry.
- A new generic "progress" Effect for other plugins. The progress
  screen is internal to `internal:lsp-installer`. Generalising to a
  reusable widget happens only if a second feature needs it.
- Refactoring `handle_install` away. The legacy synchronous path moves
  into the spawned task body verbatim; the only structural change is
  *who* awaits the future and *how* progress reaches the UI.

## UX

### After Enter on the picker

The picker still does `MultiSelectOutcome::Confirm(items)`. Instead of
emitting `Stack([CloseScreen, RunSlash{__install,…}])`, it emits
`Stack([CloseScreen, OpenScreen{"lsp_installer.progress", ids}])`.

The progress screen opens immediately and looks like:

```
┌── Installing language servers ─────────────────────────┐
│                                                        │
│  ● rust-analyzer        downloading… 12.4 MB / 28.0 MB │
│  ✓ lua-language-server  installed                      │
│  ⋯ typescript-language-server  queued                  │
│  ⋯ pyright              queued                         │
│                                                        │
│  1 of 4 done · 1 in progress · 2 queued                │
│                                                        │
│  Esc dismisses (install continues in background)       │
└────────────────────────────────────────────────────────┘
```

Stage labels per entry:

| State            | Glyph | Right-hand text                                          |
| ---------------- | ----- | -------------------------------------------------------- |
| Queued           | `⋯`   | `queued`                                                 |
| Downloading      | `●`   | `downloading… <bytes_so_far> / <total>` (or `… <bytes>`) |
| Verifying        | `●`   | `verifying SHA256…`                                      |
| Extracting       | `●`   | `extracting…`                                            |
| Running npm      | `●`   | `running npm…   <last line, truncated>`                  |
| Installed        | `✓`   | `installed`                                              |
| Failed (generic) | `✗`   | `failed: <one-line reason>`                              |
| Checksum failure | `✗`   | `SHA256 mismatch — batch aborted`                        |

Glyphs are theme-coloured (`✓` = success/green, `✗` = error/red, `●` =
accent, `⋯` = dim). Plain ASCII fallbacks (`OK`, `!!`, `..`, `..`) are a
follow-up if the theme reports a non-Unicode terminal — out of scope
here.

### After all entries finish

The footer changes to:

```
  All done — 3 installed, 1 failed.
  Press Enter to close. Restart savvagent to pick up the new servers.
```

On Enter the screen closes and emits a small batch of `PushNote`
effects summarising the run, so the conversation log keeps a permanent
record (same content as today's notes — just delivered at the end,
*from the progress screen* instead of `handle_install`).

### Esc during an install

Esc closes the modal immediately. The spawned task keeps running. When
it finishes it has no UI to update, but its final `PushNote`s still
reach the conversation log via the existing worker channel (see
"Driver task" below).

The screen tips line spells this out so it isn't a surprise.

## Architecture

Two new files inside `crates/savvagent/src/plugin/builtin/lsp_installer/`:

- `progress.rs` — `ProgressState`, `EntryStatus`, the install driver
  task, the channel wiring.
- `progress_screen.rs` — `LspProgressScreen` implementing
  `savvagent_plugin::Screen`.

`mod.rs` gains a new entry in `Contributions::screens` and a new arm in
`create_screen`. `screen.rs` (the picker) swaps the `Confirm` arm from
`RunSlash{__install,…}` to `OpenScreen{"lsp_installer.progress", …}`.

The `handle_slash("lsp", ["__install", …])` path stays in place for now
— it keeps working for any external dispatcher (CI smoke, future
keybindings), and the existing tests around it stay green. The picker
just no longer routes through it.

### Shared progress state

```rust
// progress.rs
pub struct ProgressState {
    pub entries: Vec<EntryProgress>,
    pub finished: bool,
}

pub struct EntryProgress {
    pub id:           &'static str,         // catalog id
    pub display_name: &'static str,
    pub status:       EntryStatus,
}

pub enum EntryStatus {
    Queued,
    Downloading { bytes_so_far: u64, total: Option<u64> },
    Verifying,
    Extracting,
    RunningNpm  { last_line: String },
    Installed   { installed_at: PathBuf },
    Failed      { reason: String, fatal: bool },  // fatal = ChecksumMismatch (batch aborted)
}
```

The state lives behind `Arc<Mutex<ProgressState>>`. The mutex is fine —
the writer is a single tokio task that acquires it for short
state-mutation windows and never holds it across an `await`; the reader
is the synchronous `render()` path which also holds it briefly. We do
**not** hand the mutex to `handle_install`'s `notify` callback directly
— see "Notify forwarding" below.

### Notify forwarding

`installer::install_binary_entry` and `installer::install_npm_entry`
take `notify: impl Fn(InstallProgress) + Send + Sync`. The driver task
constructs a closure that holds a clone of `Arc<Mutex<ProgressState>>`,
matches on `InstallProgress`, finds the right entry by id, and updates
its `EntryStatus`. The closure is synchronous (no await) so the mutex
lock is trivial.

Because `Fn(InstallProgress)` already takes ownership of the value, the
closure can `.clone()` strings out of the variant freely — none of the
existing call sites read the value back after `notify` returns.

### Driver task lifecycle

When `LspProgressScreen::new(ids)` runs:

1. Resolve ids → catalog entries (same logic as
   `LspInstallerPlugin::handle_install` today). Unknown ids land in
   `ProgressState.entries` with `Failed{ reason: "no catalog entry", fatal: false }`
   so the user sees what was skipped.
2. Resolve `Target::current()`, `~/.savvagent/lsp-bin`,
   `~/.savvagent/lsp.toml`. Any of these failing → push a single
   pre-failure entry, mark `finished=true`, no spawn.
3. Build the `Downloader` and `NpmRunner` once.
4. `tokio::spawn` the driver task. The task:
   - For each entry in `entries` (sequential), set status to a
     starting stage and run the appropriate `install_*_entry` future.
   - On `Ok(outcome)` → `EntryStatus::Installed{ … }` + collect for
     the config-writer pass.
   - On `Err(ChecksumMismatch)` → mark this entry `Failed { fatal: true }`,
     mark every remaining `Queued` entry `Failed { reason: "batch aborted after SHA mismatch", fatal: true }`,
     break out of the loop. Same security semantics as today's
     `handle_install`.
   - On other `Err` → `EntryStatus::Failed{ … fatal: false }` and
     continue to the next entry.
   - After the loop, run `config_writer::merge_into_user_config(...)`
     on the successful outcomes. Any write failure pushes a
     screen-level error line into `ProgressState`.
   - Set `finished = true`.

The driver task ends. The screen continues to render the final state
until the user dismisses it.

### How the UI sees state changes

The TUI main loop already polls `crossterm::event::poll(50ms)` (see
`main.rs:2620`) and re-renders every iteration regardless of whether an
event fired. That is the redraw cadence — ~20 fps free. The progress
screen's `render()` reads the latest state from the mutex on every
frame.

This means we **do not** need a new `Effect` variant, a new
`HostEvent`, or a new `WorkerMsg` to make the screen feel live. The
existing render cadence is good enough; if the user types nothing the
modal still refreshes within 50 ms of any state change.

### Final notes routed to the conversation log

Today's `handle_install` emits a flurry of `PushNote`s at the end so
the conversation log keeps a permanent record of what installed where.
We want to keep that behaviour. Two options for routing them:

- **(Picked.)** Stash the final summary list inside `ProgressState`.
  When the user presses Enter on the finished screen, the screen
  returns `Stack([CloseScreen, PushNote{…}, …])` — the runtime applies
  the notes in order. Simple, no extra plumbing. Downside: if the user
  presses Esc during install and never comes back to the modal, the
  notes never reach the conversation log.
- *Not picked.* Plumb a `WorkerMsg::Note(StyledLine)` variant so the
  driver task can push notes from anywhere. Out of scope — would touch
  `main.rs` for one feature.

To cover the Esc-during-install case for the *user-visible record*: the
screen on Esc returns `Stack([CloseScreen, PushNote{"[lsp-installer] still installing in the background — results will appear when done"}])`.
The driver task still completes; the user just sees no final summary
unless they re-open `/lsp` (which can render the post-finish view if
`ProgressState` is still cached — but caching across modal opens is a
v2 nice-to-have; in v1 the Esc path forgoes the summary).

### Open question worth flagging

If the user re-opens `/lsp` while a previous install is mid-flight, we
get two driver tasks racing on the same `lsp-bin/` and `lsp.toml`. For
v1 we accept this — the install dir is per-entry (`<bin_root>/<id>/`)
so two installs of *different* sets are independent, and the config
writer is atomic per call. We will land a note in the progress screen
once concurrency is observable in practice.

## Files that change

| File                                                                 | Change                                                                                |
| -------------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| `plugin/builtin/lsp_installer/mod.rs`                                | Advertise the new screen; add `create_screen` arm; keep `__install` slash for now.    |
| `plugin/builtin/lsp_installer/screen.rs` (picker)                    | Confirm arm emits `OpenScreen{"lsp_installer.progress", …}` instead of `RunSlash`.    |
| `plugin/builtin/lsp_installer/progress.rs` (new)                     | `ProgressState`, `EntryStatus`, driver task constructor.                              |
| `plugin/builtin/lsp_installer/progress_screen.rs` (new)              | `LspProgressScreen` implementing `Screen`.                                            |
| `plugin/builtin/lsp_installer/installer.rs`                          | No change. Notify callback already supports per-stage updates.                        |

## Testing

- **Unit:** `progress.rs` — driver task state transitions against a
  stub `Downloader` / `NpmRunner`. Cases:
  - 3 entries, all succeed → final state is 3 × `Installed`.
  - Middle entry fails (non-fatal) → 1 success, 1 failed, 1 success.
  - First entry returns `ChecksumMismatch` → entry 0 is `Failed{fatal:true}`,
    entries 1..n are also `Failed{fatal:true}` with the batch-aborted
    reason.
  - Config-writer error → `ProgressState.finished == true` and a
    config-write failure line is captured.
- **Screen:** `progress_screen.rs` — `render()` formatting on each
  `EntryStatus`; `on_key(Enter)` after `finished=true` returns the
  `Stack([CloseScreen, PushNote…])`; `on_key(Esc)` mid-install returns
  `Stack([CloseScreen, PushNote("still installing")])`.
- **Smoke:** extend the existing
  `installer::smoke_local_http_install` shape into a progress-screen
  test that drives the modal end-to-end against a local HTTP fixture,
  asserting `ProgressState` reaches the expected final shape.

Existing tests stay green: the picker's `enter_with_one_selection_*`
assertion changes from "emits `RunSlash{__install,…}`" to "emits
`OpenScreen{"lsp_installer.progress", …}`".

## Risks

- **Lock discipline.** The driver task must never hold the
  `ProgressState` mutex across an `await`. `installer::install_*_entry`
  drives the I/O — the closure runs only on each `InstallProgress`
  notification, which is fully synchronous. Easy to enforce; lint via
  `clippy::await_holding_lock`.
- **Modal stuck open after a panic.** If the driver task panics, the
  screen sees `finished == false` forever. Mitigation: catch
  `JoinError` on the spawned task; on panic, write a "driver task
  crashed" entry and set `finished = true`. The user can dismiss.
- **Frame thrash.** A fast download could update `bytes_so_far`
  thousands of times per second. The render cadence is capped at ~20
  fps regardless, so the cost is the mutex lock + state copy each
  update, which is negligible. Not optimising in v1.

## Migration / rollout

Single PR. No feature flag. The existing `handle_install` path stays
callable so smoke tests and CI don't break. Once the progress screen
ships and we see no regressions, a follow-up PR can delete the
`__install` slash sub-command (only the picker uses it today).
