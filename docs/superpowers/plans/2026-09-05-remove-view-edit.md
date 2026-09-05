# Remove `/view` and `/edit` Implementation Plan

**Goal:** Remove the `/view`, `/edit`, and `/editor-keybindings` slash commands and every line of
code that exists solely to support them — both frontends' file-editor rendering/input paths, the
three built-in plugins, the plugin-ABI enum variants that carry them, the `ratatui-code-editor` and
`egui_code_editor` crate dependencies, their locale strings, and their README/PRD documentation —
leaving zero dead code and zero orphaned documentation.

**Architecture:** This is a subtractive change across existing crate boundaries, removed in
dependency order so every task ends at a green build:

1. `crates/savvagent` ratatui frontend: the `internal:view-file`/`internal:edit-file`/
   `internal:editor-keybindings` plugins, `App::editor` and its supporting methods, the legacy
   `InputMode::ViewingFile`/`EditingFile` path, and the `main.rs`/`ui.rs` marker-screen
   dispatch/render branches. `App::active_file_path`/`clear_active_editor()` are deliberately left
   in place here — the egui frontend still reads/calls them until step 2 below.
2. `crates/savvagent` egui frontend: `editor_buffer`, `save_editor_buffer`, the marker-screen
   sync/paint branches, the `egui_app/widgets/editor.rs` + `editor_theme.rs` widgets, and (once
   those are gone) the now-unreferenced `App::active_file_path`/`clear_active_editor()` left over
   from step 1.
3. `crates/savvagent-plugin` (the plugin ABI) + `crates/savvagent-plugin-wasm` (its WASM adapter):
   once no code in either frontend constructs or matches `Effect::SaveActiveFile` /
   `ScreenArgs::ViewFile` / `ScreenArgs::EditFile`, the variants themselves come out.
4. The now-unused `ratatui-code-editor` / `egui_code_editor` workspace dependencies.
5. Locale strings (all four files, kept in lockstep per `tests/locales.rs`), `README.md`, `PRD.md`,
   and two stale doc-comment cross-references.

Tasks 1 and 2 are ordered so that egui's `ScreenArgs::ViewFile`/`EditFile` matches still compile
after Task 1 removes the ratatui side (the enum variants aren't touched until Task 3, once nothing
in `crates/savvagent` references them). Tasks 1–2 run their build/test/clippy/fmt checks scoped to
`-p savvagent` (the only crate either task touches); Tasks 3–5 run them `--workspace`, since those
tasks touch other crates or the workspace root. Every task ends with a "Format and commit" step —
no task ends with a broken build.

**Tech Stack:** Rust 2024, the existing `savvagent-plugin` `Screen`/`Effect`/`ScreenArgs` traits and
enums, `rust_i18n::t!` + the four TOML locale files under `crates/savvagent/locales/`, `cargo-dist`
for the eventual release.

**Spec:** `docs/superpowers/specs/2026-09-05-remove-view-edit-design.md` — read it first. This plan
implements it exactly.

**Release line:** `v0.20.0` has already merged to `main` via PR #29 (`release/0-20-0`) — so this
change ships as **`v0.21.0`**, the next MINOR. **Confirm the actual next-unclaimed MINOR against
`origin/main`'s `Cargo.toml` at release-cut time regardless** — another PR could claim a MINOR
between now and then. This is a **MINOR** bump, not a PATCH, because it is a breaking change to
the slash-command surface and the plugin ABI (Non-Negotiable Rule 6 — see the spec's
"Public-interface changes" section).

**Branch:** `plugin/remove-view-edit`

**File Map:**

- Delete: `crates/savvagent/src/plugin/builtin/view_file/mod.rs`,
  `crates/savvagent/src/plugin/builtin/view_file/screen.rs`,
  `crates/savvagent/src/plugin/builtin/edit_file/mod.rs`,
  `crates/savvagent/src/plugin/builtin/edit_file/screen.rs`,
  `crates/savvagent/src/plugin/builtin/editor_keybindings/mod.rs`,
  `crates/savvagent/src/plugin/builtin/themes/editor_theme.rs`,
  `crates/savvagent/src/egui_app/widgets/editor.rs`,
  `crates/savvagent/src/egui_app/widgets/editor_theme.rs`.
