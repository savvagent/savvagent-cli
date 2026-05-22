# User-defined slash commands — design

Date: 2026-05-21
Status: drafted, awaiting user review before plan
Supersedes: nothing
Related:
- `docs/superpowers/specs/2026-05-12-v0.9.0-plugin-system-design.md` — built-in plugin pattern this design slots into
- Issue [#92](https://github.com/robhicks/savvagent-rs/issues/92) — Servo-backed HTML rendering of model output (separate concern; this spec keeps user-authored definitions in markdown)

## Context: the multi-subsystem split

The "Claude-Code / OpenCode parity" ask covers four subsystems. We are
brainstorming and shipping them one at a time, each as its own version
bump and release notes per [[feedback_phase_release_rollup]]:

| Order | Sub-project | Status |
|-------|-------------|--------|
| **A** | **User slash commands** | *this spec* |
| B     | User-defined hooks (shell, Claude-Code-style stdin-JSON contract) | future |
| C     | Agents (subagents via a built-in `task` MCP tool) | future |
| D     | External plugins (WASM, honoring v0.9.0's deferred WIT promise) | future |

This decomposition was agreed during brainstorming. Plugins and built-in
slash commands already ship in v0.9.0 (`savvagent-plugin` crate, `SlashSpec`
registration through plugin manifests); A is purely about letting **users**
add their own slash commands from files on disk.

## Problem

`savvagent`'s slash commands today are statically registered by built-in
plugins. There is no on-disk extension point — adding `/review` for your
project means editing the source. Every comparable agent (Claude Code,
OpenCode, Continue, Cursor rules) lets users drop markdown files into a
known directory and have them appear as slash commands. We want the same,
plus drop-in compatibility with existing `.claude/commands/*.md` libraries
so users can reuse what they already have.

## Approach

A single new built-in plugin (`internal:user-slash-commands`) discovers
markdown files under four well-known directories, parses optional YAML
frontmatter, and registers each as a `SlashSpec` through the existing
v0.9.0 manifest pipeline. On dispatch, the plugin expands templating
tokens in the body (arguments, file inclusion, shell-output substitution)
and emits a new `Effect::SubmitPrompt` that synthesizes a user prompt for
the active host.

No `Host` changes. No `ToolRegistry` changes. One new `Effect` variant.
One new built-in plugin. One new modal `Screen` for project trust.

## Section 1 — User-facing surface

### Discovery paths

Scanned in this order; first hit per command name wins. Within a single
scope (project or user) `.savvagent/` outranks `.claude/`. Across scopes,
project outranks user.

1. `<project>/.savvagent/commands/**/*.md`
2. `<project>/.claude/commands/**/*.md`
3. `~/.savvagent/commands/**/*.md`
4. `~/.claude/commands/**/*.md`

"Project root" is the same root the existing `SAVVAGENT.md` loader
resolves: walk up from cwd until we find `.git/`, `.savvagent/`, or root.
If none of the four directories exists, the plugin contributes zero
commands and adds zero overhead.

Subdirectories become namespaces: `commands/team/lint.md` → `/team:lint`.
Nesting deeper than one level is flattened into `:`-joined segments
(`commands/team/security/audit.md` → `/team:security:audit`). Filenames
must match `[a-z0-9][-a-z0-9_]*` after the namespace; mismatches are
warn-logged and skipped.

### Frontmatter (YAML, all fields optional)

```yaml
---
description: One-line palette summary
argument-hint: <file> [branch]      # placeholder rendered in the palette
allowed-tools:                      # parsed, NOT enforced in v1 (see §non-goals)
  - read_file
  - "Bash(git diff:*)"
model: claude-sonnet-4-6            # optional one-turn provider/model override
---
```

| Field | Type | Behavior |
|---|---|---|
| `description` | string | Shown in `/` palette next to the command name. Defaults to the file path relative to its containing `commands/` root. |
| `argument-hint` | string | Rendered after the command name in the palette as a placeholder. Display only; no validation. |
| `allowed-tools` | list of strings | Parsed and stored on the `SlashSpec`. Not enforced in v1. Reserved for sub-project C (agents), which needs the same primitive. Documented in the README as forthcoming. |
| `model` | string | One-turn model override. Looked up against the active provider's `list_models()` on dispatch; unknown id → warn-log, ignore override, proceed with active model. |

Unknown keys: warn-log per key, keep the file. Missing frontmatter entirely:
the file is still loadable; treat as `description = <relative path>`.

### Body templating

Applied in a single non-recursive pass before the rendered text is
emitted as a synthetic prompt. Tokens, in order of expansion:

| Token | Behavior |
|---|---|
| `$ARGUMENTS` | Replaced with the raw argument string (everything after the command name and whitespace). |
| `$1`, `$2`, … | Whitespace-split positional args. Out-of-range positions expand to the empty string. |
| `!<cmd>` on its own line, or inline `!<code>cmd</code>` | Stdout of the shell command. Subject to the project trust check (see §trust). Non-zero exit aborts dispatch and surfaces stderr in the conversation log. |
| `@<path>` | File contents pasted inline. Missing file → literal `@<path>` plus a warning line in the conversation log (matches Claude Code's behavior). |

Single-pass means a `@`-included file containing `!` is **not**
re-expanded. This keeps the trust scope tractable and the behavior
predictable.

### Trust model

The first time we encounter project-local commands in a new project, the
TUI shows a modal:

```
This project ships commands under .savvagent/commands/ and .claude/commands/.
Some of them may run shell commands. Trust this project?

  [y] Trust always
  [n] Block shell, allow text-only this session
  [q] Cancel
```

Decisions persist at `~/.savvagent/trusted-projects.json`:

```json
{
  "projects": {
    "/home/alice/work/savvagent-rs": "always"
  }
}
```

Schema:

- Keys are canonical absolute project root paths (the same root the
  `SAVVAGENT.md` loader resolves to).
- Values are `"always"` only. `"session-text-only"` exists at runtime
  but is **not** persisted — next launch re-prompts.
- A `"cancel"` decision is not stored at all.

Re-prompting when a project's command files change hash is deferred to a
follow-up; v1 keys solely by path. The README will document this
limitation under the security section.

### Reload

`/reload-commands` rescans all four directories and replaces the existing
user-defined `SlashSpec`s in place. Built-ins and contributions from
other plugins are not touched. Hot file-watch reload is non-goal for v1
(see §non-goals).

## Section 2 — Runtime architecture

### Crate layout (new module under existing `savvagent` binary crate)

```
crates/savvagent/src/plugin/builtin/user_slash_commands/
    mod.rs              # Plugin impl: manifest(), handle_slash(), on_event(HostStarting)
    discovery.rs        # walk the four paths via `ignore::WalkBuilder`, return Vec<DiscoveredCommand>
    frontmatter.rs      # YAML parse via serde_yaml_ng (workspace dep)
    template.rs         # $ARGUMENTS / $N / ! / @ expansion, returns Result<String, ExpandError>
    trust.rs            # load/save ~/.savvagent/trusted-projects.json
    trust_modal.rs      # Screen impl for the y/n/q prompt
    tests/              # unit + integration tests
```

`internal:user-slash-commands` is added to the static built-in plugin
list in `crates/savvagent/src/plugin/builtin/mod.rs` alongside the
existing internal plugins.

### Startup discovery

`Plugin::on_event(HostEvent::HostStarting)` triggers discovery:

1. Resolve project root (same logic as `SAVVAGENT.md` loader).
2. For each of the four directories that exists, walk it via
   `ignore::WalkBuilder` with default git-ignore behavior disabled
   (commands directories should not be `.gitignore`-pruned).
3. For each `*.md` file:
   - Parse optional YAML frontmatter (between the first `---` and the
     second `---`). Malformed frontmatter → warn-log, skip file.
   - Validate the namespaced command name (slug regex above). Invalid →
     warn-log, skip file.
   - Build a `DiscoveredCommand { name, path, frontmatter }` and add
     it to the in-memory index keyed by name, respecting the
     first-wins precedence rules.
4. Return a `Vec<Effect>` containing `Effect::ContributeSlashCommands`
   (new effect — see below) with the resulting `SlashSpec` list.

Discovery failures never abort startup. At worst a project's user
commands silently disappear; the log line says why.

### New effect: `SubmitPrompt`

```rust
/// Submit a synthetic user prompt to the active host for the next turn.
/// Used by user-defined slash commands; equivalent to the user typing
/// the rendered string and pressing Enter.
SubmitPrompt {
    text: String,
    /// Optional one-turn model override (parsed from frontmatter `model:`).
    /// Ignored if the active provider doesn't expose that model id.
    model_override: Option<String>,
},
```

`apply_effects` routes `SubmitPrompt` through the same code path as a
manual prompt submission so transcripts, hooks, and footer state remain
consistent. `model_override` is one-turn: stash on
`App.next_turn_model_override`, consumed and cleared in the worker
spawn.

**Security note (documented in the spec, not enforced via permissions):**
once `SubmitPrompt` exists, any plugin can synthesize prompts on the
user's behalf. This is unavoidable for the feature to work and matches
how Claude Code's slash commands behave. Built-in plugins are reviewed in
this repo; external plugins (sub-project D) will need a permission gate
on this effect when they arrive.

### Optional new effect for discovery: `ContributeSlashCommands`

If the v0.9.0 manifest pipeline already supports late contributions
(check during plan), we use it. Otherwise add:

```rust
/// Replace this plugin's contributed slash commands with the given set.
/// Used by user-defined slash commands at startup and after /reload-commands.
ContributeSlashCommands {
    plugin_id: PluginId,
    commands: Vec<SlashSpec>,
},
```

The plan step covers determining which path the v0.9.0 surface supports.
The design works either way; this effect is the fallback.

### Dispatch path

```
user types /team:lint foo.rs
  → slash dispatcher routes to plugin internal:user-slash-commands
  → Plugin::handle_slash("/team:lint", args=["foo.rs"])
      → resolve path from in-memory command index
      → trust check:
          - body contains no `!<cmd>` token → proceed
          - body contains `!<cmd>` AND project is untrusted →
              stash (cmd_name, args) on App.pending_slash_after_trust
              return Effect::OpenScreen(trust_modal)
      → template::expand(body, args, trust_level)
      → returns Vec<Effect> = [
            Effect::SubmitPrompt { text: rendered, model_override },
            // plus any warning lines from @-missing-file as Effect::PushTranscriptNote
        ]
```

### Trust modal

Standalone `Screen` impl, pushed via `Effect::OpenScreen`. Returns one of
three results via a new `Effect::SetTrustLevel` variant:

```rust
SetTrustLevel {
    project_root: PathBuf,
    level: TrustLevel,  // Always | SessionTextOnly | Cancelled
},
```

After `SetTrustLevel`, the plugin's `on_event(SetTrustLevelApplied)`
re-runs the original `handle_slash` (or aborts on `Cancelled`). State to
make this work lives on `App.pending_slash_after_trust:
Option<(String, Vec<String>)>`, set by the plugin before opening the
modal and cleared by `apply_effects` after the re-dispatch.

### Dependencies

No new external crates. Reuses workspace-already-present:

- `serde_yaml_ng = "0.10"` — frontmatter parse.
- `ignore = "0.4"` — directory walks (already used by `tool-fs` and `tool-grep`).
- `serde_json` — trusted-projects.json round-trip.

## Section 3 — Errors, non-goals, versioning

### Error handling matrix

| Failure | Behavior | User-visible |
|---|---|---|
| Malformed YAML frontmatter | Warn-log, skip file at discovery | Startup log line; command absent from `/` palette |
| Frontmatter present but unknown keys | Warn-log per key, keep the file | Startup log line; command still works |
| Two project commands resolve to same name | Warn-log, keep first by lexicographic path | Startup log line |
| Project + user define same name | Project wins silently (documented precedence) | None |
| `@<path>` references nonexistent file | Inline literal `@<path>` + warning line in conversation log | `[warn] @missing/file.rs: file not found` above the rendered prompt |
| `!<cmd>` exits nonzero | Abort dispatch; show stderr in conversation log; do not submit prompt | `[error] !git status: exited 128 — fatal: not a git repo` |
| `!<cmd>` runs in untrusted project | Trust modal pops; on cancel, dispatch aborted; on session-text-only, `!<cmd>` aborts dispatch with a "shell substitution disabled" message | Modal then either prompt-submitted or error line |
| Trust file unreadable/malformed | Warn-log, treat all projects as untrusted | Modal on first command-with-shell |
| `model:` override names unknown model | Warn-log, ignore override, run with active model | Log line in conversation log |
| Discovery directory exists but is unreadable | Warn-log, treat as absent | Startup log line |

Discovery never aborts startup. Worst case the user sees a startup
log line and their command isn't in the palette.

### Non-goals (v1)

- **Tab-completion** of command names or arguments. The palette already
  fuzzy-matches command names; that is sufficient for v1.
- **Enforcing `allowed-tools`.** Parsed and stored on the `SlashSpec` for
  forward compatibility; not honored. The primitive lands with
  sub-project C (agents) which needs per-call `ToolRegistry` scoping.
- **Hot file-watch reload.** `/reload-commands` only. `notify` adds
  cross-platform complexity (Windows FS events are flaky) for marginal
  benefit.
- **Re-prompting trust when command files change.** Trust is keyed by
  project root path only in v1. A `.savvagent/commands/install.md`
  added after the user trusts the project does **not** re-prompt. This
  is documented in the README's security section.
- **Recursive template expansion.** Single-pass only. A `@`-included
  file containing `!` is not re-expanded.
- **Per-command sandbox profiles.** `!<cmd>` runs with the user's normal
  shell and full env. The trust prompt is the only gate. Sandbox
  integration is a follow-up if anyone asks.
- **Argument validation against `argument-hint`.** Display only.
- **HTML or any non-markdown authoring format.** See [#92](https://github.com/robhicks/savvagent-rs/issues/92) for the related
  but separate concern of *rendering model output* via Servo — that is
  about model→human artifacts, not human-authored definition files.

### Versioning and release

Lands on the next minor after the v0.15.0 multi-provider rollup
(provisionally **v0.16.0**, but per [[feedback_phase_release_rollup]] the
actual tag is decided at merge time based on what has accumulated since
the last real tag).

Same commit must:

- Bump `[workspace.package].version` and mirror into
  `[workspace.dependencies]` literals per [[feedback_semver]].
- Update README: new "User-defined slash commands" section under the
  TUI features; add `.savvagent/commands/` and
  `~/.savvagent/trusted-projects.json` to the "On-disk paths" reference.
- Add CHANGELOG entry per [[feedback_release_notes]].

No tag is pushed until the release notes are drafted per
[[feedback_release_docs]].

### Test matrix

| Test | Crate | File |
|------|-------|------|
| Discovery walks all four paths with correct precedence | `savvagent` | `plugin/builtin/user_slash_commands/discovery.rs` `#[cfg(test)]` |
| Discovery skips files with invalid slugs after warn-log | `savvagent` | same |
| Discovery handles nonexistent / unreadable directories | `savvagent` | same |
| Frontmatter: present / absent / malformed / unknown-keys / unicode in description | `savvagent` | `frontmatter.rs` |
| Template `$ARGUMENTS` / `$N` substitution including out-of-range | `savvagent` | `template.rs` |
| Template `@<path>` for existing + missing files | `savvagent` | `template.rs` |
| Template `!<cmd>` for exit-0 + exit-nonzero | `savvagent` | `template.rs` |
| Template expansion is single-pass (no recursion) | `savvagent` | `template.rs` |
| Trust file round-trip; load when file absent | `savvagent` | `trust.rs` |
| Untrusted project + body-with-shell triggers modal effect | `savvagent` | `mod.rs` integration |
| Trusted project + body-with-shell dispatches without modal | `savvagent` | same |
| Cancelled trust aborts dispatch (no `SubmitPrompt` emitted) | `savvagent` | same |
| End-to-end: discovery → handle_slash → expected `Effect` sequence | `savvagent` | `mod.rs` |
| `model:` unknown id falls back with warn | `savvagent` | `mod.rs` |
| `/reload-commands` replaces user commands without touching built-ins | `savvagent` | `mod.rs` |

Tests that touch `set_locale` or `HOME` must honor
[[feedback_test_locale_isolation]] — reset locale to `"en"` inside the
`HOME_LOCK` mutex, or sibling tests asserting on English text will fail
under parallel execution and poison the mutex.

### Open questions to resolve during plan

1. Does the v0.9.0 manifest pipeline already support runtime
   re-contribution by a plugin? If yes, we use it; if no, ship
   `Effect::ContributeSlashCommands`. The plan step inspects
   `crates/savvagent/src/plugin/manifests.rs` to determine which.
2. Should `argument-hint` be passed through to the palette's existing
   description column, or rendered as a separate visual element? The
   plan inspects the palette's `SlashSpec` consumption to pick the
   smaller diff.
3. Trust-modal keybindings (`y`/`n`/`q`) vs. arrow-key selection — pick
   during implementation based on the existing modal patterns
   (theme-picker uses arrow keys; confirm vs. follow that convention).

These are implementation-detail questions; none of them changes the
design contract. They are recorded here so the plan step does not
re-litigate the design.

## Appendix — example commands

A user's `.savvagent/commands/review.md`:

```markdown
---
description: Review the current diff
argument-hint: [optional commit range]
---

Please review the following diff and flag any issues:

!git diff $ARGUMENTS
```

A user's `.savvagent/commands/explain.md`:

```markdown
---
description: Explain a file
argument-hint: <file>
model: claude-sonnet-4-6
---

Explain what @$1 does, including its public API and any non-obvious
invariants. Be concise.
```

A namespaced team command at `.savvagent/commands/team/security.md`
becomes `/team:security`.
