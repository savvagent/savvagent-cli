# remove-view-edit Implementation Plan

**Goal:** Remove the `/view`, `/edit`, and `/editor-keybindings` slash commands and every line of
code, translation key, dependency, and doc reference that exists solely to support them — in both
the TUI (ratatui) and GUI (egui) frontends — leaving zero dead code, zero orphaned plugin-ABI
surface, and the unrelated `@`-inline-reference file picker and `/prompt-keybindings` screen fully
intact.

**Architecture:** `/view`/`/edit` are implemented as two built-in Screen plugins
(`crates/savvagent/src/plugin/builtin/view_file/`, `.../edit_file/`) that push marker screens with
id `"view-file"`/`"edit-file"` onto `App::screen_stack`; a syntax-highlighted `ratatui-code-editor`
`Editor` instance (`App::editor`) is populated by `App::load_file_into_editor` and rendered by
`ui.rs::paint_file_screen`, with dedicated key-routing in `main.rs`'s screen-stack block. A
completely dead legacy path (`InputMode::ViewingFile`/`EditingFile`, `App::open_file`, no callers)
sits alongside it. The GUI frontend has its own parallel implementation
(`egui_app/widgets/editor.rs` + `editor_theme.rs`, `egui_app/mod.rs`'s `editor_buffer` field,
`egui_app/view.rs`'s render branch) using `egui_code_editor` instead of `ratatui-code-editor`. The
plugin ABI (`savvagent-plugin`) exposes `ScreenArgs::ViewFile`/`EditFile` and `Effect::SaveActiveFile`
as the typed surface backing this; `savvagent-plugin-wasm`'s `interactive.rs` adapter projects
`ScreenArgs` to JSON for WASM-implemented screens. `/editor-keybindings`
(`plugin/builtin/editor_keybindings/`) exists solely to document the `view-file`/`edit-file`
keybindings and has no purpose once they're gone. This plan removes all of the above, innermost
(plugin ABI) first so intermediate `cargo build` runs surface the next broken caller instead of
cascading unreadably, then works outward through built-in-plugin registration, `App`/`main.rs`/
`ui.rs` (TUI), the GUI frontend, the `ratatui-code-editor` (and conditionally `egui_code_editor`)
dependency, then locales/README/PRD/CHANGELOG.

**Tech Stack:** Rust 2024, `savvagent-plugin`'s `Screen`/`Effect`/`ScreenArgs` ABI types,
`ratatui-code-editor` (removed) / `egui_code_editor` (removed if orphaned), `rust_i18n::t!` + the
four TOML locale files under `crates/savvagent/locales/`.

**Spec:** `docs/superpowers/specs/2026-09-05-remove-view-edit-design.md` — read it first (including
the "Premise corrections" section — the issue's own description of the code shape is out of date).
This plan implements it exactly.

**Release line:** the next **MINOR** version after whatever is latest-released at merge time (this is
a breaking-ABI removal, which per this repo's pre-1.0 convention — MINOR for features/breaking
changes, PATCH for fixes — must be a MINOR bump, not a patch). Do not hardcode a version number here;
confirm the exact next MINOR against `CHANGELOG.md`'s most recent released heading when the release
PR (Task 6's final note) is actually opened, since other work may land first.

**Branch:** `savvagent/remove-view-edit`

**File Map:**

- Modified: `crates/savvagent-plugin/src/types.rs` — remove `ScreenArgs::ViewFile`/`EditFile` and
  their `screen_id()` arms + tests.
- Modified: `crates/savvagent-plugin/src/effect.rs` — remove `Effect::SaveActiveFile` + its `Debug`
  arm + tests.
- Modified: `crates/savvagent-plugin-wasm/src/adapter/interactive.rs` — remove the
  `ScreenArgs::ViewFile`/`EditFile` JSON-projection arms + test fixtures.
- Deleted: `crates/savvagent/src/plugin/builtin/view_file/` (whole dir),
  `crates/savvagent/src/plugin/builtin/edit_file/` (whole dir),
  `crates/savvagent/src/plugin/builtin/editor_keybindings/` (whole dir).
- Modified: `crates/savvagent/src/plugin/builtin/mod.rs` — remove the three `pub mod` declarations
  and their doc comments (e.g. "Basic in-TUI file editor; opened via `/edit <path>`.").
- Modified: `crates/savvagent/src/plugin/mod.rs` — remove the three plugin constructions from
  `register_builtins`; update `register_builtins_pr8_complete` and any other test asserting the
  builtin ID list/count.
- Modified: `crates/savvagent/src/plugin/builtin/themes/editor_theme.rs` — remove
  `build_editor_theme`/`color_to_hex` (and their tests/docs), which exist solely for
  `ratatui-code-editor`'s syntax theme; **keep** `xterm_256_rgb` (and its tests) — it's a shared
  utility also consumed by `egui_app/convert.rs` for unrelated ANSI-color conversion.
- Modified: `crates/savvagent/src/app.rs` — remove `/view`/`/edit` `Command` entries,
  `InputMode::ViewingFile`/`EditingFile` + their debug-name arm, `App::open_file`,
  `App::load_file_into_editor`, `App::clear_active_editor`, `App::save_file`, `editor: Option<Editor>`
  field, `active_file_path` field (if confirmed orphaned), `editor_theme_for_active`,
  `borrow_editor_theme`, `language_for_path`, the `ratatui_code_editor::editor::Editor` import;
  retarget the `Prefill` test off `/view`.
- Modified: `crates/savvagent/src/main.rs` — remove the `top_id == "view-file" || "edit-file"`
  screen-stack special case and the `InputMode::ViewingFile`/`EditingFile` match arms.
- Modified: `crates/savvagent/src/ui.rs` — remove `paint_file_screen`, its call site, and the
  `InputMode::ViewingFile | InputMode::EditingFile` render blocks.
- Deleted: `crates/savvagent/src/egui_app/widgets/editor.rs`,
  `crates/savvagent/src/egui_app/widgets/editor_theme.rs`.
- Modified: `crates/savvagent/src/egui_app/widgets/mod.rs` — remove `pub mod editor;`/
  `pub mod editor_theme;` and update the module's doc comment (currently describes "the
  syntax-highlighted code editor (`view-file`/`edit-file` marker screens)" as a still-live widget).
- Modified: `crates/savvagent/src/egui_app/mod.rs` — remove `editor_buffer` field,
  `save_editor_buffer`, `ensure_buffer_for_active_screen` call, and the `id == "edit-file"` Ctrl-S
  special case.
- Modified: `crates/savvagent/src/egui_app/view.rs` — remove the
  `id == "view-file" || id == "edit-file"` render branch.
- Modified: `crates/savvagent/Cargo.toml` — remove `ratatui-code-editor.workspace = true` (and
  `egui_code_editor.workspace = true` if confirmed orphaned).
- Modified: root `Cargo.toml` — remove the `ratatui-code-editor` (and conditionally
  `egui_code_editor`) `[workspace.dependencies]` entries.
- Modified: `crates/savvagent/locales/{en,es,hi,pt}.toml` — remove orphaned keys (exact names
  confirmed by grep during implementation).
- Modified: `README.md` — remove the `/view`, `/edit`, `/editor-keybindings` table rows and the
  GUI-status paragraph's `/view`/`/edit` sentence.
- Modified: `PRD.md` — revise the "TUI editor widget" open-question entry.
- Modified: `CHANGELOG.md` — add a `### Removed` entry under `[Unreleased]`.

## Task 1: Remove the plugin-ABI surface (`savvagent-plugin`, `savvagent-plugin-wasm`)

**Files:**
- Modify: `crates/savvagent-plugin/src/types.rs`
- Modify: `crates/savvagent-plugin/src/effect.rs`
- Modify: `crates/savvagent-plugin-wasm/src/adapter/interactive.rs`

- [x] Baseline: `cargo test --workspace --no-fail-fast` — confirm green before touching anything, and
      record the total test count (this is the pre-change baseline referenced in the spec's Risks
      section and in Task 6's before/after comparison — a full-workspace run is needed here, not
      just the two plugin crates, since Task 2 deletes tests in `savvagent` too).
- [x] In `crates/savvagent-plugin/src/types.rs`: delete the `ScreenArgs::ViewFile { path: String }`
      and `ScreenArgs::EditFile { path: String }` variants, their two arms in `screen_id()`
      (`Some("view-file")`/`Some("edit-file")`), and every test referencing them (the `_view`/`_edit`
      constructions around line 518, and the `screen_id()` assertions around lines 627/631).
- [x] In `crates/savvagent-plugin/src/effect.rs`: delete the `Effect::SaveActiveFile` variant, its
      `Debug` arm (`Effect::SaveActiveFile => f.write_str("SaveActiveFile")`), and any test
      referencing it.
- [x] In `crates/savvagent-plugin-wasm/src/adapter/interactive.rs`: delete the
      `ScreenArgs::ViewFile { path } => json!({"kind": "view-file", "path": path})` and the matching
      `EditFile` arm, and the two test-fixture lines constructing
      `ScreenArgs::ViewFile`/`EditFile` (around lines 902-903 and wherever their corresponding
      assertions live).
- [x] Run `cargo check -p savvagent --all-targets` — expect compile errors pointing at every
      downstream caller (this is expected; Task 2/3/4 fix them). `cargo build`/`cargo check` scoped
      to `-p savvagent-plugin -p savvagent-plugin-wasm` would only build those two crates and their
      dependencies, not the reverse-dependent `savvagent` crate, so it cannot surface these errors —
      use `-p savvagent --all-targets` instead to pull in the whole downstream graph (including
      `main.rs`, which `--lib` alone would skip). Do not attempt to fix downstream callers from this
      task — just confirm the errors are exactly the expected ones (`edit_file`/`view_file`/
      `editor_keybindings` plugin modules, `crates/savvagent/src/plugin/effects.rs`,
      `crates/savvagent/src/app.rs`).
- [x] Run `cargo test -p savvagent-plugin -p savvagent-plugin-wasm` — expect green (these two crates
      compile standalone even though the workspace doesn't yet).
- [x] Public-interface check: this is the **breaking plugin-ABI change** — record in the PR body
      that `ScreenArgs::ViewFile`/`EditFile` and `Effect::SaveActiveFile` were removed from
      `savvagent-plugin`, per Non-Negotiable Rule 6. Flag explicitly to the architecture reviewer in
      Phase 4 step 8.
- [x] Host-swap/RwLock check: not applicable — no `app.rs`/`tui.rs` changes in this task.
- [x] ProgressDispatcher check: not applicable — no streaming provider path touched.
- [x] Format and commit: `cargo fmt --all` then
      `git commit -m "savvagent-plugin: remove ScreenArgs::ViewFile/EditFile and Effect::SaveActiveFile"`.

## Task 2: Remove the built-in `view_file`/`edit_file`/`editor_keybindings` plugins and their registration

**Files:**
- Delete: `crates/savvagent/src/plugin/builtin/view_file/` (dir)
- Delete: `crates/savvagent/src/plugin/builtin/edit_file/` (dir)
- Delete: `crates/savvagent/src/plugin/builtin/editor_keybindings/` (dir)
- Modify: `crates/savvagent/src/plugin/builtin/mod.rs`
- Modify: `crates/savvagent/src/plugin/mod.rs`

- [x] `rm -rf crates/savvagent/src/plugin/builtin/view_file crates/savvagent/src/plugin/builtin/edit_file crates/savvagent/src/plugin/builtin/editor_keybindings`.
- [x] In `crates/savvagent/src/plugin/builtin/mod.rs`: remove the `pub mod edit_file;`,
      `pub mod editor_keybindings;`, `pub mod view_file;` declarations.
- [x] In `crates/savvagent/src/plugin/mod.rs::register_builtins`: remove the three
      `Box::new(builtin::edit_file::EditFilePlugin::new())`,
      `Box::new(builtin::editor_keybindings::EditorKeybindingsPlugin::new())`,
      `Box::new(builtin::view_file::ViewFilePlugin::new())` entries from the returned plugin vec.
      Update `register_builtins_pr8_complete` (and any other test in this file asserting an exact
      builtin plugin count or enumerating builtin IDs, e.g. via `grep -n "internal:edit-file\|internal:view-file\|internal:editor-keybindings" crates/savvagent/src/plugin/mod.rs`)
      to drop the three removed IDs and decrement the expected count by 3.
- [x] `cargo check -p savvagent --all-targets` (this crate won't fully build yet — `app.rs`/`main.rs`/`ui.rs`/
      `effects.rs`/egui files still reference removed items; Task 3/4 fix those) — confirm the only
      remaining errors are in those known files, not stray references to the deleted plugin modules.
- [x] Format and commit (even though the crate doesn't build yet, this repo's task-by-task discipline
      still commits at natural checkpoints — the plan converges to a green build by the end of Task
      4): `cargo fmt --all` then
      `git commit -m "savvagent: remove view-file/edit-file/editor-keybindings builtin plugins"`.

## Task 3: Remove TUI (ratatui) state, key-routing, and rendering

**Files:**
- Modify: `crates/savvagent/src/app.rs`
- Modify: `crates/savvagent/src/main.rs`
- Modify: `crates/savvagent/src/ui.rs`
- Modify: `crates/savvagent/src/plugin/effects.rs`

- [x] In `crates/savvagent/src/plugin/effects.rs`: in `apply_one`'s `Effect::CloseScreen` arm, remove
      the `if id == "edit-file" { app.save_file(); }` and
      `if id == "view-file" || id == "edit-file" { app.clear_active_editor(); }` blocks (and the
      `Effect::SaveActiveFile => { app.save_file(); }` match arm, now unreachable since the variant
      is gone — the compiler will flag this as the next error to fix). In `open_screen`, remove the
      `file_path` pre-flight block (the `match (id, &args) { ("view-file", ScreenArgs::ViewFile { path }) | ("edit-file", ScreenArgs::EditFile { path }) => Some(path.clone()), _ => None }`
      and the `if let Some(p) = &file_path { ... }` block that calls `app.load_file_into_editor`).
- [x] In `crates/savvagent/src/app.rs`:
  - Remove the `/view` and `/edit` `Command` entries from `refresh_commands`.
  - Remove `InputMode::ViewingFile`/`InputMode::EditingFile` variants (and their doc comments +
    `#[allow(dead_code)]`), and their arm in the mode-name debug helper (~line 2812).
  - Remove `App::open_file` (fully dead — confirm zero callers with
    `grep -rn "\.open_file(" crates/` before deleting, matching the spec's premise correction 2),
    `App::load_file_into_editor`, `App::clear_active_editor`, `App::save_file`, the
    `editor: Option<Editor>` field and its `editor: None` initializer, `editor_theme_for_active`,
    `borrow_editor_theme`, `language_for_path`, and `use ratatui_code_editor::editor::Editor;`.
  - Grep `active_file_path` across the whole crate (`grep -rn "active_file_path" crates/savvagent/src/`)
    — if every remaining reference is inside code already being deleted in this task, remove the
    field and its initializer too; otherwise leave it and note why in the commit message.
  - Retarget the `select_arg_command_returns_prefill_with_seeded_input` test (~line 2660): replace
    the `/view`-filtering setup with a still-existing needs-arg command. `/bash` is the best
    candidate (needs_arg: true, alphabetically distinct enough to isolate with a filter) — e.g.
    `app.palette_filter = "ba".into()` then assert
    `CommandSelection::Prefill("/bash".into())` and `app.input_textarea.lines() == &["/bash ".to_string()]`.
    Confirm the filter string only matches one command before relying on `command_index = 0`.
  - Confirm `App::open_file_picker`, `is_file_picker_active`, `file_picker_select`,
    `close_file_picker`, and `App::file_explorer` are untouched (they back the unrelated `@`
    file-reference feature per the spec's premise correction 1) — do not remove them.
- [x] In `crates/savvagent/src/main.rs`:
  - Remove the `if top_id == "view-file" || top_id == "edit-file" { ... }` block in the screen-stack
    key-routing section (including its `is_close`/`is_save_in_edit`/`editor.input(...)` logic).
  - Remove the `InputMode::ViewingFile => match key.code { ... }` and
    `InputMode::EditingFile => match key.code { ... }` arms in the main `match app.input_mode` block.
- [x] In `crates/savvagent/src/ui.rs`:
  - Remove `fn paint_file_screen(...)` entirely.
  - Remove its call site: the `is_file_screen`/`paint_file_screen(...)` branch in the screen-stack
    paint block (keep the `else` branch's `paint_screen(...)` call as the only path).
  - Remove the `InputMode::ViewingFile | InputMode::EditingFile` render block (the popup + editor
    widget block) and the separate `if matches!(app.input_mode, InputMode::EditingFile) { ... }`
    cursor-position block right after it.
- [x] In `crates/savvagent/src/plugin/builtin/themes/editor_theme.rs`: remove `build_editor_theme`,
      `color_to_hex`, `indexed_to_hex`, their module-level doc comment, and every test that exercises
      them (`rgb_color_round_trips`, `named_colors_have_stable_hex`,
      `reset_falls_back_to_caller_provided_default`, `indexed_system_colors_match_named_equivalents`,
      `indexed_rgb_cube_uses_xterm_step_values`, `indexed_grayscale_ramp_steps_by_ten` insofar as
      they only test `color_to_hex`/`indexed_to_hex`, `build_editor_theme_includes_every_required_token_kind`,
      `build_editor_theme_emits_hex_color_strings`, `dark_and_light_themes_produce_different_string_colors`).
      **Keep** `pub(crate) fn xterm_256_rgb` and its doc comment (drop only the sentence naming the
      now-removed code-editor sink, keep the sentence about the shared egui sink) — it is still used
      by `crates/savvagent/src/egui_app/convert.rs` (`use crate::plugin::builtin::themes::editor_theme::xterm_256_rgb;`)
      for unrelated ANSI-color conversion. If any of the `indexed_to_hex`-testing assertions above
      were actually testing `xterm_256_rgb` values indirectly, rewrite them to call `xterm_256_rgb`
      directly instead of deleting the coverage.
- [x] Run `cargo check -p savvagent --all-targets` — expect the remaining errors to be confined to
      `crates/savvagent/src/egui_app/*.rs` (Task 4). If any error remains in `app.rs`/`main.rs`/
      `ui.rs`/`effects.rs`/`themes/editor_theme.rs`, it means a reference was missed — fix it before
      proceeding.
- [x] Host-swap `RwLock` check (mandatory — this task touches `app.rs`): confirm no `.await` executes
      while `Arc<RwLock<Option<Arc<Host>>>>` (or `Host`'s internal `pool`/`active_provider` locks) is
      held across any edit in this task. This task is pure deletion of synchronous state/rendering
      code with no lock interaction, so this should be trivially satisfied — state that explicitly
      in the commit message rather than skipping the check.
- [x] ProgressDispatcher check: not applicable — no streaming provider path touched.
- [x] Format and commit: `cargo fmt --all` then
      `git commit -m "savvagent: remove view-file/edit-file TUI state, key-routing, and rendering"`.

## Task 4: Remove the GUI (egui) frontend's parallel implementation

**Files:**
- Delete: `crates/savvagent/src/egui_app/widgets/editor.rs`
- Delete: `crates/savvagent/src/egui_app/widgets/editor_theme.rs`
- Modify: `crates/savvagent/src/egui_app/widgets/mod.rs`
- Modify: `crates/savvagent/src/egui_app/mod.rs`
- Modify: `crates/savvagent/src/egui_app/view.rs`

- [x] Before deleting, confirm `crates/savvagent/src/egui_app/widgets/editor_theme.rs` is genuinely
      the GUI-only file and not confused with the TUI's similarly-named
      `crates/savvagent/src/plugin/builtin/themes/editor_theme.rs` (a different file, untouched by
      this plan) — verify by path, not by content similarity.
  - `rm crates/savvagent/src/egui_app/widgets/editor.rs crates/savvagent/src/egui_app/widgets/editor_theme.rs`.
- [x] In `crates/savvagent/src/egui_app/widgets/mod.rs`: remove `pub mod editor;` and
      `pub mod editor_theme;` (keep `pub mod canvas;` and `pub mod file_picker;`).
- [x] In `crates/savvagent/src/egui_app/mod.rs`: remove the `editor_buffer: Option<widgets::editor::EditorBuffer>`
      field and its `editor_buffer: None` initializer, `fn save_editor_buffer(&mut self)`, the
      `widgets::editor::ensure_buffer_for_active_screen(&mut self.editor_buffer, &self.app)` call,
      and the `id == "edit-file"` Ctrl-S special-case block (including its
      `self.save_editor_buffer()` / `self.app.clear_active_editor()` calls — `clear_active_editor`
      itself was already removed in Task 3, so this block must go first or in the same commit).
- [x] In `crates/savvagent/src/egui_app/view.rs`: remove the
      `if id == "view-file" || id == "edit-file" { ... }` render branch (including its
      `let editable = id == "edit-file";` line and whatever it dispatches to).
- [x] Run `cargo build --workspace --all-targets` — expect a fully green build across the entire
      workspace at this point (this is the first task where every known caller has been addressed).
      If any error remains, it's a reference this plan's file map missed — grep
      `view-file\|edit-file\|ViewFile\|EditFile\|SaveActiveFile\|editor_buffer` across
      `crates/savvagent/src/` to find it before proceeding.
- [x] Host-swap `RwLock` check: not applicable to `egui_app/*.rs` (the host-swap rule is specific to
      `app.rs`/`tui.rs`'s `Arc<RwLock<Option<Arc<Host>>>>` slot) — but confirm this task didn't touch
      that slot at all (it shouldn't have; this is GUI-widget-only cleanup).
- [x] Format and commit: `cargo fmt --all` then
      `git commit -m "savvagent: remove view-file/edit-file GUI (egui) frontend"`.

## Task 5: Remove the `ratatui-code-editor` dependency, orphaned locale keys, and update docs

**Files:**
- Modify: `crates/savvagent/Cargo.toml`
- Modify: root `Cargo.toml`
- Modify: `crates/savvagent/locales/en.toml`, `es.toml`, `hi.toml`, `pt.toml`
- Modify: `README.md`
- Modify: `PRD.md`
- Modify: `CHANGELOG.md`

- [x] Confirm zero remaining references: `grep -rn "ratatui_code_editor\|ratatui-code-editor" crates/ Cargo.toml`
      should return only the two `Cargo.toml` dependency lines. Remove
      `ratatui-code-editor.workspace = true` from `crates/savvagent/Cargo.toml` and the
      `ratatui-code-editor = "0.0.3"` entry from root `Cargo.toml`'s `[workspace.dependencies]`.
- [x] Check `egui_code_editor`: `grep -rn "egui_code_editor" crates/` — if the only remaining
      reference (besides the two `Cargo.toml` lines) was in the deleted `egui_app/widgets/editor.rs`,
      remove `egui_code_editor.workspace = true` from `crates/savvagent/Cargo.toml` and the
      `egui_code_editor = "=0.2.17"` entry (plus its preceding comment explaining the `=` pin) from
      root `Cargo.toml`. If any other file still references it, leave the dependency and note why in
      the commit message.
- [x] Run `cargo check --workspace` once to regenerate `Cargo.lock` after the dependency removal(s).
- [x] For each of `crates/savvagent/locales/{en,es,hi,pt}.toml`: grep for `view-file`, `edit-file`,
      `editor-keybindings`, `file-not-found`, `file-editor-error`, `file-read-error`,
      `file-write-error`, `file-saved` (adjust the exact key stems to whatever the actual TOML key
      names are — confirm per-file, since the spec's list is best-effort). Remove every key with zero
      remaining `rust_i18n::t!("...")` callers (re-grep the Rust source after Tasks 1-4 to confirm
      each key truly has no caller before deleting) in all four files, keeping them structurally
      consistent with each other (same keys removed from each).
- [x] Run `cargo test -p savvagent --test locales` (the locale-parity integration test lives at
      `crates/savvagent/tests/locales.rs`) to confirm removing keys from all four files in lockstep
      didn't break locale-parity checks.
- [x] In `README.md`: remove the `/view <path>`, `/edit <path>`, and `/editor-keybindings` rows from
      the "Other slash commands" table, and the `/view <path>` and `/edit <path>` sentence from the
      experimental-GUI status paragraph (~lines 110-113) — rephrase the surrounding sentence so it
      reads coherently without those two commands (do not leave a dangling "and" or orphaned clause).
- [x] In `PRD.md`: revise the "TUI editor widget" open-question bullet — replace "`ratatui-code-editor`
      remains for the in-TUI viewer/editor; consolidating onto a single widget is a future cleanup,
      not a release blocker" with language reflecting that the in-TUI viewer/editor (`/view`/`/edit`)
      was removed rather than consolidated, referencing this change.
- [x] In `CHANGELOG.md`: add, under `## [Unreleased]`, a `### Removed` section (create it if it
      doesn't already exist under Unreleased) with an entry naming: the `/view`, `/edit`, and
      `/editor-keybindings` slash commands; the breaking removal of `ScreenArgs::ViewFile`/`EditFile`
      and `Effect::SaveActiveFile` from the `savvagent-plugin` ABI, with the one-line migration note
      from the spec ("no replacement — plugins should not ask the runtime to open a
      file-viewer/editor screen; if a plugin genuinely needs this, it should ship its own Screen
      implementation"); and the removal of the `ratatui-code-editor` dependency (and
      `egui_code_editor`, if removed).
- [x] Public-interface check: this task is where the slash-command removal and the CHANGELOG
      breaking-change note actually land — confirm both are present before moving on.
- [x] Format and commit: `cargo fmt --all` then
      `git commit -m "docs: remove /view, /edit, /editor-keybindings from README/PRD/CHANGELOG and drop ratatui-code-editor"`.

## Task 6: Full-workspace verification

**Files:** none (verification only).

- [x] `cargo build --workspace --all-targets` — expect clean.
- [x] `cargo clippy --workspace --all-targets` with `RUSTFLAGS=-D warnings` (matching CI) — expect
      clean, in particular no dead-code/unused-import/unused-dependency warnings from this removal.
- [x] `cargo fmt --all --check` — expect clean.
- [x] `cargo test --workspace --no-fail-fast` — expect green. Note the before/after test count in the
      PR body (per the spec's Risks section) so reviewers aren't surprised by a smaller number —
      compare against the Task 1 baseline plus whatever `register_builtins` test count changed in
      Task 2.
- [x] Manual smoke check (out-of-band, TUI runtime wiring is touched by this plan): `cargo build --workspace`
      then `cargo run -p savvagent`, open the command palette (`/`) and confirm `/view`, `/edit`, and
      `/editor-keybindings` no longer appear, `@` still opens the file-reference picker, and
      `/prompt-keybindings` still opens correctly.
- [x] Confirm no `SAVVAGENT_TOOL_*` / provider / plugin-ABI-adjacent path was inadvertently touched
      outside what this plan lists (`git diff --stat origin/main` sanity check against the File Map
      above).
- [x] **Architecture-reviewer callout (mandatory):** the PR description and the architecture-review
      dispatch (Phase 4 step 8 of the `savvagent-development` skill) must explicitly name the
      removal of `ScreenArgs::ViewFile`, `ScreenArgs::EditFile`, and `Effect::SaveActiveFile` from
      the `savvagent-plugin` public ABI, and the removal of the `/view`, `/edit`, and
      `/editor-keybindings` slash commands, as intentional breaking public-interface changes per
      Non-Negotiable Rule 6 — do not let this land as an unremarked incidental deletion.
- [x] **Release note (do not perform here):** per `RELEASING.md` and this skill's Non-Negotiable Rule
      8, after this PR merges to `main`, open a separate `release/X-Y-Z` PR that bumps
      `workspace.package.version` (and matching `workspace.dependencies` versions) to the next
      **MINOR** version (this is a breaking-ABI removal, not a patch — confirm the exact next MINOR
      number against whatever has already shipped/queued by merge time) and moves this PR's
      `CHANGELOG.md` `[Unreleased]` content under the new version heading, then tags and pushes
      `vX.Y.0` once that PR merges.
