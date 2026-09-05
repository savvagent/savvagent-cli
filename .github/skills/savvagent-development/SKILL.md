---
name: savvagent-development
description: Use when developing any feature or fix in the savvagent-cli repository (the Rust-only, MCP-first terminal coding agent) end-to-end — from a GitHub issue or plain task brief, through ship and verify, fully autonomously with no mid-run questions. Bundles the plan-by-plan discipline (committed design specs in docs/superpowers/specs/ and implementation plans in docs/superpowers/plans/), the Rust workspace conventions (everything-is-MCP-shaped, the host/tool/provider crate boundaries, the host-swap RwLock rule, the provider transport split), autonomous spec generation, plan generation, task-by-task implementation, PR review loops, verification, release cutting, and close-out. For other repositories use general-development.
---

# Savvagent-CLI Development — Autonomous End-to-End

Autonomous, plan-driven feature/fix workflow for the **savvagent-cli** repository (a Rust-only,
MCP-first terminal coding agent — see `PRD.md`). Walks from intake (a GitHub issue OR a plain task
brief) through spec → plan → implement → PR → review → merge → **release** → verify → close, with
no mid-run human questions. Returns control only when the work is shipped, released, and verified,
or on a true blocker.

This is the **savvagent-cli-specific sibling** of `general-development`. Same spine; the
repo-agnostic convention-discovery phase is replaced by the hardcoded conventions below (Rust
workspace, plan-by-plan docs under `docs/superpowers/`, the everything-is-MCP-shaped architecture).
If you are working in any other repository, use `general-development`.

## Why this shape

The spine mirrors what this repository builds: savvagent-cli treats **everything as MCP-shaped** —
a provider is just a `ProviderHandler`, a tool is just a stdio MCP server, wired together by a thin
`Host` turn loop. The workflow below applies that same separation to the repo itself: a design spec
and a plan document first (the specification), implementation against them (the mechanical
execution), and the repo's own test suite plus CI as the verifier.

[`CLAUDE.md`](../../../CLAUDE.md) is the load-bearing conventions document and outranks anything
here that has drifted from it. [`PRD.md`](../../../PRD.md) is the product vision and scope of
record; [`crates/savvagent-protocol/SPEC.md`](../../../crates/savvagent-protocol/SPEC.md) is the SPP
wire-format spec of record; [`README.md`](../../../README.md) documents slash commands, env vars,
and on-disk paths for developers. There is no archive directory for design docs — a shipped spec
under `docs/superpowers/specs/` keeps its place with its `> **Status:**` flipped to IMPLEMENTED, and
the matching plan's checkboxes are ticked in place. Docs can lag the code. Read the code.

## The Iron Law

**The source of truth is the task brief OR the GitHub issue — whichever started the work.** Not
Slack. Not the PR title. Not a teammate's summary. If the work began from a plain instruction, that
instruction (captured verbatim at intake) is the contract. If an issue exists, read the issue.

**Fully autonomous means no mid-run questions.** Make the most reasonable interpretation, document
the assumption, continue. Escalate only on true blockers. "Should I continue?" is never a stop
condition.

**Green tests are not the same as work-done.** `cargo test --workspace` does not validate that the
TUI actually spawns `savvagent-tool-fs` at runtime (a `cargo build` of the whole workspace is a
prerequisite even for TUI-only work), that a plugin example (`examples/plugin-hello-*`) still loads,
that the cross-platform release binaries built by `cargo-dist` still work, or that a released crate
version is installable. Whatever this change touches, verify it explicitly (Phase 5).

**A merge to `main` is not done until a release is cut.** Every PR that lands on `main` triggers a
version bump, tag, and published GitHub Release with build artifacts (Phase 4 step 12). There is no
"batch several PRs into one release later" carve-out in this workflow — see Non-Negotiable Rule 8.

**Violating the letter of the workflow is violating the spirit.**

## Non-Negotiable Rules

These hold for every run of this skill, no exceptions, no fast-path carve-outs:

1. **All work happens in a worktree.** Never edit, commit, or stage anything in the main checkout's
   working tree. The worktree is created in Phase 0 and removed in Phase 4 step 13. There is no
   scenario in which code is written on `main` directly.
2. **`main` changes only through PRs.** Every commit to `main` lands via a reviewed, merged PR. Never
   push a commit or branch directly to `main`; never `git push origin main`; never merge without the
   PR open, reviewed, and green. The only main-branch writes this workflow performs are squash-merges
   of PRs (Phase 4 step 11) and the tag push that cuts a release (Phase 4 step 12).
3. **Coding agents never self-attribute.** No `Co-Authored-By` trailers, no "Generated with"
   footers, no `🤖`/AI credit markers of any kind — in commit messages, PR bodies, code comments,
   docs, or READMEs. This applies to direct work and to anything delegated to a subagent. An
   otherwise-perfect commit that carries attribution is rejected and rewritten.
4. **Every PR is reviewed by a Rust-expert pass, an architecture pass, and an independent security
   pass.** No PR opens, merges, or is pushed through the review loop without a dedicated Rust-review
   dispatch, a dedicated architecture-review dispatch, AND a dedicated `security-review` dispatch
   (this CLI's built-in read-only security specialist, `task` tool `agent_type: "security-review"`)
   on record. Fast-path and trivial PRs included — there is no size-based carve-out. All three must
   pass (or their issues resolved) before merge. See "How dispatch works in this environment" below
   for exactly how each is invoked.
5. **The security review is independent.** The security-review dispatch receives **only the PR
   diff** — never the spec, never the plan, never the task brief, never the PR body summary, never
   the implementer's report. Its findings must be produced from the diff alone, so it cannot be
   steered by the implementer's framing. This matters here specifically: this repo handles API keys
   (OS keyring, never plaintext on disk), spawns shell commands (`tool-bash`), fetches arbitrary URLs
   (`tool-web`), and loads third-party plugin code (WASM/WIT and native) — a diff-only pass is the
   one review that evaluates what was actually built, unmediated.
6. **A public interface change is a deliberate, documented change.** The interfaces customers and
   agents bind to are the **SPP wire format** (`crates/savvagent-protocol/SPEC.md` — `CompleteRequest`,
   `CompleteResponse`, `StreamEvent`, content blocks), the **`ProviderHandler` / `ProviderClient`
   traits** (`savvagent-mcp`), the **tool MCP surfaces** (`tool-fs`, `tool-bash`, `tool-grep`,
   `tool-web`, `tool-lsp` — tool names and input schemas), the **plugin ABI**
   (`savvagent-plugin`/`savvagent-plugin-wit`/`savvagent-plugin-wasm`), the **slash-command surface**
   and **env vars** documented in `README.md`, and the **on-disk transcript/keyring formats**
   (`~/.savvagent/transcripts/*.json`, OS keyring entries under service `savvagent`). Additive
   changes — a new tool, a new optional field, a new provider, a new env var with a default — are the
   normal case and need no special gate while the workspace shares one pre-1.0 version. A **breaking**
   change — renaming or removing a tool, field, or slash command; changing a `StreamEvent` variant's
   shape; changing the on-disk transcript or keyring format in an incompatible way — is never an
   incidental refactor side-effect and never a fast-path change. It must be named in the spec and the
   plan, flagged explicitly to the architect reviewer, called out in `CHANGELOG.md`, and reflected in
   the version bump this repo's SemVer convention requires (pre-1.0: MINOR for
   features/breaking changes, PATCH for fixes — see `RELEASING.md`).
7. **The host-swap and transport-boundary rules in `CLAUDE.md` outrank the task brief.** Per-turn
   worker tasks clone `Arc<Host>` under a brief read lock and drop the guard before any `.await` —
   never hold the `RwLock` across an await. The `Host` only ever sees `Box<dyn ProviderClient>` and
   must never gain a provider registry of its own. Tools are always stdio child processes owned by
   `ToolRegistry`. A brief that asks for something violating one of these is a brief to escalate on
   (Stop & Escalate condition 9), not to implement.
