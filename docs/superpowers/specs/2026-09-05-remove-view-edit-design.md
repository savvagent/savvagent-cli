# Remove `/view` and `/edit` slash commands — design

Date: 2026-09-05
Status: IMPLEMENTED
Related: `savvagent/savvagent-cli#22`

## Problem

`/view` and `/edit` open a syntax-highlighted popup file viewer/editor (TUI: `ratatui-code-editor`
via `App::editor`; GUI: `egui_code_editor` via `SavvagentApp::editor_buffer`) that duplicates a
narrowly-scoped, non-core surface inconsistent with the project's "blazing-fast, narrowly focused
TUI" goal (`PRD.md`). It also drags a non-trivial amount of code and a whole dependency
(`ratatui-code-editor`) into the default TUI binary for a feature that isn't this project's job —
editing files is the model's job (via `tool-fs`), not the TUI's.

Issue #22 asks to remove `/view`/`/edit` and their associated code: the `Command` entries in
`App::refresh_commands`, `App::open_file_picker`/file-picker wiring "used only by these commands",
the `view-file`/`edit-file` screen-stack handling, `InputMode::ViewingFile`/`InputMode::EditingFile`,
the `app.editor` field/dependency if unused elsewhere, and related tests/docs.

### Premise corrections (read before implementing)

The issue's description of the code shape is slightly out of date relative to the current
repository — this section is authoritative over the issue text where they disagree:

1. **`App::open_file_picker`/`is_file_picker_active`/`file_picker_select`/`close_file_picker` are
   NOT used only by `/view`/`/edit`.** They back the `@` inline file-reference picker (typing `@` in
   the prompt, `crates/savvagent/src/main.rs:3503`), a separate, unrelated feature (confirmed: no
   caller of `App::open_file_picker` exists anywhere except that `@` key handler). **These stay.**
   The GUI's `Ctrl+O` file picker (`crates/savvagent/src/egui_app/widgets/file_picker.rs`) is the
   same `@`-reference feature for the GUI frontend and also stays.
2. **`App::open_file`/`App::input_mode = InputMode::ViewingFile/EditingFile` is already 100% dead
   code.** Grep confirms zero callers of `App::open_file` anywhere in the workspace outside its own
   definition; nothing ever sets `input_mode` to `ViewingFile`/`EditingFile` except `open_file`
   itself. The actually-reachable `/view`/`/edit` implementation is a **different, newer path**: two
   built-in Screen plugins (`crates/savvagent/src/plugin/builtin/view_file/`,
   `.../edit_file/`) that push marker screens with id `"view-file"`/`"edit-file"` onto
   `App::screen_stack`; `App::load_file_into_editor`/`App::clear_active_editor` (not
   `open_file`/nothing) populate/clear `App::editor` for that path, and dedicated code in
   `main.rs`'s screen-stack key-routing block and `ui.rs::paint_file_screen` render it. Both the
   dead legacy path (`InputMode::ViewingFile`/`EditingFile`, `App::open_file`) and the live plugin
   path (`"view-file"`/`"edit-file"` screens) are in scope for removal — the dead one because it's
   dead code the AC explicitly calls out, the live one because it's the actual `/view`/`/edit`
   feature.
3. **The GUI (egui) frontend has its own, separate implementation of the same feature** —
   `crates/savvagent/src/egui_app/widgets/editor.rs` (an `egui_code_editor`-backed buffer),
   `editor_theme.rs`, wiring in `egui_app/mod.rs` (`editor_buffer` field, `save_editor_buffer`,
   `ensure_buffer_for_active_screen`) and `egui_app/view.rs` (renders when the top screen id is
   `"view-file"`/`"edit-file"`). The issue text doesn't mention the GUI frontend (it predates the
   v0.19.0 GUI work), but per the AC ("no dead code remains", `cargo clippy --workspace --all-targets`
   clean) this must be removed too — it becomes unreachable dead code the moment the two screen
   plugins that create `"view-file"`/`"edit-file"` screens are removed, in both frontends.