- Modified: `crates/savvagent/src/app.rs`, `crates/savvagent/src/main.rs`,
  `crates/savvagent/src/ui.rs`, `crates/savvagent/src/plugin/effects.rs`,
  `crates/savvagent/src/plugin/mod.rs`, `crates/savvagent/src/plugin/builtin/mod.rs`,
  `crates/savvagent/src/plugin/builtin/themes/mod.rs`, `crates/savvagent/src/egui_app/mod.rs`,
  `crates/savvagent/src/egui_app/view.rs`, `crates/savvagent/src/egui_app/widgets/mod.rs`,
  `crates/savvagent/src/egui_app/convert.rs`,
  `crates/savvagent-plugin/src/effect.rs`, `crates/savvagent-plugin/src/types.rs`,
  `crates/savvagent-plugin-wasm/src/adapter/interactive.rs`, `crates/savvagent/Cargo.toml`,
  `Cargo.toml` (root), `Cargo.lock`, `crates/savvagent/locales/{en,es,hi,pt}.toml`, `README.md`,
  `PRD.md`, `crates/savvagent/src/plugin/builtin/prompt_keybindings/mod.rs`,
  `crates/savvagent/src/plugin/builtin/keybindings_view.rs`, `CHANGELOG.md`.

---

## Task 1: Remove the ratatui-side view/edit/editor-keybindings feature

**Files:**

- Delete: `crates/savvagent/src/plugin/builtin/view_file/mod.rs`,
  `crates/savvagent/src/plugin/builtin/view_file/screen.rs`,
  `crates/savvagent/src/plugin/builtin/edit_file/mod.rs`,
  `crates/savvagent/src/plugin/builtin/edit_file/screen.rs`,
  `crates/savvagent/src/plugin/builtin/editor_keybindings/mod.rs`,
  `crates/savvagent/src/plugin/builtin/themes/editor_theme.rs`
- Modify: `crates/savvagent/src/app.rs`, `crates/savvagent/src/main.rs`,
  `crates/savvagent/src/ui.rs`, `crates/savvagent/src/plugin/effects.rs`,
  `crates/savvagent/src/plugin/mod.rs`, `crates/savvagent/src/plugin/builtin/mod.rs`,
  `crates/savvagent/src/plugin/builtin/themes/mod.rs`, `crates/savvagent/src/egui_app/convert.rs`

- [ ] Baseline: run `cargo test -p savvagent plugin::register_builtins_pr8_complete -- --nocapture`
      and confirm it currently passes with 30 plugin ids including `internal:view-file`,
      `internal:edit-file`, `internal:editor-keybindings`. This is the test that will fail (by
      design) once the plugins below are deregistered, and which this task updates at the end.
- [ ] In `crates/savvagent/src/plugin/mod.rs`: remove the three registration calls
      (`Box::new(builtin::edit_file::EditFilePlugin::new())`,
      `Box::new(builtin::editor_keybindings::EditorKeybindingsPlugin::new())`,
      `Box::new(builtin::view_file::ViewFilePlugin::new())`) and the "PR 4 adds: view-file,
      edit-file" comment. Update `register_builtins_pr8_complete`'s expected-id list (remove
      `internal:edit-file`, `internal:editor-keybindings`, `internal:view-file`) and its expected
      count (`assert_eq!(set.plugins.len(), 30)` → `27`). Also update the separate registry-size
      assertion later in the same test — `assert_eq!(reg.len(), 35, ...)` → `32` (30 non-provider +
      4 provider + 1 hook = 35 currently; 27 + 4 + 1 = 32 after this removal) — this is a distinct
      assertion from `set.plugins.len()` and is easy to miss.
- [ ] In `crates/savvagent/src/plugin/builtin/mod.rs`: remove the `pub mod edit_file;`,
      `pub mod editor_keybindings;`, `pub mod view_file;` declarations and their doc-comment
      mentions.