8. **Every merge to `main` cuts a release.** No PR merges and is considered shipped without a
   version bump, a `CHANGELOG.md` entry, a pushed `vX.Y.Z` tag, and a published GitHub Release with
   build artifacts (Phase 4 step 12, following `RELEASING.md`'s manual process). This is not optional
   and not batchable across PRs — cut the release as part of closing out the PR that necessitated it.

## When to Use This Skill vs. Alternatives

| Situation                                                              | Use                                                     |
| ------------------------------------------------------------------------ | -------------------------------------------------------- |
| Any feature/fix in savvagent-cli, full lifecycle, no human in the loop   | **savvagent-development** (this skill)                  |
| Work in another repository                                               | `general-development`                                   |
| Already mid-implementation, just need to address PR review comments      | the Review-Response step here (Phase 4 step 9)          |
| Spec/plan only, will hand off to a human implementer                     | Phases 1–2 of this skill                                |
| Guided mode with human approval at each checkpoint                       | run the phases directly, stopping at each gate          |
| One-line typo fix or docs nit                                            | Fast-path below — the full spec/plan phases are overkill |

## Fast-Path: Trivial Tasks (skip the spec + critique loops)

This repo is **plan-by-plan by house style** (see the existing `docs/superpowers/plans/` and
`docs/superpowers/specs/` directories) — but genuine triviality does not need a design spec. Skip the
spec document, the spec critique, and the plan critique ONLY when **ALL** of the following are true:

- Single-file or 1–2 logical source files (tests and lock files don't count toward the cap; a file
  and its required mirror/duplicate count as one logical file)
- No new public interface: no new MCP tool, no new slash command, no new SPP wire type, no new
  `ProviderHandler`/`ProviderClient` method, no new crate, no new plugin ABI surface
- No **breaking** change to the SPP wire format, a tool's input schema, the plugin ABI, the
  slash-command surface, or the on-disk transcript/keyring format (Non-Negotiable Rule 6) — breaking
  changes are never fast-path
- No change to the host-swap `RwLock` discipline, the provider transport split (in-process vs. MCP
  HTTP), the stdio tool transport, or the `rmcp` `ProgressDispatcher` forwarder-abort pattern
- No change to crate boundaries (no crate gains a new dependency edge; `Host` does not gain a
  provider registry)
- No behavior change on a code path covered by tests (a type-only fix is fine; a logic change that
  alters runtime behavior is not)
- No change to deploy/distribution shape (`.github/workflows/`, `Cargo.toml`'s
  `[workspace.metadata.dist]`, packaging scripts)
- The acceptance criterion fits in one sentence

Concrete examples that qualify:

- Fix a type error with no behavior delta
- Fix a typo in a string / comment / docstring
- Rename a local variable
- Delete demonstrably dead code (verified zero callers via `cargo build --workspace`)
- Run `cargo fmt --all` over a crate
- Update a hardcoded constant the brief names verbatim

**Even when fast-pathing, the plan document is not skipped.** Per house style, the plan lives in the
repo — a fast-path ticket still lands a minimal single-task plan at
`docs/superpowers/plans/YYYY-MM-DD-<slug>.md` in the plan's task form (Goal + one `## Task` with
`- [ ]` steps + TDD + commit step). The design spec and both critique loops are the parts that are
skipped. Add to the PR body: `Fast-path: no design spec per savvagent-development trivial-task
criteria — <reason>.`

If you find yourself rationalizing into the fast-path on something that touches 3+ source files,
introduces a new interface, touches the host-swap or provider-transport rules, adds a new crate, or
has more than a one-sentence AC → STOP. Write the spec. The fast-path is for genuine triviality, not
"I think this is small."

| Fast-path rationalization                                     | Reality                                                                                                                          |
| ---------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| "It's only 3 files"                                               | Fast-path caps at 2. Three files → spec.                                                                                        |
| "The new MCP tool is tiny"                                        | A new tool is a new public interface with its own input schema and description an LLM has never read the docs for. Spec.        |
| "I'll add a provider registry to `Host` for this one case"        | The host only ever sees `Box<dyn ProviderClient>`; a registry inside the host is a design defect the architect review will flag. |
| "I'll hold the `RwLock` across this one await, it's quick"        | That's the exact bug class the host-swap rule exists to prevent. Spec it, or don't do it.                                        |
| "The type fix incidentally fixes a bug"                           | If behavior changes, you need the spec to record what it changed and why.                                                         |
| "I'll fast-path the first sub-change and spec the rest"           | If the work splits into sub-changes, write the spec. Multi-step work doesn't fast-path.                                          |
| "No spec, but I'll still write a one-line plan"                   | Either the work needs a plan (then write the spec too) or it doesn't (then it doesn't need the plan either — and per house style, even fast-path keeps a minimal plan doc). |
| "This PR is tiny, I'll skip cutting a release for it"              | Rule 8 has no size carve-out. Cut the release.                                                                                    |

## Repository Conventions (savvagent-cli)