4. **`/editor-keybindings` becomes meaningless dead functionality once `/view`/`/edit` are removed.**
   `crates/savvagent/src/plugin/builtin/editor_keybindings/` exists solely to document keybindings
   "active inside the `view-file`/`edit-file` screens" (its own module doc). The issue doesn't name
   it explicitly, but leaving a slash command whose entire content describes a now-nonexistent
   screen violates the "no dead code" AC and would be confusing/misleading to users. **In scope for
   removal**, alongside its README row and locale keys. `/prompt-keybindings` (a different,
   unrelated screen) is untouched.

## Approach

Remove, in dependency order (innermost/leaf code first so intermediate `cargo build` runs stay
informative rather than cascading):

1. **`savvagent-plugin` (plugin ABI):** remove `ScreenArgs::ViewFile { path }` and
   `ScreenArgs::EditFile { path }` variants and their `screen_id()` arms
   (`crates/savvagent-plugin/src/types.rs`), and the `Effect::SaveActiveFile` variant (its `Debug`
   arm and any tests) in `crates/savvagent-plugin/src/effect.rs` — `SaveActiveFile` exists solely to
   let the `edit-file` screen request a save-on-Ctrl-S and has no purpose once that screen is gone.
   Remove the associated tests for all three. This is the breaking plugin-ABI change here — see
   "Public-interface changes" below.
2. **`savvagent-plugin-wasm`:** remove the two `ScreenArgs::ViewFile`/`EditFile` match arms in the
   host→WASM JSON projection (`crates/savvagent-plugin-wasm/src/adapter/interactive.rs`, used when
   the host hands a `ScreenArgs` to a WASM-implemented screen at creation time — **not** a
   guest-to-host request path; see the corrected "Public-interface changes" note below) and their
   test fixtures.
3. **Built-in plugins:** delete `crates/savvagent/src/plugin/builtin/view_file/`,
   `.../edit_file/`, and `.../editor_keybindings/` wholesale (each is a self-contained
   `mod.rs`/`screen.rs` pair; `editor_keybindings` is `mod.rs` only). Remove their `pub mod`
   declarations in `crates/savvagent/src/plugin/builtin/mod.rs`, and — this is the file that
   actually constructs and registers them, not `builtin/mod.rs` — remove the
   `Box::new(builtin::edit_file::EditFilePlugin::new())`,
   `Box::new(builtin::editor_keybindings::EditorKeybindingsPlugin::new())`, and
   `Box::new(builtin::view_file::ViewFilePlugin::new())` entries from the plugin vec built in
   `crates/savvagent/src/plugin/mod.rs::register_builtins`, and update that function's tests (at
   least `register_builtins_pr8_complete` and any other test asserting the exact builtin-plugin
   count or ID list) to drop the three removed IDs and decrement the expected count.
4. **`App` (TUI) state, `crates/savvagent/src/app.rs`:**
   - Remove the `/view`/`/edit` `Command` entries in `refresh_commands`.
   - Remove `InputMode::ViewingFile`/`InputMode::EditingFile` variants and their `#[allow(dead_code)]`
     doc comments, and their match arm in the `Debug`-ish mode-name helper (~line 2812).
   - Remove `App::open_file` (fully dead), `App::load_file_into_editor`, `App::clear_active_editor`,
     `App::save_file`, the `editor: Option<Editor>` field, the `active_file_path` field **if** it has
     no other reader after this change (verify — grep first, it's currently only touched by the
     file-editor code paths being removed), `editor_theme_for_active`, `borrow_editor_theme`,
     `language_for_path`, and the `use ratatui_code_editor::editor::Editor;` import.
   - Update the one test asserting `CommandSelection::Prefill("/view".into())`
     (`crates/savvagent/src/app.rs:2666`) to exercise a command that still exists (e.g. `/edit` was
     the pairing target before — pick `/save` or another argless/needs-arg command already in the
     list; a needs-arg command like `/bash` preserves the same "prefill" code path coverage).
   - `App::open_file_picker`/`is_file_picker_active`/`file_picker_select`/`close_file_picker` and
     `App::file_explorer` **stay** (premise correction 1).
