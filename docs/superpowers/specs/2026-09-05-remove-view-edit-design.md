# Remove `/view` and `/edit` — design

Date: 2026-09-05
Status: pending review
Related: savvagent/savvagent-cli#22

## Problem

`/view` and `/edit` (built-in slash commands backed by the `internal:view-file`
and `internal:edit-file` plugins) open an in-TUI code-editor surface —
`ratatui-code-editor` in the ratatui frontend, `egui_code_editor` in the egui
frontend — that doesn't work well in practice and pulls in a full editor
widget dependency that is inconsistent with `PRD.md`'s goal of a narrowly
focused, blazing-fast TUI. `/editor-keybindings` (`internal:editor-keybindings`)
exists solely to document the keybindings of the editor these two commands
open; removing the editor without removing this command would leave a slash
command that opens a help screen for a feature that no longer exists — dead
documentation, not dead code, but the same defect class the issue's "no dead
code remains" AC is guarding against. This spec therefore scopes the removal
to all three commands and their exclusively-supporting code.

## Scope

**In:**

- Remove the `/view`, `/edit`, and `/editor-keybindings` slash commands and
  their three backing built-in plugins (`internal:view-file`,
  `internal:edit-file`, `internal:editor-keybindings`).
- Remove the ratatui-side file-editor rendering/input path: `App::editor`
  (`ratatui_code_editor::editor::Editor`), `App::active_file_path`,
  `App::load_file_into_editor`, `App::clear_active_editor`, the legacy
  `App::open_file`/`App::save_file` popup-editor path, the legacy
  `InputMode::ViewingFile`/`InputMode::EditingFile` variants and their
  `main.rs`/`ui.rs` key-handling and rendering branches, and the
  `view-file`/`edit-file` marker-screen dispatch/rendering in `main.rs` and
  `ui.rs`.
- Remove the egui-side file-editor: `egui_app`'s `editor_buffer` field,
  `save_editor_buffer`, the marker-screen sync/paint branches in
  `egui_app/mod.rs` and `egui_app/view.rs`, and the
  `egui_app/widgets/editor.rs` + `egui_app/widgets/editor_theme.rs` widget
  files in full.
- Remove the `ratatui-code-editor` and `egui_code_editor` crate dependencies
  (root `Cargo.toml` + `crates/savvagent/Cargo.toml`, regenerate `Cargo.lock`)
  — confirmed via repo-wide grep that neither crate has any caller outside
  the file-editor code being removed here.
- Remove the plugin-ABI enum variants that exist only to carry these
  screens: `Effect::SaveActiveFile`, `ScreenArgs::ViewFile`,
  `ScreenArgs::EditFile` (`crates/savvagent-plugin/src/effect.rs`,
  `crates/savvagent-plugin/src/types.rs`), and their corresponding match
  arms in `crates/savvagent-plugin-wasm/src/adapter/interactive.rs`.
- Remove the `themes/editor_theme.rs` (ratatui) and
  `egui_app/widgets/editor_theme.rs` (egui) theme-builder files (each is
  used exclusively by the code editor being removed) and the
  `editor_theme_for_active`/`borrow_editor_theme`/`language_for_path`
  helpers in `app.rs` (used exclusively by `load_file_into_editor`/`open_file`).
- Remove the now-orphaned `Command` entries for `/view` and `/edit` in
  `App::refresh_commands` (`app.rs`) — a legacy, already-dead static command
  list (superseded by the plugin-manifest-driven command palette; see
  Premise corrections below) but still worth cleaning per the issue's
  "no dead code remains" AC. There is no matching `/editor-keybindings` entry
  in that list to remove (it was only ever plugin-registered).
- Remove all locale keys (`en`/`es`/`hi`/`pt.toml`) for
  `picker.view-file.*`, `picker.edit-file.*`, `picker.editor-keybindings.*`,
  `slash.view-summary`, `slash.edit-summary`,
  `slash.editor-keybindings-summary`, `plugin.view-file-description`,
  `plugin.edit-file-description`, `plugin.editor-keybindings-description`,
  and any `notes.file-*` keys that become unused once `load_file_into_editor`/
  `open_file`/`save_file` are removed (verified per-key against remaining
  callers before deletion — see Task 1 step in the plan).