- [ ] **Relocate the shared `xterm_256_rgb` helper before touching `editor_theme.rs`'s module
      declaration or deleting the file.** `crates/savvagent/src/plugin/builtin/themes/editor_theme.rs`
      is not exclusively editor-support code despite its name: its `xterm_256_rgb` function is
      imported by `crates/savvagent/src/egui_app/convert.rs` (production code mapping every
      `ratatui::style::Color` the egui frontend renders — conversation log, screens, all of it — to
      an `egui::Color32`, not just code-editor syntax colors; see the spec's Premise corrections).
      Cut `xterm_256_rgb`'s full body from `editor_theme.rs` and paste it into
      `crates/savvagent/src/egui_app/convert.rs` as a private (`fn xterm_256_rgb`, no `pub(crate)`
      needed since it's now file-local) helper near `ratatui_color_to_color32` (its only caller).
      Update `convert.rs`'s `use crate::plugin::builtin::themes::editor_theme::xterm_256_rgb;`
      import — remove it, since the function is now local. Only `build_editor_theme`,
      `color_to_hex`, `indexed_to_hex`, and their tests remain in `editor_theme.rs` after this move
      (`indexed_to_hex` stays behind — it's a thin `editor_theme.rs`-local wrapper over
      `xterm_256_rgb` used only by `color_to_hex`, so it moves with the rest of the editor-only
      code, not with `xterm_256_rgb`). Run `cargo build -p savvagent --all-targets` now, before
      proceeding, to confirm the relocation alone compiles clean in both directions (nothing else
      in `editor_theme.rs` is deleted yet at this point — only `xterm_256_rgb` has moved).
- [ ] In `crates/savvagent/src/plugin/builtin/themes/mod.rs`: remove `pub mod editor_theme;`.
- [ ] Delete `crates/savvagent/src/plugin/builtin/view_file/mod.rs`,
      `crates/savvagent/src/plugin/builtin/view_file/screen.rs`,
      `crates/savvagent/src/plugin/builtin/edit_file/mod.rs`,
      `crates/savvagent/src/plugin/builtin/edit_file/screen.rs`,
      `crates/savvagent/src/plugin/builtin/editor_keybindings/mod.rs`,
      `crates/savvagent/src/plugin/builtin/themes/editor_theme.rs` (now containing only
      `build_editor_theme`/`color_to_hex`/`indexed_to_hex` and their tests, per the relocation step
      above — safe to delete whole-file) (and the now-empty `view_file/`/`edit_file/` directories).
- [ ] In `crates/savvagent/src/plugin/effects.rs`: remove the `Effect::CloseScreen` view-file/
      edit-file teardown block (the `if id == "edit-file" { app.save_file(); }` /
      `if id == "view-file" || id == "edit-file" { app.clear_active_editor(); }` lines — `CloseScreen`
      still pops the screen stack, just without the file-editor-specific teardown) and the
      `Effect::SaveActiveFile => { app.save_file(); }` match arm (falls through to the existing
      `other => { tracing::warn!(...) }` catch-all — safe, since `Effect` is `#[non_exhaustive]` and
      that catch-all already exists). In `open_screen`, remove the `file_path` pre-flight block
      (the `match (id, &args) { ("view-file", ScreenArgs::ViewFile { path }) | ("edit-file",
      ScreenArgs::EditFile { path }) => Some(path.clone()), _ => None }` match and the
      `if let Some(p) = &file_path { ... }` block that calls `load_file_into_editor`). Do not touch
      `ScreenArgs::ViewFile`/`EditFile` themselves yet — they're still valid enum variants at this
      point (removed in Task 3).
- [ ] In `crates/savvagent/src/app.rs`: remove the `InputMode::ViewingFile`/`InputMode::EditingFile`
      variants and their doc comments; the `/view`/`/edit` `Command` entries in `refresh_commands`;
      the `editor_theme_for_active`/`borrow_editor_theme`/`language_for_path` helper functions; the
      `load_file_into_editor`/legacy `open_file`/`save_file` methods; the `editor: Option<Editor>`
      field and its initializer; the `use ratatui_code_editor::editor::Editor;` import; the stale
      `/view` prefill test; and the `input_mode_label()` match arms for the two removed `InputMode`
      variants. **Do NOT remove `active_file_path: Option<PathBuf>` or the `clear_active_editor()`
      method yet** — `crates/savvagent/src/egui_app/widgets/editor.rs:145` still reads
      `app.active_file_path` and `crates/savvagent/src/egui_app/mod.rs:587` still calls
      `self.app.clear_active_editor()`, and the egui side isn't deleted until Task 2. Removing
      either now breaks `cargo build -p savvagent --all-targets` at the end of *this* task, since
      both frontends live in the same crate. Leaving them in place is safe: after this task's other
      deletions, nothing in the ratatui path still sets `active_file_path` to `Some(_)`, so it's
      inert (always `None`) for the one commit until Task 2 finishes the removal.
- [ ] In `crates/savvagent/src/main.rs`: remove the `view-file`/`edit-file` marker-screen
      pre-dispatch routing block (the `if top_id == "view-file" || top_id == "edit-file" { ... }`
      block that routes keys into `app.editor`) and the legacy `InputMode::ViewingFile`/
      `InputMode::EditingFile` key-handling match arms. Do **not** touch the `@`/`Ctrl+O`
      file-picker wiring — unrelated, out of scope (confirmed in the spec).
- [ ] In `crates/savvagent/src/ui.rs`: remove the `view-file`/`edit-file` screen-stack render branch,
      the legacy `InputMode::ViewingFile`/`EditingFile` popup-editor render branch, and the
      `paint_file_screen` function in full.
- [ ] Run `cargo build -p savvagent --all-targets` and fix any remaining compile errors (expect
      several — e.g. `notes.file-*` locale keys are still referenced by code paths that no longer
      exist; confirm each error traces back to code this step already intended to remove, not a
      missed dependency elsewhere).
- [ ] Run `cargo test -p savvagent`, `cargo clippy -p savvagent --all-targets`, `cargo fmt --all`.
      Expect green (the egui crate may still reference `editor_buffer`/`ScreenArgs::ViewFile` —
      that's fine, it isn't touched until Task 2, and `ScreenArgs`/`Effect` themselves are untouched
      until Task 3, so egui still compiles against the current ABI).
- [ ] Public-interface check: this task removes 3 slash commands (breaking, per Non-Negotiable
      Rule 6 — see spec). Not yet reflected in `CHANGELOG.md` (Task 5 adds the entry once all
      removal is complete) or the version bump (release-cut step, after merge).
- [ ] Host-swap / `RwLock` check: `app.rs`/`main.rs` are touched, but every change here is pure
      state/field/method removal with no new `.await` — no lock is introduced or held differently.
- [ ] `ProgressDispatcher` check: not applicable — no streaming provider path touched.
- [ ] Format and commit: `cargo fmt --all` then
      `git commit -m "savvagent: remove ratatui-side /view, /edit, /editor-keybindings"`.

## Task 2: Remove the egui-side view/edit feature

**Files:**

- Delete: `crates/savvagent/src/egui_app/widgets/editor.rs`,
  `crates/savvagent/src/egui_app/widgets/editor_theme.rs`
- Modify: `crates/savvagent/src/egui_app/mod.rs`, `crates/savvagent/src/egui_app/view.rs`,
  `crates/savvagent/src/egui_app/widgets/mod.rs`

- [ ] Baseline: run `cargo test -p savvagent egui_app -- --nocapture` and confirm the current egui
      editor-widget tests pass (they'll be deleted, not fixed, since the whole widget goes).
- [ ] In `crates/savvagent/src/egui_app/mod.rs`: remove the `editor_buffer:
      Option<widgets::editor::EditorBuffer>` field and its initializer, `save_editor_buffer`, and the
      marker-screen sync/`Ctrl-S`-interception block that syncs `editor_buffer` against the top
      screen and intercepts save on `edit-file`.
- [ ] In `crates/savvagent/src/egui_app/view.rs`: remove the `view-file`/`edit-file` marker-screen
      paint branch (`if id == "view-file" || id == "edit-file" { ... }`).
- [ ] In `crates/savvagent/src/egui_app/widgets/mod.rs`: remove the `pub mod editor;` and
      `pub mod editor_theme;` declarations and the doc-comment mention of the file editor (keep the
      `Ctrl+O` file-picker mention — `widgets::file_picker` stays, unrelated to this removal).
- [ ] Delete `crates/savvagent/src/egui_app/widgets/editor.rs` and
      `crates/savvagent/src/egui_app/widgets/editor_theme.rs` in full (including their inline
      `#[cfg(test)]` modules).
- [ ] In `crates/savvagent/src/app.rs`: now that the egui widgets above no longer reference them,
      remove the `active_file_path: Option<PathBuf>` field and its initializer, and the
      `clear_active_editor()` method (both deliberately left in place by Task 1 — see that task's
      note — because `egui_app/widgets/editor.rs:145` and `egui_app/mod.rs:587` still referenced
      them until the deletions above). Confirm via
      `grep -rn "active_file_path\|clear_active_editor" crates/savvagent/src` that only the
      definition sites existed before this step (zero remaining call sites in either frontend).
- [ ] Run `cargo build -p savvagent --all-targets` and fix any remaining compile errors. At this
      point, verify by grep that no code in `crates/savvagent` still constructs or matches
      `Effect::SaveActiveFile`, `ScreenArgs::ViewFile`, or `ScreenArgs::EditFile`:
      `grep -rEn "SaveActiveFile|ScreenArgs::ViewFile|ScreenArgs::EditFile" crates/savvagent/src`
      should return zero hits (this is the precondition Task 3 depends on).
- [ ] Run `cargo test -p savvagent`, `cargo clippy -p savvagent --all-targets`, `cargo fmt --all`.
      Expect green.
- [ ] Public-interface check: no additional slash-command/ABI surface change beyond Task 1 — this
      task removes the egui-side implementation of the same already-flagged breaking change.
- [ ] Host-swap / `RwLock` check: not applicable — `egui_app/mod.rs` changes here are pure
      field/method removal on `SavvagentApp`, no `Arc<RwLock<Option<Arc<Host>>>>` interaction.
- [ ] `ProgressDispatcher` check: not applicable.
- [ ] Format and commit: `cargo fmt --all` then
      `git commit -m "savvagent: remove egui-side /view, /edit editor widget"`.

## Task 3: Remove the plugin-ABI enum variants and their WASM adapter arms

**Files:**

- Modify: `crates/savvagent-plugin/src/effect.rs`, `crates/savvagent-plugin/src/types.rs`,
  `crates/savvagent-plugin-wasm/src/adapter/interactive.rs`

- [ ] Precondition check (repeat of Task 2's last grep, now against the whole workspace):
      `grep -rEn "SaveActiveFile|ScreenArgs::ViewFile|ScreenArgs::EditFile" crates/ --include=*.rs`
      must show hits **only** inside `crates/savvagent-plugin/src/effect.rs`,
      `crates/savvagent-plugin/src/types.rs`, and `crates/savvagent-plugin-wasm/src/adapter/interactive.rs`
      (the definitions and adapter arms this task is about to remove) — if any other crate still
      has a hit, stop and investigate before proceeding.
- [ ] In `crates/savvagent-plugin/src/effect.rs`: remove the `SaveActiveFile` variant from the
      `Effect` enum and its `Display` impl arm (`Effect::SaveActiveFile =>
      f.write_str("SaveActiveFile")`).
- [ ] In `crates/savvagent-plugin/src/types.rs`: remove the `ViewFile { path: String }` and
      `EditFile { path: String }` variants from `ScreenArgs`, their `screen_id()` match arms
      (`ScreenArgs::ViewFile { .. } => Some("view-file")`, `ScreenArgs::EditFile { .. } =>
      Some("edit-file")`), and their unit tests.
- [ ] In `crates/savvagent-plugin-wasm/src/adapter/interactive.rs`: remove the
      `ScreenArgs::ViewFile { path } => json!({"kind": "view-file", "path": path})` and
      `ScreenArgs::EditFile { path } => json!({"kind": "edit-file", "path": path})` match arms and
      their unit-test usages.
- [ ] Run `cargo build --workspace --all-targets` and fix any remaining compile errors (expect
      none, given the Task 2 precondition check, but confirm).
- [ ] Run `cargo test --workspace`, `cargo clippy --workspace --all-targets`, `cargo fmt --all`.
      Expect green.
- [ ] Public-interface check: this is the plugin-ABI half of the breaking change named in the spec
      — `Effect` and `ScreenArgs` are both `#[non_exhaustive]`, so no external plugin could have
      exhaustively matched them, but a variant removal still breaks source compatibility for any
      external plugin that *constructs* one of these variants. Flagged explicitly to the
      architecture reviewer in the PR's mandatory review trio (Phase 4 step 8) — do not let this
      pass review silently.
- [ ] Host-swap / `RwLock` check: not applicable.
- [ ] `ProgressDispatcher` check: not applicable.
- [ ] Format and commit: `cargo fmt --all` then
      `git commit -m "savvagent-plugin: remove SaveActiveFile, ScreenArgs::ViewFile/EditFile"`.

## Task 4: Remove the `ratatui-code-editor` / `egui_code_editor` dependencies

**Files:**

- Modify: `crates/savvagent/Cargo.toml`, `Cargo.toml` (root), `Cargo.lock`

- [ ] Precondition check: `grep -rn "ratatui_code_editor\|egui_code_editor" crates/ --include=*.rs`
      must return zero hits (both crates' only callers were deleted in Tasks 1–2).
- [ ] In `crates/savvagent/Cargo.toml`: remove `egui_code_editor.workspace = true` and
      `ratatui-code-editor.workspace = true`.
- [ ] In the root `Cargo.toml`'s `[workspace.dependencies]`: remove the `egui_code_editor` and
      `ratatui-code-editor` entries.
- [ ] Run `cargo check --workspace` to regenerate `Cargo.lock`. Diff the lockfile change
      (`git diff Cargo.lock`) and confirm it only removes `egui_code_editor`, `ratatui-code-editor`,
      and any transitive dependency that has no other reference in the tree (verify with
      `cargo tree --workspace -i <pkg>` for anything unexpected in the diff) — no unrelated version
      churn should appear.
- [ ] Run `cargo build --workspace --all-targets`, `cargo test --workspace`,
      `cargo clippy --workspace --all-targets`, `cargo fmt --all --check`. Expect green.
- [ ] Run `cargo tree --workspace` and confirm neither `egui_code_editor` nor `ratatui-code-editor`
      appears anywhere in the output.
- [ ] Public-interface check: dependency removal only, no additional surface change.
- [ ] Host-swap / `RwLock` check: not applicable.
- [ ] `ProgressDispatcher` check: not applicable.
- [ ] Format and commit: `cargo fmt --all` then
      `git commit -m "savvagent: drop ratatui-code-editor and egui_code_editor dependencies"`.

## Task 5: Update locales, README, PRD, and stale doc comments

**Files:**

- Modify: `crates/savvagent/locales/en.toml`, `es.toml`, `hi.toml`, `pt.toml`, `README.md`,
  `PRD.md`, `crates/savvagent/src/plugin/builtin/prompt_keybindings/mod.rs`,
  `crates/savvagent/src/plugin/builtin/keybindings_view.rs`, `CHANGELOG.md`

- [ ] Baseline: run `cargo test -p savvagent --test locales -- --nocapture` (or the equivalent
      `tests/locales.rs` invocation) and confirm it currently passes with the `picker.view-file.*`
      / `picker.edit-file.*` / `picker.editor-keybindings.*` / `slash.view-summary` /
      `slash.edit-summary` / `slash.editor-keybindings-summary` /
      `plugin.view-file-description` / `plugin.edit-file-description` /
      `plugin.editor-keybindings-description` keys present in all four locale files (this is the
      baseline the key-parity test still needs after removal — same key set across all four, just
      smaller).
- [ ] For each of `crates/savvagent/locales/en.toml`, `es.toml`, `hi.toml`, `pt.toml`: remove the
      `[picker.view-file]`, `[picker.edit-file]`, `[picker.editor-keybindings]` tables in full;
      remove `slash.view-summary`, `slash.edit-summary`, `slash.editor-keybindings-summary`,
      `plugin.view-file-description`, `plugin.edit-file-description`,
      `plugin.editor-keybindings-description`. Before removing any `notes.file-not-found` /
      `notes.file-editor-error` / `notes.file-read-error` / `notes.file-saved` /
      `notes.file-write-error` key, grep for remaining callers
      (`grep -rEn "notes\.file-not-found|notes\.file-editor-error|notes\.file-read-error|notes\.file-saved|notes\.file-write-error" crates/savvagent/src`)
      — remove only the keys with zero remaining callers after Tasks 1–2; if any still has a
      caller, leave it and note why in the commit message.
- [ ] Run `cargo test -p savvagent --test locales` (or the repo's actual locale-test invocation)
      and confirm the key-parity + placeholder-parity checks still pass across all four files.
- [ ] In `README.md`: remove `/view`, `/edit`, `/editor-keybindings` from the slash-command table
      and any surrounding prose describing the file editor/viewer.
- [ ] In `PRD.md`: update the M6 milestone's "TUI editor widget decision" bullet and the matching
      risks/open-questions "TUI editor widget" entry to reflect that `ratatui-code-editor` (and the
      view/edit feature it backed) has now been removed rather than consolidated — do **not**
      rewrite the M5/M6 historical milestone bullets that narrate `/view`/`/edit` shipping as part
      of that milestone's original scope (matches this repo's convention of not rewriting past
      `CHANGELOG.md` entries).
- [ ] In `crates/savvagent/src/plugin/builtin/prompt_keybindings/mod.rs`: reword the module doc
      comment's "Mirrors `internal:editor-keybindings` for symmetry" line (that plugin no longer
      exists) to describe the screen on its own terms.
- [ ] In `crates/savvagent/src/plugin/builtin/keybindings_view.rs`: remove the
      `internal:editor-keybindings` mention from the module doc comment's "Used by
      `internal:prompt-keybindings` and `internal:editor-keybindings`" line — the shared viewer is
      still used by `prompt-keybindings` and any future help screen, so the file itself is
      otherwise untouched.
- [ ] Add a `CHANGELOG.md` entry under `## [Unreleased]` → `### Removed`:
      "**`/view`, `/edit`, and `/editor-keybindings` slash commands removed**, along with the
      `ratatui-code-editor`/`egui_code_editor` in-TUI file editor they backed. Breaking change to
      the slash-command surface and the plugin ABI (`Effect::SaveActiveFile`,
      `ScreenArgs::ViewFile`/`EditFile` removed from `savvagent-plugin`)." (adjust wording as
      needed to match this repo's changelog voice — read the most recent `### Removed`/`### Fixed`
      entries first for tone).
- [ ] Run `cargo build --workspace --all-targets`, `cargo test --workspace`,
      `cargo clippy --workspace --all-targets`, `cargo fmt --all --check`. Expect green.
- [ ] Public-interface check: the `CHANGELOG.md` entry above is the required Non-Negotiable Rule 6
      documentation of the breaking change; the version bump itself happens at release-cut, not in
      this PR.
- [ ] Host-swap / `RwLock` check: not applicable.
- [ ] `ProgressDispatcher` check: not applicable.
- [ ] Format and commit: `cargo fmt --all` then
      `git commit -m "docs: remove /view, /edit, /editor-keybindings references"`.

## Task 6 (release, not part of this PR): cut the next MINOR release

Per `RELEASING.md` and this skill's Non-Negotiable Rule 8, after this task's PR merges to `main`,
open a separate `release/X-Y-Z` PR: confirm the actual next-unclaimed MINOR version against
`origin/main`'s current `Cargo.toml` (`v0.20.0` has already merged via PR #29 as of this plan's
writing, so `v0.21.0` is the current expected target — but re-check at cut time in case another
MINOR has landed in between), bump `workspace.package.version` (and matching
`workspace.dependencies` versions), move the `## [Unreleased]` content (including this task's
`### Removed` entry) under the new version heading in `CHANGELOG.md`, then tag and push once that PR
merges. Because this change is breaking (Non-Negotiable Rule 6), the bump **must** be MINOR, not
PATCH, regardless of what other unreleased changes have accumulated.