5. **`main.rs` key routing:**
   - Remove the screen-stack special-case block for `top_id == "view-file" || "edit-file"`
     (~line 3234–3250) — once the two screens no longer exist, `top_id` can never equal those
     strings, but the branch's dead condition + the `editor.input(...)` call it guards must go so
     clippy doesn't flag unreachable/dead logic and so the `App::editor`-touching code compiles away
     cleanly.
   - Remove the `InputMode::ViewingFile => …` and `InputMode::EditingFile => …` match arms in the
     main key-handling `match app.input_mode` block (~line 3668–3701).
6. **`ui.rs` rendering:** remove `paint_file_screen` and its call site (the `is_file_screen`
   branch in the screen-stack paint block), and the `InputMode::ViewingFile | InputMode::EditingFile`
   render block (popup + cursor-position blocks, ~lines 1167–1235 per the current file).
7. **GUI (egui) frontend:** delete `crates/savvagent/src/egui_app/widgets/editor.rs` and
   `editor_theme.rs` (verify `editor_theme.rs` has no other caller — the TUI's own
   `editor_theme_for_active`/`build_editor_theme` helper in `plugin/builtin/themes/editor_theme.rs`
   is a **different, similarly-named file**; do not confuse the two), remove the `pub mod editor;`
   / `pub mod editor_theme;` declarations in `egui_app/widgets/mod.rs` (keep `canvas`/`file_picker`),
   remove `editor_buffer` field + `save_editor_buffer`/`ensure_buffer_for_active_screen` wiring and
   the `id == "edit-file"` Ctrl-S special-case in `egui_app/mod.rs`, and the
   `id == "view-file" || id == "edit-file"` render branch in `egui_app/view.rs`.
8. **`Cargo.toml` dependency:** remove `ratatui-code-editor.workspace = true` from
   `crates/savvagent/Cargo.toml` and the corresponding `[workspace.dependencies]` entry in the root
   `Cargo.toml`, **only after** confirming (grep) zero remaining references anywhere in the
   workspace. Check whether `egui_code_editor` (the GUI equivalent) is also now unused and remove it
   too under the same condition.
9. **Locales:** remove the now-orphaned translation keys from all four locale files
   (`crates/savvagent/locales/{en,es,hi,pt}.toml`) — `slash.view-summary`/`slash.edit-summary` (or
   however named; confirm exact keys during implementation), `notes.file-not-found`,
   `notes.file-editor-error`, `notes.file-read-error`, `notes.file-write-error`, `notes.file-saved`,
   `picker.view-file.*`, `picker.edit-file.*`, `picker.editor-keybindings.*`, and the
   `plugin.editor-keybindings-description`/`slash.editor-keybindings-summary` keys — **only the keys
   with zero remaining `rust_i18n::t!` callers** after steps 1–7; a stray shared key must not be
   deleted out from under a surviving caller.
10. **README.md:** remove the `/view <path>`/`/edit <path>`/`/editor-keybindings` rows from the
    "Other slash commands" table, the GUI-status paragraph's `/view`/`/edit` sentence
    (lines ~110–113), and the `@` "file picker" note tweak only if its wording referenced `/view`
    (verify; it currently reads as a general note and likely needs no change beyond removing the
    two table rows).
11. **CHANGELOG.md:** add a `### Removed` entry under `[Unreleased]` naming the breaking plugin-ABI
    change explicitly (see below).