- Update `README.md`'s slash-command table and any prose describing `/view`,
  `/edit`, or `/editor-keybindings`.
- Update the central built-in-plugin-id assertion test in
  `crates/savvagent/src/plugin/mod.rs` (`register_builtins_pr8_complete`):
  remove the three plugin ids from the expected list and drop the expected
  count from 30 to 27.
- Delete all now-fully-dead source files (see File Map) and their inline
  `#[cfg(test)]` modules along with them.

**Out:**

- The `@`-path file picker (`App::open_file_picker`/`file_picker_select`/
  `close_file_picker`, `App::file_explorer`, `App::is_file_picker_active`,
  egui's `widgets::file_picker::FilePicker`) is **not** touched — confirmed
  by grep that it is triggered by `@` and `Ctrl+O` for path-insertion into
  the prompt, independent of `/view`/`/edit`, and used by other commands.
- `prompt_keybindings` (the `/prompt-keybindings` help screen) is unrelated
  and unaffected — it documents the main prompt textarea, not the file
  editor. The Shift+drag mouse-capture note added there in #10/PR #25 stays.
- No change to the SPP wire format, a tool's MCP schema, or the on-disk
  transcript/keyring format.

## Public-interface changes (Non-Negotiable Rule 6)

**This is a breaking change** to two public surfaces:

1. **The slash-command surface.** `/view`, `/edit`, and `/editor-keybindings`
   are removed. Any user or script relying on these commands breaks. This is
   the change the issue explicitly asks for.
2. **The plugin ABI** (`savvagent-plugin` crate). `Effect::SaveActiveFile`,
   `ScreenArgs::ViewFile`, and `ScreenArgs::EditFile` are removed. Both
   `Effect` and `ScreenArgs` are `#[non_exhaustive]`, so no external plugin
   can have exhaustively matched them without a catch-all arm — but a
   variant removal still breaks source compatibility for any external plugin
   that *constructs* one of these variants (unlikely in practice, since they
   exist to open savvagent's own built-in screens, but the surface is public
   and the removal must be named regardless).

Both changes must be:
- flagged explicitly to the architecture reviewer — the plan's Phase 4 step 8
  dispatch instructions (once the plan is written) must call this out by
  name; this is a requirement on the forthcoming plan, not yet satisfied by
  this spec alone,
- called out in `CHANGELOG.md` under a `### Removed` heading,
- reflected in a **MINOR** version bump per this repo's pre-1.0 SemVer
  convention (breaking changes get MINOR, not PATCH) at release-cut time.

## Premise corrections

The issue's proposal list cites `App::open_file_picker` as code to remove.
Investigation (repo-wide grep across `app.rs`/`main.rs`/`egui_app/`) shows
`open_file_picker`/`file_picker_select`/`close_file_picker`/`file_explorer`
are **not** view/edit-specific — they back the shared `@`-path-insertion
picker triggered by `@` and `Ctrl+O` in both frontends, used by any command
that accepts a file path argument. This spec does **not** remove them (see
Scope → Out). This corrects the issue's proposal to match the code as it
exists today (the issue may have been written against an older snapshot,
or the picker's scope grew since).

The issue also describes `/view`/`/edit` as being implemented via
`InputMode::ViewingFile`/`InputMode::EditingFile` in `main.rs`. That input-mode
machinery is real but is already commented as **legacy and dead**
(`#[allow(dead_code)]` on both variants) — the live implementation is the
`internal:view-file`/`internal:edit-file` **Screen plugins**
(`crates/savvagent/src/plugin/builtin/view_file/`,
`crates/savvagent/src/plugin/builtin/edit_file/`), registered through the
plugin manifest system and dispatched via the screen stack. Both the live
plugin path and the legacy dead-code path share the same `App::editor`
backing store and must be removed together — removing only one would leave
the other as orphaned dead code referencing a field that no longer has a
live writer.

Similarly, `App::refresh_commands`'s static `Command` list (which the issue
names directly) is confirmed dead in production: the real command palette
(`internal:command-palette`) sources its list from the plugin manifest
registry via `build_palette_commands` in `crates/savvagent/src/plugin/effects.rs`,
not from `App.commands`. `App::refresh_commands`/`App.commands` and its
`filtered_command_indices`/`select_command`/`palette_push_char`/
`palette_pop_char`/`close_command_palette` methods are only exercised by
`app.rs`'s own unit tests today. This spec removes the `/view`/`/edit`
entries from that dead list (per the issue and the "no dead code" AC) but
does **not** attempt a broader cleanup of the rest of that legacy apparatus
— that is out of scope for this issue and would be its own follow-up.

## Architecture

No architectural surface is added; this is a subtractive change across the
existing crate boundaries:

- `crates/savvagent` (TUI + egui shell): both frontends' rendering/input
  paths for the file editor are removed, along with the built-in plugins
  that own the `/view`/`/edit`/`/editor-keybindings` slash commands and
  screens.
- `crates/savvagent-plugin` (plugin ABI): three `#[non_exhaustive]` enum
  variants are removed (see Public-interface changes).
- `crates/savvagent-plugin-wasm` (WASM adapter): the two match arms that
  serialize `ScreenArgs::ViewFile`/`EditFile` to JSON for the WASM/WIT
  boundary are removed alongside the enum variants — a variant with no
  producer needs no adapter arm.

No change to `savvagent-host`'s turn loop, the host-swap `RwLock` discipline,
the provider transport split, or `ToolRegistry` tool dispatch — none of this
touches those surfaces.

## File Map

**Delete entirely** (fully dead once the plugins are deregistered):

- `crates/savvagent/src/plugin/builtin/view_file/mod.rs`
- `crates/savvagent/src/plugin/builtin/view_file/screen.rs`
- `crates/savvagent/src/plugin/builtin/edit_file/mod.rs`
- `crates/savvagent/src/plugin/builtin/edit_file/screen.rs`
- `crates/savvagent/src/plugin/builtin/editor_keybindings/mod.rs`
- `crates/savvagent/src/plugin/builtin/themes/editor_theme.rs`
- `crates/savvagent/src/egui_app/widgets/editor.rs`
- `crates/savvagent/src/egui_app/widgets/editor_theme.rs`

**Edit** (remove specific sections, file remains for other purposes):

- `crates/savvagent/src/app.rs` — `InputMode::ViewingFile`/`EditingFile`
  variants (~lines 270-293); `/view`/`/edit` `Command` entries in
  `refresh_commands` (~1452-1458); `editor_theme_for_active`/
  `borrow_editor_theme`/`language_for_path` helpers (~211-249);
  `load_file_into_editor`/`clear_active_editor`/legacy `open_file`/
  `save_file` (~1628-1715); the `editor`/`active_file_path` fields
  (~607-608) and their initializers; the `ratatui_code_editor::editor::Editor`
  import (~191); the stale `/view` prefill test (~2666) and
  `input_mode_label()` strings for the removed variants (~2812).
- `crates/savvagent/src/main.rs` — the `view-file`/`edit-file` marker-screen
  pre-dispatch routing (~3224-3252); the legacy `InputMode::ViewingFile`/
  `EditingFile` key-handling match arms (~3668-3704). Do **not** touch the
  `@`/`Ctrl+O` file-picker wiring (~3500-3504) — out of scope.
- `crates/savvagent/src/ui.rs` — the `view-file`/`edit-file` screen-stack
  render branch and legacy `InputMode` popup-editor render branch
  (~332-395); `paint_file_screen` (~1165-1195+ through its end).
- `crates/savvagent/src/plugin/effects.rs` — the `Effect::CloseScreen`
  view-file/edit-file teardown block (~55-65); the `Effect::SaveActiveFile`
  arm's body (becomes unreachable once the variant is removed — remove the
  match arm itself); the `open_screen` preflight's view-file/edit-file file
  load (~614-621).
