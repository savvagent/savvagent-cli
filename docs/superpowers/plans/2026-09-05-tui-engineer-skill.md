# tui-engineer-skill Implementation Plan

**Goal:** Add a `tui-engineer` Claude Code skill (`SKILL.md`, Copilot/Claude-compatible frontmatter
format matching `rust-engineer`) covering best practices for building and reviewing terminal UIs in
this repo's ratatui/crossterm stack, resolving issue #24.

**Architecture:** No source-code changes. Per the convention documented in `CLAUDE.md`'s "Claude Code
skills" section (added in #23/#28): repo-authored, project-specific Claude Code skills are committed
under `.claude/skills/<name>/SKILL.md`, matching the existing `.claude/skills/rust-engineer/SKILL.md`
precedent (same frontmatter shape: `name`, `description`, `tools`, `model`). This plan adds
`.claude/skills/tui-engineer/SKILL.md` in that same format, covering: core principles (immediate-mode
redraw discipline, state/render separation, never blocking the event loop, panic-safe terminal
restoration), layout & responsiveness (`Constraint`-based splits, testing at small terminal sizes,
scroll-offset clamping), widgets & styling (centralized theming, avoiding color-only state signaling,
`NO_COLOR`/low-color degradation), async integration (`crossterm::EventStream` + `tokio::select!`,
dirty-flag redraw batching, and this repo's `Arc<RwLock<Option<Arc<Host>>>>` host-swap rule),
accessibility (keyboard-first design, visible focus indicators, discoverable help), testing
(`TestBackend` snapshot testing, pure state-transition unit tests matching `ui::tests::*` /
`canvas_input::tests::*` conventions), and project-specific notes (the `Effect`-based canvas dispatch
pattern, and the cross-platform lesson from #18 — never hardcode `/bin/true`, resolve via `PATH`).

**Tech Stack:** Markdown documentation only (`SKILL.md` frontmatter + prose, matching
`.claude/skills/rust-engineer/SKILL.md`'s shape).

**Spec:** none — fast-path per `savvagent-development`'s trivial-task criteria (single file, no new
interface, no behavior change; see PR body).

**Release line:** v0.20.2 (patch — docs-only, no new interface).

**Branch:** `docs/tui-engineer-skill`

**File Map:**

- New: `.claude/skills/tui-engineer/SKILL.md` — the new skill document.

## Task 1: Add `.claude/skills/tui-engineer/SKILL.md`

**Files:**

- Create: `.claude/skills/tui-engineer/SKILL.md`

- [x] Confirm current state (failing-first sanity check): `git ls-files .claude/skills/` on this
      branch shows only `rust-engineer/SKILL.md` before this task — the gap issue #24 describes.
- [x] Add frontmatter matching `rust-engineer`'s shape: `name: tui-engineer`, a `description` field
      stating when to use it (designing/building/reviewing ratatui/crossterm TUIs; applies whenever
      touching `crates/savvagent` — `app.rs`, `tui.rs`, `ui.rs`, `canvas_input.rs`, plugin
      screens/widgets), `tools: Read, Write, Edit, Bash, Glob, Grep`, `model: sonnet`.
- [x] Write the body covering (as prose sections, not a rigid checklist): core principles, layout &
      responsiveness, widgets & styling, async integration (including this repo's host-swap
      `RwLock` rule and the `crossterm::EventStream` + `tokio::select!` pattern), accessibility,
      testing conventions matching this repo's existing `ui::tests::*`/`canvas_input::tests::*`
      patterns, a performance checklist, and a "Project-specific notes (savvagent-cli)" section
      citing the `Host` `Arc<RwLock<Option<Arc<Host>>>>` pattern, the `Effect`-based canvas dispatch
      pattern (`crates/savvagent/src/canvas_input.rs`), and the `/bin/true`-hardcoding lesson from
      #18 (resolve system commands via `PATH`, never a hardcoded platform-specific absolute path).
- [x] Verify accuracy against the current codebase: confirm the `Host` swap pattern
      (`crates/savvagent/src/app.rs`, `tui.rs`), the `Effect`/`apply_canvas_effects` pattern
      (`crates/savvagent/src/canvas_input.rs`), and the `open_url_system_browser` `PATH`-resolution
      tests referenced in the skill still match the code as described.
- [x] Public-interface check: none — docs only, no SPP/tool/plugin/slash-command/on-disk-format
      change.
- [x] Host-swap / `RwLock` check: not applicable — no code touched (the skill *documents* the rule,
      it doesn't change it).
- [x] `ProgressDispatcher` check: not applicable — no streaming provider path touched.
- [x] Format and commit: `git commit -m "docs: add tui-engineer skill"`.

## Task 2 (release, not part of this PR): cut v0.20.2

Per `RELEASING.md` and this skill's Non-Negotiable Rule 8, after this task's PR merges to `main`, open
a separate `release/0-20-2` PR: bump `workspace.package.version` (and matching `workspace.dependencies`
versions) to `0.20.2`, move the relevant `## [Unreleased]` content under a new `## 0.20.2 - <date>`
heading in `CHANGELOG.md`, then tag and push `v0.20.2` once that PR merges.
