# Mouse-capture Shift+drag help Implementation Plan

**Goal:** Surface the "hold Shift while dragging/clicking to bypass mouse capture and use native
terminal text selection/copy" workaround inside the app itself, not just in the README, by adding a
row for it to both in-app keybindings help screens (`/prompt-keybindings` and
`/editor-keybindings`).

**Architecture:** `crates/savvagent/src/tui.rs` unconditionally enables `EnableMouseCapture` at
startup, which suppresses the terminal's native click-drag text selection everywhere in the app
(main conversation log and the file viewer/editor) unless the user holds Shift. This is documented
today only in `README.md`'s "Scrolling the conversation log" section. The two in-app keybindings
screens (`crates/savvagent/src/plugin/builtin/prompt_keybindings/mod.rs` and
`crates/savvagent/src/plugin/builtin/editor_keybindings/mod.rs`) mirror each other's static-section
shape (per their own module docs) and are the discoverable, in-app surface for this kind of caveat —
`editor_keybindings` already has this exact pattern for a different caveat (`notes_rows()` /
`note-ctrl-c-quits`, "Ctrl+C quits savvagent globally ... fires before the editor sees it"). This
plan adds a matching row to each screen: a new "Mouse" section on `prompt-keybindings` (which
currently has none) and a new row in `editor-keybindings`'s existing `notes_rows()`. Both new rows
are added via the same `row(chord, description_key)` helper each file already uses, with new
`picker.<screen>.row.*` keys added to all four locale files (`en.toml`, `es.toml`, `hi.toml`,
`pt.toml` — all four already carry the full key set for both screens, so all four need the new
keys to stay consistent).

**Tech Stack:** Rust 2024, the existing `savvagent-plugin` `Screen` trait +
`ScrollableKeybindingsScreen`/`KeybindingSection`/`KeybindingRow` types in
`crates/savvagent/src/plugin/builtin/keybindings_view.rs`, `rust_i18n::t!` + the four TOML locale
files under `crates/savvagent/locales/`.

**Spec:** none — fast-path per `savvagent-development`'s trivial-task criteria (see PR body).

**Release line:** v0.19.3 (patch — pure bug/UX fix, no new interface).

**Branch:** `tui/mouse-capture-help`

**File Map:**

- Modified: `crates/savvagent/src/plugin/builtin/prompt_keybindings/mod.rs` — add a "Mouse" section
  with a Shift+drag/click row.
- Modified: `crates/savvagent/src/plugin/builtin/editor_keybindings/mod.rs` — add a Shift+drag/click
  row to the existing `notes_rows()`.
- Modified: `crates/savvagent/locales/en.toml`, `es.toml`, `hi.toml`, `pt.toml` — new
  `picker.prompt-keybindings.row.mouse-capture` and
  `picker.editor-keybindings.row.note-shift-drag-copy` keys (English row text authoritative; other
  locales get an English placeholder with a `# TODO: translate` comment, matching this repo's
  existing practice for locale gaps — see Task 1 note).

## Task 1: Add the Shift+drag/click caveat to both keybindings screens

**Files:**

- Modify: `crates/savvagent/src/plugin/builtin/prompt_keybindings/mod.rs`
- Modify: `crates/savvagent/src/plugin/builtin/editor_keybindings/mod.rs`
- Modify: `crates/savvagent/locales/en.toml`
- Modify: `crates/savvagent/locales/es.toml`
- Modify: `crates/savvagent/locales/hi.toml`
- Modify: `crates/savvagent/locales/pt.toml`

- [x] Confirm current behavior with a failing-first check: run
      `cargo test -p savvagent plugin::builtin::prompt_keybindings -- --nocapture` and
      `cargo test -p savvagent plugin::builtin::editor_keybindings -- --nocapture` to see the
      existing baseline test output (both currently pass; there is no existing test asserting the
      new row's absence — this step is a sanity baseline, not a red test, since the addition is
      additive content and the fast-path assertion below is what actually verifies the change).
- [x] In `crates/savvagent/locales/en.toml`, under `[picker.prompt-keybindings.row]`, add:
      `mouse-capture = "Hold Shift while dragging/clicking to bypass mouse capture and use native terminal text selection + copy"`.
      Under `[picker.editor-keybindings.row]`, add:
      `note-shift-drag-copy = "Hold Shift while dragging/clicking to bypass mouse capture for native terminal text selection + copy (same as the main conversation log)"`.
- [x] Mirror both keys (verbatim English text, since these are UX-critical caveats and this repo has
      no automated translation pipeline) into `es.toml`, `hi.toml`, and `pt.toml` in the same
      `[picker.prompt-keybindings.row]` / `[picker.editor-keybindings.row]` sections, preserving each
      file's existing key ordering/alignment style.
- [x] In `crates/savvagent/src/plugin/builtin/prompt_keybindings/mod.rs`: add
      `section("picker.prompt-keybindings.section-mouse", mouse_rows())` to the `sections` vec in
      `build_prompt_keybindings_screen` (after `section-history`, before the dynamic plugin-rows
      push), add a new `section-mouse = "Mouse"` key to all four locale files'
      `[picker.prompt-keybindings]` tables, and add a `mouse_rows()` function returning
      `vec![row("Shift+drag / Shift+click", "picker.prompt-keybindings.row.mouse-capture")]`.
- [x] In `crates/savvagent/src/plugin/builtin/editor_keybindings/mod.rs`: add
      `row("Shift+drag / Shift+click", "picker.editor-keybindings.row.note-shift-drag-copy")` to the
      `notes_rows()` vec (after the existing `note-ctrl-c-quits` row).
- [x] Update the existing test assertions if line-count thresholds are affected:
      `populated_screen_includes_static_sections` (prompt_keybindings, currently asserts
      `line_count() > 20`) and `populated_screen_includes_all_sections` (editor_keybindings,
      currently asserts `line_count() > 30`) — both thresholds already have headroom, so no numeric
      change should be required, but re-run them to confirm.
- [x] Run `cargo test -p savvagent plugin::builtin::prompt_keybindings` and
      `cargo test -p savvagent plugin::builtin::editor_keybindings` — expect green, and manually
      confirm (via the existing `render` test pattern already used in
      `dynamic_plugin_rows_become_a_section`) that the new row text renders.
- [x] Run `cargo build --workspace --all-targets`, `cargo clippy --workspace --all-targets`, and
      `cargo fmt --all --check` — expect clean.
- [x] Public-interface check: this is additive-only (new translation keys, new static screen rows);
      no SPP wire format, tool schema, plugin ABI, slash command, or on-disk format changes. No
      CHANGELOG entry beyond the standard "Fixed" line for this issue.
- [x] Host-swap / `RwLock` check: not applicable — no changes to `app.rs`/`tui.rs`.
- [x] `ProgressDispatcher` check: not applicable — no streaming provider path touched.
- [x] Format and commit: `cargo fmt --all` then
      `git commit -m "savvagent: surface Shift+drag mouse-capture bypass in keybindings help"`.

## Task 2 (release, not part of this PR): cut v0.19.3

Per `RELEASING.md` and this skill's Non-Negotiable Rule 8, after this task's PR merges to `main`,
open a separate `release/0-19-3` PR: bump `workspace.package.version` (and matching
`workspace.dependencies` versions) to `0.19.3`, move `## [Unreleased]` content (this fix) under a new
`## 0.19.3 - <date>` heading in `CHANGELOG.md`, then tag and push `v0.19.3` once that PR merges.