- `crates/savvagent/src/plugin/mod.rs` — the `EditFilePlugin`/
  `EditorKeybindingsPlugin`/`ViewFilePlugin` registration calls (~110-111,
  136); the "PR 4 adds: view-file, edit-file" comment (~72-73); the
  `register_builtins_pr8_complete` test's expected-id list (remove
  `internal:edit-file`, `internal:editor-keybindings`, `internal:view-file`)
  and expected count (30 → 27).
- `crates/savvagent/src/plugin/builtin/mod.rs` — the `pub mod edit_file;`,
  `pub mod editor_keybindings;`, `pub mod view_file;` declarations and their
  doc-comment mentions.
- `crates/savvagent/src/plugin/builtin/themes/mod.rs` — the
  `pub mod editor_theme;` declaration.
- `crates/savvagent/src/egui_app/mod.rs` — the `editor_buffer` field
  (~126-131) and its initializer; `save_editor_buffer` (~347-364); the
  marker-screen sync/`Ctrl-S`-interception block (~531-590).
- `crates/savvagent/src/egui_app/view.rs` — the `view-file`/`edit-file`
  marker-screen paint branch (~114-123).
- `crates/savvagent/src/egui_app/widgets/mod.rs` — the `pub mod editor;`,
  `pub mod editor_theme;` declarations and the doc-comment mention of the
  file editor (keep the `Ctrl+O` file-picker mention — that widget stays).
