# formalize-claude-code-skills Implementation Plan

**Goal:** Document, in `CLAUDE.md`, the repo-endorsed pattern for using Claude Code skills alongside
this repo's own Copilot CLI skills (`.github/skills/`), resolving issue #23: whether `.claude/skills/`
is committed or personal/machine-local, which skills (if any) get vendored, and how a tracker-related
skill's conventions must defer to this repo's GitHub Issues convention (never JIRA).

**Architecture:** No code changes. `.claude/skills/rust-engineer/SKILL.md` is already a tracked,
committed file in this repo (confirmed via `git ls-files .claude/skills/`) — a repo-authored,
Claude-Code-compatible skill with no counterpart under `.github/skills/` (which today holds only
`savvagent-development`) — that precedent, not the issue's speculative "gitignore recommended"
framing, is the one CLAUDE.md must document, since the code (an existing committed file) outranks a
proposal when the two disagree. The convention this plan documents: `.claude/skills/<name>/SKILL.md`
is for repo-authored, project-specific skills (independently authored for Claude Code, or
complementing a `.github/skills/` skill where one exists for the same topic) and IS committed;
generic/personal Claude Code skills a
contributor pulls in from `~/.claude/skills/` (e.g. a symlinked `creating-tickets`) are NOT to be
added under this repo's `.claude/skills/` at all — they stay in the user's home directory, outside
repo tracking, so no `.gitignore` change is needed. Any tracker-related skill a contributor uses
locally must still defer to this repo's GitHub Issues convention (`.github/skills/savvagent-development`
and `gh issue`/`gh pr`), never a JIRA-first abstraction, when working in this repo.

**Tech Stack:** Markdown documentation only (`CLAUDE.md`).

**Spec:** none — fast-path per `savvagent-development`'s trivial-task criteria (single file, no new
interface, no behavior change; see PR body).

**Release line:** v0.19.4 (patch — docs-only, no new interface).

**Branch:** `docs/claude-skills`

**File Map:**

- Modified: `CLAUDE.md` — new "Claude Code skills" section documenting the `.claude/skills/`
  convention and precedence vs. `.github/skills/`.

## Task 1: Document the `.claude/skills/` convention in `CLAUDE.md`

**Files:**

- Modify: `CLAUDE.md`

- [x] Confirm current state (failing-first sanity check): `grep -n "claude/skills" CLAUDE.md` returns
      nothing today — the gap issue #23 describes.
- [x] Add a new `## Claude Code skills` section to `CLAUDE.md` (after "Persistence", the current last
      section) covering:
      - `.claude/skills/<name>/SKILL.md` is for repo-authored, project-specific skills (e.g.
        `rust-engineer`, `tui-engineer`) — committed, same status as `.github/skills/`.
      - Generic/personal Claude Code skills pulled from `~/.claude/skills/` (e.g. `creating-tickets`)
        are NOT added to this repo's `.claude/skills/` — they stay personal/machine-local in the
        user's home directory; nothing to gitignore since they're never placed under the repo tree.
      - Any tracker-related skill used while working in this repo must defer to this repo's GitHub
        Issues convention (`gh issue`, `.github/skills/savvagent-development`) — never JIRA or another
        tracker abstraction.
      - Precedence/conflict handling: if a Claude Code skill and a `.github/skills/` skill overlap in
        purpose, the `.github/skills/` skill (this repo's own, e.g. `savvagent-development`'s
        ticket-creation conventions) governs for this repo; the Claude Code skill's generic guidance
        yields.
- [x] Re-run `grep -n "claude/skills" CLAUDE.md` to confirm the new section is present and readable.
- [x] Public-interface check: none — docs only, no SPP/tool/plugin/slash-command/on-disk-format
      change.
- [x] Host-swap / `RwLock` check: not applicable — no code touched.
- [x] `ProgressDispatcher` check: not applicable — no streaming provider path touched.
- [x] Format and commit: `git commit -m "docs: document .claude/skills convention"`.

## Task 2 (release, not part of this PR): cut v0.19.4

Per `RELEASING.md` and this skill's Non-Negotiable Rule 8, after this task's PR merges to `main`, open
a separate `release/0-19-4` PR: bump `workspace.package.version` (and matching `workspace.dependencies`
versions) to `0.19.4`, move the relevant `## [Unreleased]` content under a new `## 0.19.4 - <date>`
heading in `CHANGELOG.md`, then tag and push `v0.19.4` once that PR merges.