12. **`PRD.md`:** update the "TUI editor widget" open-question entry (`PRD.md`'s risks/open-questions
    section, currently reads "`ratatui-code-editor` remains for the in-TUI viewer/editor;
    consolidating onto a single widget is a future cleanup") — it is present-tense product direction
    that becomes false once this ships. Revise to reflect that the in-TUI viewer/editor was removed
    (not consolidated) rather than deleting the historical record outright.

## Scope

**In:**
- Removing the `/view`, `/edit`, and `/editor-keybindings` slash commands and every code path that
  exists solely to support them, in both the TUI and GUI (egui) frontends.
- Removing the dead legacy `InputMode::ViewingFile`/`EditingFile` path (already unreachable before
  this change).
- Removing `ScreenArgs::ViewFile`/`EditFile` and `Effect::SaveActiveFile` from the plugin ABI
  (`savvagent-plugin`) and their wasm-adapter JSON bridge arms.
- Removing the `ratatui-code-editor` dependency (and `egui_code_editor` if confirmed orphaned) once
  no code references it.
- Updating README, `PRD.md`, and locale files to match.
- A `CHANGELOG.md` entry documenting the removal as a breaking (removed-feature) change.

**Out:**
- The `@`-inline-reference file picker (`App::open_file_picker`/`file_picker_select`, GUI
  `Ctrl+O`/`FilePicker`) — unrelated feature, stays untouched (premise correction 1).
- `/prompt-keybindings` — unrelated screen, stays untouched.
- Any other Screen plugin (`themes`, `connect`, `resume`, `model`, `language`, `plugins-manager`,
  `changelog`, `lsp-installer`, `trust`, …) — untouched.
- Adding a new/replacement way to view or edit files in the TUI (out of scope per the issue; the
  model already has `tool-fs`).

## Public-interface changes

**Breaking.** `ScreenArgs::ViewFile { path: String }`, `ScreenArgs::EditFile { path: String }`, and
`Effect::SaveActiveFile` are part of the plugin ABI (`savvagent-plugin`). The JSON projection in
`savvagent-plugin-wasm`'s `interactive.rs` adapter serializes a `ScreenArgs` **from host to a
WASM-implemented screen at screen-creation time** (`{"kind": "view-file", "path": ...}` /
`{"kind": "edit-file", ...}`) — it is not a guest-to-host request channel; a WASM plugin cannot ask
the runtime to open a screen by sending this JSON shape (guest-issued `OpenScreen` effects are
converted with `ScreenArgs::None` regardless, per `convert.rs`). Removing these three ABI items is a
breaking change under Non-Negotiable Rule 6 with two distinct real-world impacts:
- A **native Rust plugin** that references `ScreenArgs::ViewFile`/`EditFile`/`Effect::SaveActiveFile`
  in its own source fails to **compile** against the new `savvagent-plugin` version.
- An **external WASM plugin** that previously implemented a screen expecting to receive the
  `"view-file"`/`"edit-file"` JSON `kind` at creation time will simply never receive it again — those
  runtime screen ids no longer exist, so nothing ever asks the plugin to create a screen for them.
  This is a silent behavior change (a dead code path in the plugin, not a crash) rather than an error
  surfaced to the plugin.

This must be:
- named explicitly in this spec (done, here) and the plan,
- flagged explicitly to the architecture reviewer in Phase 4 step 8,
- called out in `CHANGELOG.md` under a `### Removed` heading with a one-line migration note ("no
  replacement — plugins should not ask the runtime to open a file-viewer/editor screen; if a plugin
  genuinely needs this, it should ship its own Screen implementation"),
- reflected in the version bump: per this repo's pre-1.0 convention (MINOR for
  features/breaking changes, PATCH for fixes) this is a MINOR bump.

The `/view`, `/edit`, and `/editor-keybindings` slash commands themselves are also a public
interface per Rule 6 ("the slash-command surface... documented in README.md") — their removal is the
explicit, deliberate point of issue #22 and is called out the same way.

Everything else in this change (deleting dead code, an unused dependency, orphaned translation
keys) has no public-interface surface of its own.

## Assumptions

- **`App::active_file_path` is removed along with `App::editor`** unless a grep at implementation
  time turns up a surviving reader outside the code being deleted (the spec's step 4 above already
  flags this as a "verify first" item — rationale: at spec-writing time every known reference to
  `active_file_path` is inside the code paths slated for removal, but the implementer must
  reconfirm against the actual working tree rather than trust this document, since specs can lag).
- **The `/view`↔`/edit` pairing in the existing `Prefill` test** (`app.rs:2666`) is retargeted to a
  different existing needs-arg command (`/bash`) rather than deleted outright — rationale: the test
  exercises `select_command`'s general prefill behavior, which is still valid coverage worth keeping
  once given a surviving example command.
- **`egui_code_editor` dependency removal is conditional**, not assumed — rationale: unlike
  `ratatui-code-editor` (confirmed TUI-only, used exclusively by the code being removed), the GUI
  crate may reference `egui_code_editor` transitively through other widgets; the implementer must
  grep before deleting the dependency line.
- **No new user-facing replacement is added** for viewing/editing files from the TUI/GUI — rationale:
  explicitly out of scope per the issue; the model already has `read_file`/`write_file` via
  `tool-fs`.
- **This ships as a MINOR version bump** (breaking removal, pre-1.0 convention) — rationale: Rule 6 +
  `CHANGELOG.md`'s stated convention.

## Goal & Success Criteria

Remove `/view`, `/edit`, and `/editor-keybindings` and every line of code, translation key, dependency,
and doc reference that exists solely to support them, in both the TUI and GUI frontends, leaving
zero dead code and zero orphaned plugin-ABI surface, while leaving the unrelated `@`-reference file
picker and `/prompt-keybindings` screen fully intact.

- [ ] `/view`, `/edit`, `/editor-keybindings` no longer appear in the command palette or
      `SlashRouter` dispatch in either frontend.
- [ ] `cargo build --workspace --all-targets` and `cargo clippy --workspace --all-targets` (with
      `RUSTFLAGS=-D warnings`, matching CI) are clean — no dead-code/unused-import/unused-dependency
      warnings from this removal.
- [ ] `cargo test --workspace` passes.
- [ ] `ratatui-code-editor` no longer appears in any `Cargo.toml`/`Cargo.lock` dependency graph
      reachable from `crates/savvagent`; `egui_code_editor` likewise if confirmed orphaned.
- [ ] README's slash-command table and GUI-status paragraph no longer mention `/view`, `/edit`, or
      `/editor-keybindings`.
- [ ] `CHANGELOG.md` documents the removal under `### Removed` with the plugin-ABI breaking-change
      note.

## Error Handling & Edge Cases

- A native plugin still referencing the removed `ScreenArgs::ViewFile`/`EditFile`/
  `Effect::SaveActiveFile` items fails to compile against the new `savvagent-plugin` — the intended,
  documented breaking-change behavior.
- An external WASM plugin that implements a screen keyed on the `"view-file"`/`"edit-file"` JSON
  `kind` simply stops being invoked (the runtime never creates those screens again) — a silent
  behavior change, not a crash; no compatibility shim is added (see "Public-interface changes").
- Nothing else reads `"view-file"`/`"edit-file"` as bare strings outside the code being deleted
  (confirmed via repo-wide grep during investigation) — no shim needed elsewhere.

## Risks & Open Questions

- **Test count/coverage drop.** Deleting `view_file`, `edit_file`, and `editor_keybindings` removes
  their unit tests too (e.g. `editor_keybindings`'s `populated_screen_includes_all_sections`). This
  is expected and correct — the tests exist only to cover code being deleted — but the plan should
  explicitly note the pre/post test count so the fast-path/final reviewers aren't surprised by a
  smaller `cargo test --workspace` output.
- **Exact locale key names** are confirmed against the actual TOML files during implementation
  (the approach section's step 9 list is the best-effort inventory from static analysis, not a
  guaranteed-exhaustive key list) — the implementer greps each locale file for `view-file`,
  `edit-file`, `editor-keybindings`, and the specific notes/picker key prefixes named above before
  deleting, in all four files, to avoid leaving orphaned keys or breaking a surviving reference.