- `crates/savvagent-plugin/src/effect.rs` — the `SaveActiveFile` variant
  and its `Display` arm.
- `crates/savvagent-plugin/src/types.rs` — the `ViewFile`/`EditFile`
  variants, their `screen_id()` arms, and their unit tests (~518, ~627-631).
- `crates/savvagent-plugin-wasm/src/adapter/interactive.rs` — the
  `ScreenArgs::ViewFile`/`EditFile` JSON-serialization match arms (~747-748)
  and their unit-test usages (~902-903).
- `crates/savvagent/Cargo.toml` — `egui_code_editor.workspace = true`
  (~85), `ratatui-code-editor.workspace = true` (~97).
- `Cargo.toml` (root) — the `egui_code_editor` and `ratatui-code-editor`
  workspace-dependency entries; run `cargo check --workspace` afterward to
  regenerate `Cargo.lock` (do not hand-edit the lockfile).
- `crates/savvagent/locales/{en,es,hi,pt}.toml` — remove
  `picker.view-file.*`, `picker.edit-file.*`, `picker.editor-keybindings.*`,
  `slash.view-summary`, `slash.edit-summary`,
  `slash.editor-keybindings-summary`, `plugin.view-file-description`,
  `plugin.edit-file-description`, `plugin.editor-keybindings-description`;
  remove `notes.file-not-found`/`notes.file-editor-error`/
  `notes.file-read-error`/`notes.file-saved`/`notes.file-write-error` only
  if grep confirms zero remaining callers after the `app.rs` edits above (a
  final verification step in the plan, since some of these note keys are
  generic enough another caller might reuse them — check before deleting).
- `README.md` — remove `/view`/`/edit`/`/editor-keybindings` from the
  slash-command table and any prose describing the file editor/viewer.