| Convention            | Value                                                                                                                                                                                                                                                                                                                     |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Repo                   | `savvagent/savvagent-cli` — pass `--repo savvagent/savvagent-cli` on `gh` commands run from a worktree or outside the checkout.                                                                                                                                                                                          |
| Trunk                  | `main`. **All** work happens in a worktree; **all** main-branch changes land via merged PRs. No direct commits, pushes, or merges to `main` outside a PR (Non-Negotiable Rules 1–2).                                                                                                                                     |
| Worktree               | **Required.** `git worktree add .claude/worktrees/<branch> -b <branch> origin/main` — worktrees live **inside the repo** at `.claude/worktrees/<branch>` (already gitignored) — **never `git add .`** in the main checkout. Branch off `origin/main`, never local `main` (see Phase 0 trunk-sync). Every code edit, commit, and push happens from inside this worktree. |
| Branch name            | `<area>/<kebab-slug>` — area is a crate short name (`host`, `protocol`, `mcp`, `provider`, `tool-fs`, `tool-lsp`, `canvas`, `plugin`) or a theme (`ci`, `docs`, `release`).                                                                                                                                                |
| Commit format          | `<scope>: <subject>` — scope is a crate directory or area: `savvagent-host:`, `savvagent-protocol:`, `savvagent-mcp:`, `provider-anthropic:`, `tool-fs:`, `tool-lsp:`, `savvagent-canvas:`, `docs:`, `ci:`, `release:`. Squash-merge to `main` via PR.                                                                    |
| AI attribution         | **Never.** No `Co-Authored-By`, no "Generated with", no `🤖`/AI credit markers in commits, PR bodies, comments, or docs — direct work or subagent work (Non-Negotiable Rule 3).                                                                                                                                            |
| Spec storage           | **Repo file, committed.** `docs/superpowers/specs/YYYY-MM-DD-<slug>-design.md` — never a tracker comment, never uncommitted.                                                                                                                                                                                              |
| Plan storage           | **Repo file, committed.** `docs/superpowers/plans/YYYY-MM-DD-<slug>.md` — same rule.                                                                                                                                                                                                                                       |
| Plan format            | Read a recent plan under `docs/superpowers/plans/` first and match it: a `# <slug> Implementation Plan` title, a **Goal** paragraph, **Architecture**, **Tech Stack**, a `**Spec:**` line pointing at the committed design doc, a **File Map** (or "File structure"), then one `## Task N: <name>` per task with `- [ ]` steps in failing-test-first order, exact file paths, exact commands, and a final format-and-commit step. Existing plans vary on extras (some add `**Dependency:**`/`**Depends on:**` lines for cross-plan ordering) — this skill additionally asks for `**Release line:**` and `**Branch:**` lines (see Phase 2 step 5) to make the mandatory release-cut (Non-Negotiable Rule 8) traceable; add them even though not every pre-existing plan carries them. |
| Record-as-shipped      | On completion, flip the spec's `> **Status:**` to IMPLEMENTED and tick the plan's remaining `- [ ]` boxes, then commit as `docs: record <…> as shipped`. There is no archive directory — do not move the files.                                                                                                          |
| Conventions of record  | [`CLAUDE.md`](../../../CLAUDE.md) at the repo root is the load-bearing document — read it before any non-trivial change, and treat it as outranking anything here that has drifted. `PRD.md` is the vision/scope of record, `crates/savvagent-protocol/SPEC.md` is the wire-format spec of record, `README.md` documents developer-facing conventions. Docs can still lag the code. Read the code. |
| Build                  | `cargo build` builds everything (required even for TUI-only work, because the TUI spawns `savvagent-tool-fs` at runtime). `cargo run -p savvagent` runs the TUI.                                                                                                                                                          |
| Test command           | `cargo test --workspace` (from the root). Per crate: `cargo test -p savvagent-host`, `cargo test -p savvagent-host -- name::of::test` for a single test. Headless smoke test: `cargo run -p savvagent-host --example headless -- "list my Cargo.toml"` (needs a provider — see README "Running providers as standalone MCP servers"). No database, no external services — tests are self-contained.                                                                                                             |
| Lint / format          | `cargo fmt --all --check` and `cargo clippy --workspace --all-targets` (CI runs with `RUSTFLAGS=-D warnings`). `bacon` (default job `check`), `bacon clippy-all` (clippy across the workspace), `bacon test` for continuous local checking.                                                                              |
| Linux system deps      | `libdbus-1-dev` (keyring's secret-service backend) and `libfontconfig1-dev` + `pkg-config` (Blitz, pulled in by `savvagent-canvas` and `savvagent` via the `internal:html-canvas` built-in plugin) — neither is preinstalled on the GitHub `ubuntu-latest` runner image; install both before `cargo build`/`test`/`clippy` on a fresh Linux box.                                                                                                                                                                     |
| CI                     | `.github/workflows/ci.yml` — `lint` (fmt + clippy, Linux only), `test` (matrix: ubuntu/macos/windows — Windows excludes `savvagent-canvas`/`savvagent` due to a Blitz font-discovery hang on the GH Windows runner), `cross-vendor-gate` (`cargo test -p savvagent-host --test cross_vendor_history`), `dist-plan` (validates the `cargo-dist` config parses). Runs on every PR and on pushes to `master`/`main`. **The merge gate is the CI run YOUR merge commit triggered, by run ID** — never "the latest run". |
| Release                | **Manual** per `RELEASING.md` (release-plz automation is currently broken upstream — see that file for the tracking issue). Bump `workspace.package.version` + every internal `workspace.dependencies` version in `Cargo.toml`, update `CHANGELOG.md`, tag `vX.Y.Z`, push the tag. `.github/workflows/release.yml` (cargo-dist) then builds and publishes platform binaries/installers; `.github/workflows/package-linux.yml` attaches `.deb`/`.rpm` automatically afterward.                                    |
| Known pre-existing gap | Windows test coverage for `savvagent-canvas`/`savvagent` is intentionally excluded from CI (Blitz font-discovery hang at process init) — do not treat this as a regression you caused; see the comment in `ci.yml`.                                                                                                       |
| Versioning             | The whole workspace shares one pre-1.0 `0.MINOR.PATCH` version (`workspace.package.version` in the root `Cargo.toml`). Per `CHANGELOG.md`'s convention: MINOR bumps for features and breaking changes, PATCH for fixes. Public-interface changes are governed by Non-Negotiable Rule 6.                                  |

Full convention reference is [`CLAUDE.md`](../../../CLAUDE.md) at the repo root. The above is the
load-bearing subset for this workflow.

## Load-Bearing Invariants (get these right — every review step below checks them)

These are not style rules; they are the invariants this repository is built around. `CLAUDE.md`
explains the reasoning behind each — read it, and treat the list here as the checklist.

1. **Everything is MCP-shaped.** A provider is just a `ProviderHandler` (`savvagent-mcp`); a tool is
   just a stdio MCP server. Providers are linked **in-process by default** via
   `InProcessProviderClient` — a standalone binary form (`savvagent-anthropic`, etc.) exists only for
   wire-protocol debugging, not as the primary path.
2. **The turn loop lives in `Host`.** `Host::run_turn_streaming` (in `savvagent-host`) loops
   `provider.complete` → `tool_registry.call` until the model emits `end_turn`, forwarding
   `StreamEvent`s as it goes. Tool-use looping, session state, and project-context loading
   (`SAVVAGENT.md`, if present) all live here — the TUI is a thin shell on top. A behavior change to
   the turn loop belongs in `savvagent-host`, not in `crates/savvagent`.
3. **The host-swap `RwLock` rule is never violated.** The TUI keeps the active host as
   `Arc<RwLock<Option<Arc<Host>>>>`. Per-turn worker tasks clone the `Arc<Host>` under a brief read
   lock and **drop the guard before any `.await`** — never hold the `RwLock` across an await.
   `/connect` swaps the slot atomically. See `crates/savvagent/src/app.rs` and `tui.rs`.
4. **The provider transport split is a hard boundary.** In-process (`InProcessProviderClient`) is the
   default; MCP-over-HTTP (`rmcp`'s Streamable HTTP transport) is opt-in, selected only when
   `SAVVAGENT_PROVIDER_URL` is set. `Host` only ever sees `Box<dyn ProviderClient>` and must never
   know which path is active — **there is no provider registry inside the host.**
5. **Tools are always stdio child processes, owned by `ToolRegistry`.** They're reaped on shutdown.
   New tools are wired via `HostConfig::with_tool` in `crates/savvagent/src/main.rs`, mirroring
   `crates/tool-fs`'s stdio-MCP-server shape.
6. **The `rmcp` `ProgressDispatcher` forwarder-abort pattern must be preserved.**
   `subscriber.next()` from `rmcp`'s `ProgressDispatcher` does **not** auto-close when the RPC
   completes. Forwarder tasks that pump progress notifications must `JoinHandle::abort()` after the
   request future resolves, or the caller's mpsc waiter deadlocks. Used in `provider-anthropic`'s and
   `provider-gemini`'s streaming paths — replicate it exactly for any new streaming provider.
7. **Secrets never touch disk in plaintext.** API keys live in the OS keyring under service
   `savvagent`, account `<provider id>`; `/connect` is the only writer. Never log a key, never echo
   one in an error or a transcript, never commit one, never write one to a transcript JSON.
8. **Sandboxed execution is a real security boundary, not a courtesy.** `tool-bash` runs arbitrary
   shell commands and `tool-web` fetches arbitrary URLs on the model's behalf; plugin code
   (`savvagent-plugin-wasm`/`savvagent-plugin-wit`) runs inside the process. Any change that widens
   what a tool or plugin can reach (filesystem scope, network egress, process spawning) is a
   deliberate, reviewed change — never an incidental side effect of a refactor.
9. **Errors should be actionable for an LLM caller that has never read the docs.** No `unwrap()`
   outside tests, no silent fallback on a resolution failure. Comments explain **why**, especially
   where the obvious implementation is wrong.

## Tracker abstraction (GitHub Issues or ticketless)

savvagent-cli uses **GitHub Issues when an issue exists, ticketless otherwise.** There is no JIRA.
Resolve once at intake and stay on that path for the whole task.

> **Status labels.** This repo does not currently carry `status:in-progress` / `status:in-review`
> labels — only the stock GitHub label set (`bug`, `documentation`, `enhancement`, etc.). If you want
> tracker-visible state transitions, create the labels once
> (`gh label create status:in-progress --repo savvagent/savvagent-cli`, likewise `status:in-review`)
> before using them below; otherwise treat those two transition rows as optional and rely on the PR
> itself (open = in progress, ready-for-review = in review) as the visible state.

| Lifecycle step     | GitHub Issues                                                                       | Ticketless                                                                 |
| ------------------- | ------------------------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| Ref form            | `savvagent/savvagent-cli#123` (`#123` short)                                          | the captured task brief                                                    |
| Intake / read AC    | `gh issue view <n> --repo savvagent/savvagent-cli --json title,body,labels`           | the user's instruction, captured verbatim in working memory + the PR body  |
| → In Progress       | `gh issue edit <n> --repo savvagent/savvagent-cli --add-label status:in-progress` (if the label exists; otherwise skip) | n/a — capture `T_impl_start` only                                          |
| → In Review         | `gh issue edit <n> --repo savvagent/savvagent-cli --add-label status:in-review` (if the label exists; otherwise skip)   | n/a                                                                        |
| Spec / Plan record  | committed to `docs/superpowers/specs/` / `docs/superpowers/plans/`; reference the paths from the PR body | same — the repo docs ARE the durable record                                |
| Close               | `gh issue close <n> --comment "…"` (the merged PR's `Closes #N` may have done it)     | n/a — the Phase 6 summary is the close-out                                 |
| Branch              | `<area>/<slug>`                                                                        | `<area>/<slug>`                                                            |
| Commit subject      | `<scope>: <subject>`                                                                   | `<scope>: <subject>`                                                       |
| PR linkage          | body contains `Closes #N`                                                             | body restates the task brief as the AC                                     |

On the ticketless path there is no worklog sink, so the Phase 6 summary's timeline is the only time
record. `T_*` timestamps are still captured at phase boundaries to feed it.

## Phase 0 — Pre-flight (fresh context)

Intake reads ("what does this work want?") get corrupted by prior conversation cruft — stale paths,
abandoned plans, half-finished refactors. **Note:** `README.md`'s `/clear` documents a slash command
in the *savvagent product being developed* (resets its own TUI conversation) — it is not a tool
available to the orchestrating agent running this skill. This orchestrating CLI has no
`/clear`-and-reinvoke primitive of its own within a session, so the only fresh-context mechanism
available to it is dispatching isolated subagents via the `task` tool (see "How dispatch works in
this environment" below) — every reviewer, critique, and implementer step in this skill runs as its
own `task` dispatch specifically so it gets a clean context window built from a self-contained
prompt, not from whatever has accumulated in the orchestrating session. The orchestrating session
itself stays fresh by never doing the reviewing/implementing work inline — it reads sources, writes
the spec/plan, dispatches, and aggregates results.

**Not clean context for a dispatched subagent:** a prompt that says "read the plan file" or "see
above" instead of pasting the actual text inline. Every dispatch prompt must be self-contained —
this is why the templates in `agent-prompts.md` say "paste verbatim, do not improvise."

**Branch + worktree safety:**

1. Run `git branch --show-current` in the current working directory.
2. **Trunk-sync check (mandatory).** Before any worktree creation, verify local trunk is in sync with
   origin:
   ```bash
   git fetch origin
   git rev-list origin/main..main               # MUST be empty
   ```
   If it returns commits, **local main is ahead of origin** — those commits will silently inherit
   into the new branch and contaminate the PR diff against `origin/main`. Surface them to the user
   (commits + their file paths); do NOT discard. They represent unpushed work that needs handoff
   BEFORE the new worktree is created.
3. If on `main` (or any trunk), create the worktree branching from `origin/main` explicitly (NOT
   local `main`) — belt-and-suspenders against step 2's check ever drifting:
   ```bash
   git worktree add .claude/worktrees/<branch> -b <branch> origin/main
   ```
   Worktrees live inside the repo at `.claude/worktrees/<branch>` (already covered by
   `.gitignore`). Never code on `main` directly.
4. If already on a feature branch in a worktree → proceed there.
5. `git status --porcelain` must be clean in the worktree before any code edit. Surface unexpected
   uncommitted changes; do NOT discard them.
6. **`main` is write-protected by policy.** The only way a commit reaches `main` is a reviewed,
   merged PR. Never commit to the main checkout's branch, never `git push origin main`, never merge
   a branch into `main` by hand. If the work needs to touch `main` (e.g. the record-as-shipped
   commit, or the release version bump), it does so through its own worktree + PR like everything
   else.

Create a todo (via the `sql` tool's `todos` table) for each phase (1–6), plus one per plan task once
the plan exists, and update `status` as you go (`pending` → `in_progress` → `done`/`blocked`). Use
`todo_deps` to record that Phase 2 depends on Phase 1, each implementation task depends on the plan
being committed, etc.

## How dispatch works in this environment

This skill is written for the GitHub Copilot CLI's `task` tool, not Claude Code's `Agent` tool —
there is no `subagent_type` catalog with `rust-pro` / `architect-reviewer` / `security-auditor` /
`code-reviewer` names. Every dispatch in this skill maps to a `task` tool call with one of this
CLI's seven `agent_type`s (`explore`, `task`, `general-purpose`, `rubber-duck`, `code-review`,
`security-review`, `research`). Use this table everywhere `agent-prompts.md` says
"`subagent_type: X`" in the original template shape:

| Role in this skill                                            | `agent_type`        | Why                                                                                                                     |
| ----------------------------------------------------------------- | ---------------------- | --------------------------------------------------------------------------------------------------------------------- |
| Spec critique, plan critique (Phase 1 step 4, Phase 2 step 6)      | `rubber-duck`          | Purpose-built for high-signal feedback on plans/specs: catches logic/design flaws, ignores style — exactly this job.    |
| Implementer, spec compliance review, review-response (Phase 3 A/C, Phase 4 step 9) | `general-purpose`     | Needs the full toolset (edit files, run cargo, use git/gh) and high-quality reasoning.                                  |
| Code quality review, final code review (Phase 3 E/H)               | `code-review`          | Purpose-built read-only diff reviewer; reports only high-confidence bugs/logic errors against a real change set.       |
| Rust-expert pass, architecture pass (Phase 4 step 8 trio)          | `general-purpose`      | Needs to read `CLAUDE.md` + the Load-Bearing Invariants and reason about idiomatic Rust / crate boundaries, not just the diff. |
| Independent security pass (Phase 4 step 8 trio)                    | `security-review`      | This CLI's built-in security specialist — mandatory per this skill's Non-Negotiable Rule 4, invoked explicitly for every PR. |

**Sync vs. background.** Dispatch sequential, order-dependent steps (implementer tasks, spec/plan
critique loops, spec-compliance review) with `mode: "sync"` — you need the result before deciding the
next action. The mandatory review trio (Phase 4 step 8) is three **independent** read-only passes:
issue all three `task` calls in the same response (parallel) rather than one after another, or use
`mode: "background"` if you want to keep working on something else (e.g. drafting the PR-comment
aggregation skeleton) while they run; then use `read_agent`/wait-for-notification to collect results.

**Model selection.** Where the plan/spec calls for "mechanical, 1–2-file task" pick a fast model
(e.g. `model: "claude-haiku-4.5"` or `reasoning_effort: "low"` on the default model). For
multi-file integration work or design-judgment tasks, omit `model`/`reasoning_effort` overrides (or
raise `reasoning_effort` to `"high"`/`"xhigh"`) so the dispatch inherits strong reasoning.

**Every dispatch prompt must be fully self-contained** — paste the actual spec/plan/task text
inline; a dispatched agent has no access to this session's context or files it hasn't been told
about explicitly (it does have full repo filesystem/tool access, but not this conversation's memory).

**The security-review dispatch has a mandatory output contract** (defined in the orchestrating
Copilot CLI's own system instructions for its `security-review` agent type — not something visible
by reading this repo, so don't expect to find it in a repo file): after it completes, findings must
be presented as a table using the severity emoji (🔴 CRITICAL / 🟠 HIGH / 🟡 MEDIUM / ⚪ LOW), and the
generic contract then calls for `ask_user` to offer follow-up actions. **This skill overrides the
follow-up-question step for autonomy:** present the table as specified, but instead of blocking on
`ask_user`, auto-resolve per the Phase 4 step 8 fix-loop rule below (Critical/High → fix now and
re-review; Medium/Low → log in the per-task ledger and continue) so a fully autonomous run never
stalls waiting on a human. If you are running this skill in guided/interactive mode with a human
present, the `ask_user` step may be used as written instead.

**There is no in-session equivalent of "a subagent cannot recursively dispatch further subagents."**
Every `task` dispatch in this CLI is issued by the top-level orchestrating session, never by another
dispatched agent trying to spawn its own subagents — so there is nothing to collapse or defer here.
If a dispatched `general-purpose` agent reports that a review or sub-task genuinely needs further
isolated investigation, the orchestrating session (you) issues that follow-up `task` call directly.

## Phase 1 — Intake + spec

> **Fast-path note:** Steps 1 + 2 always run. Steps 3 + 4 (spec draft + critique) are skipped if the
> work qualifies under "Fast-Path: Trivial Tasks." When fast-pathing, jump from step 2 directly to
> Phase 2 step 5 and write the minimal single-task plan.

### Step 1: Read the source directly

- **GitHub:** `gh issue view <n> --repo savvagent/savvagent-cli --json title,body,labels` — the body
  is the AC.
- **Ticketless:** capture the user's instruction verbatim in working memory. That string is the AC
  for the rest of the run; restate it in the PR body so the contract is durable.

If a teammate summarized it, still read the source — summaries lose AC.

### Step 2: Transition / mark In Progress

- **GitHub:** `gh issue edit <n> --repo savvagent/savvagent-cli --add-label status:in-progress` (only
  if the label exists — see the Tracker abstraction note above).
- **Ticketless:** nothing to transition.

**Capture `T_impl_start = now`** in ISO-8601 with explicit timezone offset. Hold for the Phase 6
summary.

### Step 3: Spec draft (committed, no human review)

**The spec IS a repo file in this repository** — the plan-by-plan convention requires it. Create
`docs/superpowers/specs/YYYY-MM-DD-<slug>-design.md`, following the structure of the most recent spec
under `docs/superpowers/specs/` (read one first):

- Title: `# <Change> — design` — descriptive, sentence case
- Header block: `Date:`, `Status:` (`pending review`, flipped to `IMPLEMENTED` at close-out),
  optional `Roadmap:` / `Related:` lines when the change is a phase of a larger plan or ties to a
  `PRD.md` item
- **Problem** — what's missing / broken, in the terms `PRD.md` and `CLAUDE.md` use
- **Approach** — the shape of the fix, referencing the actual crate layout (`crates/savvagent`,
  `savvagent-host`, `savvagent-protocol`, `savvagent-mcp`, `provider-*`, `tool-*`, `savvagent-plugin*`,
  `savvagent-canvas`) and citing `file:line` for existing code the design touches
- **Scope** with **In:** and **Out:** — explicit non-goals
- **Public-interface changes** — call out explicitly whether the change is additive or breaking to
  the SPP wire format, a tool's MCP schema, the plugin ABI, the slash-command surface, or the
  on-disk transcript/keyring format (Non-Negotiable Rule 6)
- **Premise corrections** — if the task brief's premises do not survive contact with the repository
  (`CLAUDE.md`, `PRD.md`, and `README.md` can each be ahead of or behind the code), record the
  corrections explicitly instead of silently building to the wrong premise

Required sections, wherever they fit: **Assumptions** (every choice made without asking, each with a
one-line rationale — the highest-value section), **Goal & Success Criteria** (one paragraph + 3–5
measurable bullets), **Error Handling & Edge Cases**, **Risks & Open Questions**.

**Commit the spec draft** as `docs: add <slug> design spec` before critique. The critique loop below
revises the _committed file_ in place (new commits per round), never a working-memory-only copy.

### Step 4: Spec critique subagent

Dispatch using the **`Spec Critique`** template in [`agent-prompts.md`](agent-prompts.md) — **read
that file now and paste the template verbatim; do not improvise the prompt body.**
`agent_type: "rubber-duck"` (fallback `"general-purpose"`). Fill `<PASTE FULL SPEC TEXT>` from the committed spec and cite the
source ref. In the Repo Profile placeholder, paste the "Load-Bearing Invariants" section above and
the relevant Repository Conventions.

**Maximum 2 revision rounds (3 reviewer dispatches total).** On Issues Found, revise the spec in the
committed file and redispatch with the updated text inline. If issues remain after the third pass,
append them to `Risks & Open Questions` and continue. Do NOT loop further.

**When the loop converges, commit the approved version** (once) and note the spec path in the plan
and the PR body.

## Phase 2 — Plan

> **Fast-path note:** On a fast-path ticket, write the minimal single-task plan directly — skip the
> critique loop.

### Step 5: Plan draft (committed, no human review)

Write the plan in this repository's established format — read a recent plan under
`docs/superpowers/plans/` first and match it:

- Title: `# <slug> Implementation Plan`
- **Goal** paragraph, **Architecture** paragraph, **Tech Stack** paragraph
- **Spec:** line pointing at the committed design spec — "read it first. This plan implements it
  exactly."
- **Release line:** the next `vX.Y.Z` this work ships as (per the SemVer convention in
  `CHANGELOG.md`). Not every pre-existing plan has this line — this skill adds it so the mandatory
  release-cut (Non-Negotiable Rule 8) is traceable back to the plan that necessitated it.
- **Branch:** the branch name this plan lands on. Same rationale — add it even if the plan you used
  as a template didn't have one.
- **File Map** (or "File structure") — grouped by **New crate/files** and **Modified files**, each
  with a one-line responsibility
- One `## Task N: <name>` per task, listing **Files:** (Create/Modify), then `- [ ]` steps in
  **failing-test-first order**: write failing test → run → implement → run → commit. Include exact
  file paths, exact commands (`cargo test -p savvagent-host`, `cargo test -p savvagent-host -- <test
  name>`, `cargo clippy --workspace --all-targets`, `cargo fmt --all`), and the expected result of
  each run

Every task MUST include:

- Exact file paths in THIS repo's layout
- **A step recording whether a public interface changed** (SPP wire format, tool schema, plugin ABI,
  slash command, env var, on-disk format) — additive changes get a one-line CHANGELOG note; breaking
  changes get the full treatment from Non-Negotiable Rule 6
- **A step verifying the host-swap `RwLock` rule** whenever the task touches `crates/savvagent/src/app.rs`
  or `tui.rs` — no `.await` may execute while the read guard is held
- **A step preserving the `ProgressDispatcher` forwarder-abort pattern** whenever the task adds or
  modifies a streaming provider path
- The repo's actual test + lint commands for the TDD steps
- A final "Format and commit" step: `cargo fmt --all` + `git commit -m "<scope>: <subject>"`
- A final task in the plan (the last `## Task N`) that bumps `workspace.package.version` and every
  internal `workspace.dependencies` version in `Cargo.toml`, adds the `CHANGELOG.md` entry, and notes
  the release will be cut per `RELEASING.md` after merge (Phase 4 step 12)

**Commit the plan draft** as `docs: add <slug> implementation plan` before critique.

### Step 6: Plan critique subagent

Dispatch using the **`Plan Critique`** template in [`agent-prompts.md`](agent-prompts.md) — **read
that file now and paste the template verbatim.** `agent_type: "rubber-duck"` (fallback `"general-purpose"`). Fill `<PASTE FULL
PLAN TEXT>`, `<PASTE FULL SPEC TEXT>`, and the Repo Profile (Load-Bearing Invariants + conventions +
test/lint commands).

Same revision-loop shape as step 4 (revise the committed file, redispatch with the updated text
inline). **Maximum 2 revision rounds.** If unresolved issues remain, prepend a `## Known Plan Gaps`
section and continue.

**When the loop converges, commit the approved version** (once).

## Phase 3 — Implement

Read the plan ONCE, then extract every task's full text + context into your own working memory.
Create one todo (via the `sql` tool) per task. **Do NOT make implementer subagents re-read the plan** —
provide them the full task text inline.

**Sequential, not parallel.** Implementer subagents on the same branch will conflict on the working
tree. Parallelism happens across separate worktrees on separate features, not within one run.

For each task in plan order:

### A. Dispatch implementer

Dispatch using the **`Implementer Dispatch`** template in [`agent-prompts.md`](agent-prompts.md) —
**read that file now and paste the template verbatim** (the `## AUTONOMOUS MODE` block and `## Report
Format` are load-bearing). `agent_type: "general-purpose"`, `mode: "sync"`. Fill the task text, the Context block
(include the relevant Load-Bearing Invariants + the repo's test/lint commands), the source ref, and
the worktree path.

**Model selection:**

- Mechanical 1–2-file tasks with complete specs → a lightweight/fast model
- Multi-file integration work → omit (inherit parent)
- Design-judgment tasks the plan explicitly flags → the parent's highest-capability model

### B. Handle implementer status

| Status               | Action                                                                                                                                  |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `DONE`                | Proceed to spec compliance review (step C)                                                                                             |
| `DONE_WITH_CONCERNS`  | If correctness/scope: dispatch fix subagent now with the specific concern as the new task. If minor: log in per-task ledger and proceed |
| `NEEDS_CONTEXT`       | If discoverable in the repo, re-dispatch with the context filled in. If genuinely unknowable: treat as BLOCKED                          |
| `BLOCKED`             | See Stop & Escalate below                                                                                                               |

### C. Spec compliance review

Dispatch using the **`Spec Compliance Review`** template in [`agent-prompts.md`](agent-prompts.md) —
**read that file now and paste the template verbatim** (the `## CRITICAL: Do Not Trust The Report`
block is load-bearing — the reviewer must read the actual commits, not the implementer's claims).
`agent_type: "general-purpose"`, `mode: "sync"`. Fill the task text and the implementer's report verbatim.

### D. Spec fix loop (max 2 fix dispatches)

- ✅ → quality review (step E).
- ❌ → re-dispatch implementer with status `FIX_SPEC_ISSUES`, supplying the reviewer's findings as
  the new task. Re-run spec review.
- **Three failed spec reviews in a row → escalate.**

### E. Code quality review

Capture commit boundaries:

- `BASE_SHA = git rev-parse HEAD~<N>` where N = commits this task produced
- `HEAD_SHA = git rev-parse HEAD`

Dispatch using the **`Code Quality Review`** template in [`agent-prompts.md`](agent-prompts.md) —
**read that file now and paste the template verbatim.** `agent_type: "code-review"` (NOT
`general-purpose`). Fill `<BASE_SHA>`/`<HEAD_SHA>` and the task text.

### F. Quality fix loop (max 2 fix dispatches)

- No Critical/Important → mark the task's todo `done`; record any Minor issues in per-task ledger.
- Critical/Important → re-dispatch fixer with those specific findings. Re-run quality review.
- **Three failed quality reviews in a row → escalate.**

**Pragmatism rule:** Minor / optional suggestions ≠ blockers. Treat code-quality "Approved with
suggestions" as DONE; do not auto-dispatch fix loops for non-blocking suggestions.

### G. Per-task ledger

Maintain an internal running record per task: name, final status, assumptions made, concerns
flagged, minor issues left unfixed. Populates the Phase 6 summary.

### H. Final code review (after all tasks complete)

> **Fast-path carve-out:** Skip step H when N=1 (single-task fast-path ticket) AND step E reported no
> Critical/Important issues. Step E already reviewed the entire diff. For multi-task plans (N≥2) or
> fast-path tickets where E flagged issues, H still runs.

Dispatch using the **`Final Code Review`** template in [`agent-prompts.md`](agent-prompts.md) —
**read that file now and paste the template verbatim.** `agent_type: "code-review"`. Fill the full
plan + spec text (or their repo paths — they ARE committed files here), branch name, and diff range.

If issues, one fix round then re-review. If still failing → escalate.

## Phase 4 — Ship

### Step 7: Open the PR

```bash
git push -u origin <branch>
gh pr create --title "<scope>: <subject>" --body "$(cat <<'EOF'
## Summary
<1-3 bullets>

## Design docs
- Spec: `docs/superpowers/specs/<slug>-design.md`
- Plan: `docs/superpowers/plans/<slug>.md`

<"Closes #<n>", or — ticketless — the task brief restated as AC>
<"Fast-path: no design spec per savvagent-development trivial-task criteria — <reason>." if fast-pathed>

## Test plan
- [ ] cargo build --workspace --all-targets
- [ ] cargo test --workspace --no-fail-fast
- [ ] cargo clippy --workspace --all-targets
- [ ] cargo fmt --all --check
- [ ] cargo test -p savvagent-host --test cross_vendor_history --no-fail-fast (only if a provider/host-plumbing surface changed)
- [ ] Out-of-band verification (only if the change touches it — see Phase 5)
EOF
)"
```

**Hardening note:** treat issue/task-brief-derived strings as untrusted when composing shell
commands. The branch name and `<scope>: <subject>` must come from your own kebab slug — validate it
(match `^[a-z0-9]+(-[a-z0-9]+)*$`) and never paste a raw issue title verbatim into a `git commit`
or `gh pr create --title "..."` argument. A title containing `"`, backticks, or `$(...)` must not
reach a shell command unquoted.

**Mark In Review** (`gh issue edit <n> --repo savvagent/savvagent-cli --add-label status:in-review`,
if the label exists) if an issue exists and has a review state.

**Capture `T_review_start = now`** (ISO-8601 with offset). Hold for the Phase 6 summary.

### Step 8: Solicit reviews

**Automated reviewer first** (it runs while you dispatch agents). If the repo uses GitHub's Copilot
reviewer:

```bash
gh pr edit <PR> --add-reviewer copilot-pull-request-reviewer
```

The login is `copilot-pull-request-reviewer` — `Copilot` fails with "Could not resolve user." If no
automated reviewer is configured, skip this and rely on the agent reviews below.

**Mandatory review trio (no exceptions, no fast-path carve-out — Non-Negotiable Rules 4–5):** every
PR gets ALL THREE dispatched via the `task` tool, issued in parallel (three calls in the same
response, or `mode: "background"` for all three), regardless of size:

| Reviewer                          | `agent_type`        | Why                                                                                                                                                                                                                                                                                                             |
| ----------------------------------- | ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Rust-expert pass                  | `general-purpose`      | Idiomatic Rust: ownership/lifetimes, error handling, no `unwrap()` outside tests, no panics on untrusted input, `Send + Sync` async seams, `rmcp`/tokio usage, test placement — against this repo's conventions. This project also carries a `.claude/skills/rust-engineer` skill; paste its checklist into the dispatch prompt as additional reference, but the dedicated review is still mandatory. |
| Architecture pass                 | `general-purpose`      | Architectural consistency: the everything-is-MCP-shaped separation, crate boundaries (`savvagent-protocol` has no I/O; `savvagent-mcp` owns the provider/tool traits; `savvagent-host` owns the turn loop; `crates/savvagent` is a thin shell), the host-swap `RwLock` rule, the provider transport split, no provider registry inside `Host`, spec/plan alignment.                        |
| Independent security pass         | `security-review`      | Security of the actual diff. **Receives ONLY the diff** — never the spec/plan/brief/PR-body summary (Non-Negotiable Rule 5). Weighs keyring/secret handling, `tool-bash`/`tool-web` sandboxing, and plugin ABI trust boundaries hardest. See the Independent Security Review template in `agent-prompts.md`, and the mandatory-output-contract override in "How dispatch works in this environment" above. |

Both `general-purpose` reviews the actual diff (commit range), not the summary. Treat their findings
like any other review: Critical/Important must be fixed or explicitly dismissed before merge; all
three must clear before merge (step 11).

Add ad-hoc `explore`/`general-purpose` dispatches on top by topic if the change warrants it (e.g. a
rendering-focused pass for `savvagent-canvas`/Blitz changes). The independent security pass above
already covers every PR; for changes touching the keyring, `tool-bash`, `tool-web`, or the plugin
ABI, give it the extra instruction to scrutinize those surfaces hardest.

Aggregate the agent reports into a single PR comment grouped by **Critical / Important / Suggestions
/ Strengths** so review threads stay flat instead of one comment per agent.

### Step 9: Fork a review-response subagent

Once reviews have posted, immediately dispatch a dedicated review-response subagent with fresh
context to avoid polluting the main thread with review-fix churn.

Dispatch using the **`Review-Response Subagent`** template in [`agent-prompts.md`](agent-prompts.md)
— **read that file now and paste the template verbatim** (the fix-or-dismiss + thread-resolve
mutation + inline-reply requirements are load-bearing). `agent_type: "general-purpose"`. Fill the PR
number `<N>` and the source ref.

### Step 10: PR review loop

Human reviewers post on their own schedule. Each new round forks a NEW review-response subagent.
Continue until merged.

**Idempotency required.** Every iteration must be safe to no-op. First action of any loop iteration:
enumerate unresolved threads:

```bash
gh pr view <PR> --json reviewDecision,reviews,statusCheckRollup
gh api repos/savvagent/savvagent-cli/pulls/<PR>/comments
# GraphQL: pullRequest.reviewThreads(first: 100) { nodes { id isResolved comments { ... } } }
```

A comment is "new since last pass" if its thread is unresolved AND your last reply (if any) is older
than the latest comment in that thread. If zero new, exit cleanly — no commits, no replies, no
tracker writes.

> **The merge gate is the CI run YOUR merge commit triggered, by run ID.** `.github/workflows/ci.yml`
> runs `lint` (fmt → clippy), `test` (ubuntu/macos/windows matrix), `cross-vendor-gate`, and
> `dist-plan` on every PR. Capture the run id
> (`gh run list --repo savvagent/savvagent-cli --branch <branch> --limit 5`) and track THAT id —
> "the latest run" is a teammate's merge seconds after yours.

| Iteration state                                                              | Action                                                     |
| ------------------------------------------------------------------------------ | ------------------------------------------------------------ |
| All threads resolved + your CI run green + approval present                    | Merge (step 11)                                            |
| All threads resolved + your CI run green + awaiting human approval             | Idle. Next iteration no-ops                                 |
| Open threads you cannot address (security / missing requirement / ambiguous)   | Escalate per Stop & Escalate                                |
| Same unresolved thread across multiple iterations after you replied            | Wake the human reviewer. Do NOT silently retry              |
| Your CI run failed                                                             | Treat as "fix this" comment. Address before next iteration  |

### Step 11: Merge

**Run `gh pr merge` from the main checkout, NOT from inside the worktree.** From-worktree merge can
corrupt main-checkout staging.

```bash
cd <main-checkout-root>
gh pr merge <PR> --squash --delete-branch
```

**Capture `T_pipeline_start = now`** (ISO-8601 with offset).

### Step 12: Cut the release (mandatory — Non-Negotiable Rule 8)

**Every merge to `main` cuts a release.** This is not optional, not batchable, and not skippable for
"small" PRs. Follow `RELEASING.md`'s manual process (release-plz automation is currently broken
upstream — see that file), from a fresh worktree off the just-updated `main`, itself landing via its
own PR:

```bash
cd <main-checkout-root>
git checkout main && git pull --ff-only
git worktree add .claude/worktrees/release-vX.Y.Z -b release/vX.Y.Z origin/main
cd .claude/worktrees/release-vX.Y.Z
```

1. **Bump the version.** In the root `Cargo.toml`, update `workspace.package.version` and every
   internal `workspace.dependencies` entry's `version` field to match. Run `cargo check --workspace`
   to regenerate `Cargo.lock`.
2. **Update `CHANGELOG.md`.** Rename `## [Unreleased]` to `## X.Y.Z - YYYY-MM-DD` (today's date);
   add a fresh empty `## [Unreleased]` above it. Follow Keep a Changelog categories
   (Added/Changed/Fixed/Removed) and this repo's SemVer convention (pre-1.0: MINOR =
   features/breaking changes, PATCH = fixes).
3. **Validate locally:**
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets
   cargo test --workspace
   ```
4. **Commit, open a PR, and merge to `main`** — same worktree + PR discipline as any other change,
   reviewed like any other PR (the mandatory trio still applies; a version-bump-only PR is a small,
   fast review, not a skipped one).
5. **Tag the merge commit and push the tag:**
   ```bash
   git checkout main && git pull
   git tag -a vX.Y.Z -m "vX.Y.Z"
   git push origin vX.Y.Z
   ```
   This triggers `.github/workflows/release.yml` (cargo-dist), which builds binaries/installers for
   macOS arm64, Linux x86_64/aarch64, and Windows msvc, and publishes the GitHub Release.
6. **`.deb`/`.rpm` packages attach automatically** once `release.yml` finishes, via
   `.github/workflows/package-linux.yml`'s `workflow_run` trigger. If it fails or needs a manual
   re-run: `gh workflow run "Package (deb/rpm)" -f tag=vX.Y.Z`.
7. **Verify the release:** `gh release view vX.Y.Z` — confirm all expected platform
   archives/installers plus `.deb`/`.rpm` are attached.

**Capture `T_release_start = now`** at the tag push (ISO-8601 with offset), and hold the release
version for the Phase 6 summary.

### Step 13: Clean up + record-as-shipped

```bash
cd <main-checkout-root>
git checkout main && git pull --ff-only
git branch -d <branch>                              # -D only if force needed and intentional
git worktree remove .claude/worktrees/<branch>       # required — worktrees live inside the repo
git worktree remove .claude/worktrees/release-vX.Y.Z
```

**Record-as-shipped (mandatory, do not skip):** in a fresh worktree off the updated `main` — the
same `.claude/worktrees/<branch>` + PR flow as the feature itself, never a direct commit to the main
checkout — flip the spec's `> **Status:**` to `IMPLEMENTED` and tick the plan's remaining `- [ ]`
boxes, then commit as `docs: record <…> as shipped` and merge via PR. The files stay where they are;
there is no archive. This is how the plan-by-plan record stays current — the docs ARE the project's
history.

## Phase 5 — Deploy, verify, close

**CI exists; there is no server deploy pipeline.** savvagent-cli ships as cross-platform binaries
built by `cargo-dist` and published as GitHub Release assets (plus `.deb`/`.rpm` packages) — there is
no hosted service to deploy. `.github/workflows/ci.yml` gates every PR and every push to
`master`/`main`; the release build (step 12) IS the deploy. Phase 5 is: confirm YOUR CI run is green,
run the out-of-band checklist for what the change touched, confirm the release artifacts published
cleanly, then close.

### Step 14: Confirm YOUR merge commit's CI run is green

```bash
gh run list --repo savvagent/savvagent-cli --branch main --limit 5   # find the run for YOUR merge SHA
gh run watch <run-id> --repo savvagent/savvagent-cli
```

Track the run by ID, never "the latest run" — a teammate's merge seconds after yours steals it.
`lint`, `test` (all three OSes), `cross-vendor-gate`, and `dist-plan` must all pass.

**Capture `T_verify_start = now`.**

### Step 15: Out-of-band artifact verification

CI does NOT apply these. Verify whatever the change touched, explicitly:

- **TUI runtime wiring** — if `crates/savvagent/src/main.rs`, `app.rs`, or `tui.rs` changed:
  `cargo build --workspace` (the TUI spawns `savvagent-tool-fs` at runtime and needs the binary to
  exist), then `cargo run -p savvagent` and exercise the changed surface manually.
- **A new/changed provider** — if `crates/provider-*` changed: run the headless smoke test —
  `cargo run -p savvagent-host --example headless -- "list my Cargo.toml"` — with the appropriate
  `*_API_KEY` set, and confirm the `ProgressDispatcher` forwarder-abort pattern is intact for any
  streaming path touched.
- **A new/changed tool** — if `crates/tool-*` changed: confirm `HostConfig::with_tool` wiring in
  `crates/savvagent/src/main.rs` still resolves the binary (via `$PATH` or its
  `SAVVAGENT_TOOL_*_BIN` override), and that the tool's MCP input schema still validates.
- **Plugin ABI** — if `crates/savvagent-plugin*` changed: build and load at least one example plugin
  (`examples/plugin-hello-interactive`, `plugin-hello-provider`, `plugin-hello-static`) and confirm
  it still loads and runs.
- **`savvagent-canvas`/Blitz** — if `crates/savvagent-canvas` changed: confirm it still builds on
  Linux/macOS (the Windows CI exclusion for this crate is expected, not a regression you introduced).
- **CI** — if `.github/workflows/` changed: confirm the workflow parses and the jobs actually ran on
  this PR (`gh run list --repo savvagent/savvagent-cli --branch <branch>`).
- **Packaging/dist config** — if `[workspace.metadata.dist]` or `.github/workflows/release*.yml` /
  `package-linux.yml` changed: `dist plan` locally, and treat the next real release tag as the actual
  verification (the release step below).

A vacuously-satisfied item ("no out-of-band surface touched") is satisfied, not skipped — state it
explicitly.

### Step 16: Release verification

Confirm the release cut in step 12 actually published cleanly:

```bash
gh release view vX.Y.Z --repo savvagent/savvagent-cli
```

Confirm the expected platform archives/installers are attached, plus `.deb`/`.rpm` once
`package-linux.yml` finishes. If any platform artifact is missing or the workflow failed, treat it as
a Phase 5 failure — do not close the loop on a partially-published release.

### Step 17: Close

- **GitHub:** `gh issue close <n> --repo savvagent/savvagent-cli --comment "<summary>"` (the merged
  PR's `Closes #N` may have closed it already — verify with
  `gh issue view <n> --repo savvagent/savvagent-cli --json state`).
- **Ticketless:** no close action — the Phase 6 summary is the close-out.

Close-out summary:

```
Shipped.

PR: <url>
Release: vX.Y.Z — <gh release view URL>
Out-of-band applied: <TUI/provider/tool/plugin/canvas surface, or "none">
Smoke: <one-line outcome>
```

## Phase 6 — Final summary

Output a single concise message:

```
savvagent-development complete.

Source: <issue #> / task brief — <title> — Closed
PR: <url>
Release: vX.Y.Z
Branch: <branch> (deleted, worktree removed)
Spec: docs/superpowers/specs/<slug>-design.md
Plan: docs/superpowers/plans/<slug>.md
Tasks completed: N / N
Commits: <count>
Timeline: <T_impl_start → T_review_start → T_pipeline_start → T_release_start → T_verify_start, or "n/a">

Out-of-band applied: <list, or "none">

Assumptions worth reviewing (from spec + per-task ledger):
- <bullet>
(up to 5)

Minor issues left unaddressed (intentional, low-priority):
- <bullet, or "none">

Final reviewer assessment: <Ready / Needs follow-up — details>
```

Then STOP. Do not pick up the next task. Do not offer to chain another run.

## Stop & Escalate

Stop the pipeline and return control to the developer when ANY of these is true:

1. A task is BLOCKED and re-dispatching with more context did not unblock it after one retry.
2. A task fails spec review three times in a row.
3. A task fails quality review three times in a row (with Critical or Important issues).
4. Test infrastructure is broken in a way that prevents verifying any task.
5. The plan has internal inconsistencies (a later task assumes a structure earlier tasks didn't produce).
6. The pipeline has run for unreasonable wall-clock time and is making no progress.
7. The AC contradicts the spec/plan you built (mid-flight requirements change).
8. An agent review surfaces a security finding (secrets, injection, sandbox-escape, PII) —
   especially one touching the keyring, `tool-bash`, `tool-web`, or the plugin ABI trust boundary.
9. A proposed change would add a provider registry inside `Host`, hold the host-swap `RwLock` across
   an `.await`, drop the `ProgressDispatcher` forwarder-abort pattern, write a secret to disk in
   plaintext, or widen a tool/plugin's reach (filesystem, network, process spawning) without an
   explicit design decision — these are not negotiable design choices.
10. Out-of-band verification (Phase 5 step 15) fails after a green merge.
11. The release (Phase 4 step 12) fails to publish cleanly, or a platform artifact is missing at
    Phase 5 step 16 — the merge is not considered shipped until this is resolved.
12. The same bug pattern is discovered elsewhere — file a follow-up issue; do NOT silently widen scope.
13. An out-of-band prerequisite from another change (an unmerged plan, an unpublished release this
    work depends on) is missing.

On escalation, output:

```
savvagent-development halted at Phase <N> — <step name>.

Reason: <one of the conditions above, with specifics>
Source: <issue/brief>
Branch: <branch>
Worktree: .claude/worktrees/<branch>
PR: <url, if open>
Last successful step: <step name>
Commits so far: <git log --oneline since branch point>
Recommended next step: <suggestion>
```

Then STOP. Do not push, do not open a PR, do not merge, do not close, do not cut a release.

Speed pressure does not eliminate any step. It can require escalation; it never authorizes skipping.

## Calibration vs. Skipping

Within each step you may calibrate effort to risk. You may NEVER eliminate a step.

| Step                                                    | Cheapest valid form for a small change                                                             | Skip?                                                |
| --------------------------------------------------------- | -------------------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| Convention reference                                       | Skim CLAUDE.md + the latest plan + test command                                                    | NEVER                                                  |
| Read source                                                | 20-second skim of description + AC                                                                 | NEVER                                                  |
| Spec draft                                                 | 1-page spec with Assumptions + Approach + Scope (committed)                                          | **Only** if fast-path criteria met                     |
| Spec critique                                              | 1 reviewer dispatch                                                                                 | **Only** when the spec is skipped under fast-path      |
| Plan draft                                                 | Minimal single-task plan (committed)                                                                | NEVER — house style keeps the plan even on fast-path   |
| Plan critique                                              | 1 reviewer dispatch                                                                                 | **Only** when the plan is fast-pathed (trivial task)   |
| Worktree at start                                          | `git worktree add .claude/worktrees/<branch> -b <branch> origin/main`                                | NEVER (never code on main)                             |
| Main-branch writes                                         | reviewed, merged PR (squash) — incl. record-as-shipped and the release-bump PR                       | NEVER (no direct push/hand-merge)                       |
| Implementer dispatch                                       | 1 `task` call (`general-purpose`, sync) with full task text inline                                    | NEVER                                                  |
| Spec compliance review                                     | 1 reviewer dispatch reading actual commits                                                          | NEVER                                                  |
| Quality review                                             | 1 `code-review` `task` dispatch                                                                      | NEVER                                                  |
| Automated reviewer                                         | 1 `gh pr edit --add-reviewer …`                                                                     | Only if the repo has none configured                    |
| Rust expert + architect + independent security review      | 1 parallel `task` dispatch each (`general-purpose` ×2, blind-diff `security-review`)                 | NEVER                                                  |
| Review-response subagent                                   | 1 `general-purpose` `task` dispatch with PR# + ref                                                    | NEVER                                                  |
| Branch + worktree cleanup                                  | `git branch -d` + `git worktree remove`                                                             | NEVER                                                  |
| Record-as-shipped                                          | flip spec Status + tick plan boxes; `docs:` commit                                                  | NEVER                                                  |
| Release cut (#12)                                          | version bump + CHANGELOG entry + tag push, via a small reviewed PR                                   | NEVER (Non-Negotiable Rule 8)                          |
| Out-of-band verification (#15)                             | 30-second check per touched surface                                                                 | NEVER (vacuous is fine)                                |
| Target verification (#14/#16)                              | YOUR merge commit's CI run green, by run ID + `gh release view` on the new tag                        | NEVER                                                  |
| Tracker transitions                                        | 1 call per transition                                                                               | Only on the ticketless path, or if status labels don't exist |

A vacuously-satisfied step ("no out-of-band surface touched") is satisfied, not skipped. State it
explicitly.

## Common Rationalizations (All Are Violations)

| Excuse                                                             | Reality                                                                                                                                          |
| ---------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| "It's a small change, skip the spec/plan"                              | Only skip the _spec_ if ALL fast-path criteria hold. The plan doc is not skippable.                                                                |
| "I'll ask the user mid-run about an ambiguity"                         | Fully autonomous. Make the most reasonable interpretation, document under Assumptions, continue.                                                   |
| "Slack/the PR/the summary IS the spec"                                 | Summaries lose AC. The brief/issue is source of truth. 30 seconds.                                                                                 |
| "cargo test --workspace is green, it's shipped"                        | The TUI's runtime tool-spawn, a plugin example, and the actual release artifacts are not test outputs. Verify them at #15–16.                       |
| "I'll skip the plan doc, the code is self-documenting"                 | The plan repository IS this project's history. Commit the doc.                                                                                     |
| "I'll skip the record-as-shipped commit"                               | The spec Status and the plan's checkboxes are how the docs track shipped state. Close the loop.                                                     |
| "I'll add a provider registry to Host, it's simpler"                   | The host only ever sees `Box<dyn ProviderClient>`. A registry inside the host is the exact anti-pattern the transport split exists to prevent.       |
| "Holding the RwLock across this one await is fine, it's fast"          | That's the exact bug class the host-swap rule exists to prevent — it doesn't matter how fast the await resolves.                                    |
| "I'll skip the ProgressDispatcher abort, the subscriber will just idle" | It deadlocks the caller's mpsc waiter. Abort the forwarder task after the request future resolves, every time.                                       |
| "I'll log the API key for debugging"                                   | Secrets never cross a trust boundary into logs, errors, transcripts, or responses. Keyring only.                                                    |
| "This is a tiny PR, I'll batch the release with the next one"          | Rule 8 has no batching carve-out. Cut the release now, as part of closing out this PR.                                                              |
| "The latest CI run is green, mine will be too"                         | Latest ≠ yours — a teammate's merge seconds after yours steals it. Capture YOUR run ID at merge time and track THAT id.                              |
| "Copilot's comments are auto-generated, safe to ignore"                | Read each. They find real bugs. Reply with fix-or-dismiss reasoning, then resolve the thread.                                                       |
| "The Windows test exclusion for savvagent-canvas is a bug I should fix"| It's a documented, intentional CI carve-out (Blitz font-discovery hang) — not a regression to chase unless the plan specifically targets it.          |