- `PRD.md` — the M6 milestone's "TUI editor widget decision" bullet and the
  matching "TUI editor widget" entry under the risks/open-questions section
  both currently describe `ratatui-code-editor` as "pending a future
  consolidation pass" — that question is now resolved (removed rather than
  consolidated); update both to reflect the resolution. The M5/M6 milestone
  bullets that narrate `/view`/`/edit` shipping as part of that historical
  milestone (e.g. "`/view` and `/edit` open files in the in-TUI
  viewer/editor") are a historical record of what shipped at the time and
  are **not** rewritten, matching this repo's convention of not editing
  past `CHANGELOG.md` entries — only the still-open decision entries change.
- `crates/savvagent/src/plugin/builtin/prompt_keybindings/mod.rs` — the
  module doc comment's "Mirrors `internal:editor-keybindings` for symmetry"
  line becomes stale once that plugin is deleted; reword to drop the
  now-nonexistent comparison.
- `crates/savvagent/src/plugin/builtin/keybindings_view.rs` — the module
  doc comment's "Used by `internal:prompt-keybindings` and
  `internal:editor-keybindings`" line; drop the `editor-keybindings` mention
  (the shared viewer is still used by `prompt-keybindings` and any future
  help screen, so the file itself is not touched beyond this comment).

## Assumptions

- **Removing `/editor-keybindings` alongside `/view`/`/edit`**, even though
  the issue's proposal list doesn't name it explicitly — it exists solely to
  document the editor being removed; leaving it would surface a help screen
  for a nonexistent feature, which the issue's own "no dead code remains" AC
  is written to prevent. (High confidence — the file's own module doc says
  it documents "the keybindings active inside the `view-file` / `edit-file`
  screens.")
- **Not touching `App::open_file_picker`/`file_picker_select`/
  `file_explorer`** despite the issue naming `open_file_picker` — confirmed
  via grep this is shared `@`/`Ctrl+O` path-insertion infrastructure, not
  view/edit-specific. Removing it would break unrelated commands. (High
  confidence, directly falsifiable via grep, verified above.)
- **Not attempting a broader cleanup of `App::refresh_commands`'s dead
  static command list** beyond removing the `/view`/`/edit` entries named in
  the issue — the rest of that legacy apparatus (`filtered_command_indices`,
  `select_command`, etc.) is a pre-existing, unrelated dead-code smell not
  in this issue's scope; flagging it as a candidate follow-up rather than
  scope-creeping this change. (Medium confidence — reasonable interpretation
  of "no dead code remains" as scoped to code this change's removal makes
  newly, fully dead, not a general dead-code sweep of the whole file.)
- **Deleting `Effect::SaveActiveFile`/`ScreenArgs::ViewFile`/`EditFile`**
  rather than leaving them as unused-but-present enum variants — since
  they exist to carry exactly the two screens being removed and have no
  other producer or consumer after this change, keeping them would itself
  be dead code in the plugin ABI. (High confidence.)
- **`notes.file-*` locale keys**: assumed removable pending a final grep
  check in the plan (Task 1) rather than removed unconditionally here, since
  they're generic-sounding strings that a future feature might plausibly
  reuse — the plan verifies zero remaining callers before deleting, not
  before this spec is written.

## Goal & Success Criteria

Remove `/view`, `/edit`, and `/editor-keybindings` and all code that exists
solely to support them, across both the ratatui and egui frontends and the
plugin ABI, leaving zero dead code and zero orphaned documentation.

- [ ] `/view`, `/edit`, `/editor-keybindings` no longer appear in either
      frontend's command palette (verified by running `cargo run -p savvagent`
      and by the updated `register_builtins_pr8_complete` test).
- [ ] `cargo build --workspace --all-targets` and
      `cargo clippy --workspace --all-targets` are clean — no unused-import,
      unused-field, or dead-code warnings from this removal.
- [ ] `cargo test --workspace` passes, including the updated
      `register_builtins_pr8_complete` plugin-id-count assertion and the
      updated locale key-parity test (`tests/locales.rs`).
- [ ] `egui_code_editor` and `ratatui-code-editor` no longer appear in
      `cargo tree --workspace` output.
- [ ] `README.md`'s slash-command table no longer lists `/view`, `/edit`,
      or `/editor-keybindings`.

## Error Handling & Edge Cases

- **A transcript replay or user script that still invokes `/view`/`/edit`**
  after this ships: the command dispatcher's existing "unknown command"
  path handles this the same way any other removed/typo'd slash command is
  handled today — no new error-handling code is needed.
- **A third-party WASM plugin compiled against the older `savvagent-plugin`
  ABI** that constructs `ScreenArgs::ViewFile`/`EditFile` or
  `Effect::SaveActiveFile`: it will fail to compile against the new crate
  version (source-incompatible), which is the expected, documented
  consequence of a breaking ABI change — not a runtime failure mode this
  change needs to guard against.
- **Locale key removal must stay in lockstep across all four locale files**
  — `tests/locales.rs`'s key-parity check enforces this; removing a key
  from `en.toml` but not `es`/`hi`/`pt.toml` fails that test immediately,
  which is the intended safety net.

## Risks & Open Questions

- The `notes.file-*` locale keys' final disposition depends on a grep check
  performed during implementation (Task 1), not resolved in this spec —
  flagged above under Assumptions rather than blocking spec approval.
- `Cargo.lock` regeneration via `cargo check --workspace` may produce
  unrelated churn if other dependencies also moved since the lockfile was
  last regenerated; the plan's Task 1 verification step diffs the lockfile
  change to confirm it's limited to the two removed crates (and their
  now-unreferenced transitive dependencies) before committing.
