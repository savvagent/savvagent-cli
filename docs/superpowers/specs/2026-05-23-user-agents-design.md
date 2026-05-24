# User-defined agents — design

Date: 2026-05-23
Status: drafted, awaiting user review before plan
Supersedes: nothing
Related:
- `docs/superpowers/specs/2026-05-12-v0.9.0-plugin-system-design.md` — `HookKind`, `Effect`, and the built-in plugin pattern this design extends
- `docs/superpowers/specs/2026-05-21-user-slash-commands-design.md` — sub-project A; established four-path discovery, frontmatter parsing, `@<path>` inclusion
- `docs/superpowers/specs/2026-05-22-user-hooks-design.md` — sub-project B; established `PreToolUseGate`, `SubagentStop` reservation, hook stdin contract this design extends
- Issue [#92](https://github.com/robhicks/savvagent-rs/issues/92) — separate concern, unaffected

## Context: the multi-subsystem split

| Order | Sub-project | Status |
|-------|-------------|--------|
| A | User slash commands | shipped (PR #94) |
| B | User-defined hooks | shipping (worktree-user-hooks-followups → next minor) |
| **C** | **Agents (subagents via a built-in `task` MCP tool)** | *this spec* |
| D | External plugins (WASM, honoring v0.9.0's deferred WIT promise) | future |

Sub-projects A and B introduced the four-path discovery pattern
(`.savvagent/` and `.claude/` × project and user scopes) and a stdin/exit-code
hook contract compatible with Claude Code's `settings.json`. Sub-project C
reuses that discovery shape for agent definition files and adds the
last missing primitive needed for parity: **a `task` tool the parent model
can call to spawn a focused subagent with a constrained tool set**.

## Problem

Today, every model interaction in savvagent runs against the full active
tool registry with one system prompt. Long, focused tasks ("review this
diff for security issues", "trace the dependency graph for module X")
either pollute the main conversation or have to be hand-rolled by the
user as a separate `/connect` session.

Every comparable agent exposes a subagent surface:

- Claude Code: `.claude/agents/<name>.md` + the built-in `Task` tool with a `subagent_type` enum populated from those files.
- OpenCode: `.opencode/agent/<name>.md` plus an analogous tool.
- Cursor: rules-driven implicit modes.

Savvagent will ship the Claude-Code-compatible shape so existing
`.claude/agents/*.md` libraries work unmodified, and the parent model
can delegate focused work to a subagent that runs its own turn loop
with its own system prompt and a filtered tool set.

## Approach

A new built-in plugin (`internal:user-agents`) discovers `.md` files
under the four standard directories, parses frontmatter into an
in-memory `AgentSpec` index, and registers an **in-process** `task` tool
whose `subagent_type` JSON-schema enum is regenerated from that index.

When the parent model calls `task`, the tool handler builds a `SubHost`
— a value that owns its own session state (messages, system prompt,
model selection, tool filter) but **shares** the parent's `ProviderClient`,
`ToolRegistry`, `PreToolUseGate`, and permissions cache via `Arc`. The
SubHost drives `run_turn_inner` to its own `end_turn`; the final
assistant text is wrapped as a single `ToolResult` content block and
returned to the parent's tool-use loop.

Sub-project B's user hooks see subagent tool calls indistinguishably
from parent calls (via a new optional `subagent` field in the stdin
payload), and the previously-reserved `SubagentStop` event lights up
in this release.

## Section 1 — User-facing surface

### Discovery paths

Mirror sub-projects A and B. First-wins by agent name; project beats
user, savvagent beats claude:

1. `<project>/.savvagent/agents/**/*.md`
2. `<project>/.claude/agents/**/*.md`
3. `~/.savvagent/agents/**/*.md`
4. `~/.claude/agents/**/*.md`

Project root resolves the same way `SAVVAGENT.md` does (walk up from
cwd for `.git/`, `.savvagent/`, or root). Slug = filename without
`.md`, lowercase-kebab-case validated.

### File shape

Claude-Code-compatible frontmatter; body is the subagent's system prompt:

```markdown
---
name: code-reviewer
description: Reviews staged diffs for correctness bugs. Use after writing code, before commit.
tools: tool-fs:read_file, tool-fs:glob, tool-grep:search
model: claude-sonnet-4-6
---

You are a senior code reviewer. When invoked, ...
```

| Key | Type | Required | Notes |
|---|---|---|---|
| `name` | string | optional | Stored on `AgentSpec`; warn-log if it disagrees with the filename slug. Filename slug wins for the `subagent_type` enum |
| `description` | string | **required** | Goes into the `task` tool's `subagent_type` enum description so the parent model knows when to pick this agent |
| `tools` | comma-separated string OR YAML list | optional | Exact tool names. Omitted = inherit parent's full tool set. `tools: []` = nothing but `task` |
| `model` | string | optional | Routed through the same model-override path sub-project A introduced |
| body | markdown | **required** | Replaces (does not append to) the default system prompt for the subagent. `@<path>` includes expanded at load time |

### The `task` tool

A new in-process tool (registered by the user-agents plugin via a new
`Effect::RegisterInProcessTool` variant — see §2) named `task`. Schema:

```json
{
  "name": "task",
  "description": "Spawn a subagent to handle a focused task. Returns the subagent's final response as a single text block.",
  "input_schema": {
    "type": "object",
    "required": ["description", "prompt", "subagent_type"],
    "properties": {
      "description": { "type": "string", "description": "3-5 word task label shown in the TUI" },
      "prompt": { "type": "string", "description": "The task for the subagent" },
      "subagent_type": { "type": "string", "enum": ["<populated from discovered agents>"] }
    }
  }
}
```

If zero agents are discovered, `task` is **not registered at all** — the
parent model never sees it. Whenever the agent index reloads
(`/reload-agents`), the plugin re-emits `RegisterInProcessTool` with a
fresh `subagent_type` enum and the host swaps the registration atomically.

### Trust gate

Agent body files are model system prompts; they don't execute shell on
load. They can encourage the subagent to call shell tools, which is
gated by the existing permissions/hook chain. **No new trust prompt for
v1.** `@<path>` includes are allowed in agent bodies; `!<cmd>` shell
substitution from sub-project A is **not** allowed (bodies are static).

### `/reload-agents`

Rescans all four `agents/` directories and replaces the in-memory
`AgentSpec` index, then re-registers the `task` tool with the new enum.
Contributed by the plugin's manifest, matching A's `/reload-commands`
and B's `/reload-hooks`.

## Section 2 — Runtime architecture (the Sub-Host)

### In-process tool registration

The `task` tool's implementation needs direct access to the parent's
`ProviderClient`, `ToolRegistry`, and `PreToolUseGate` — putting it
behind a stdio MCP boundary would force serialization of host-internal
state. We introduce an **in-process tool** registration path on
`ToolRegistry`:

- New `Effect::RegisterInProcessTool { spec: ToolDef, handler: Arc<dyn InProcessToolHandler> }`
- `ToolRegistry` grows a parallel `HashMap<String, Arc<dyn InProcessToolHandler>>` next to its stdio children map
- `ToolRegistry::call` first checks the in-process map, then the stdio map
- `ToolDef`s from both paths flow through `tool_defs()` so providers see them uniformly

The `InProcessToolHandler` trait surface:

```rust
#[async_trait::async_trait]
pub trait InProcessToolHandler: Send + Sync + 'static {
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: ToolCallContext,
    ) -> Result<ToolResult, HostError>;
}

pub struct ToolCallContext {
    pub host: Arc<Host>,
    pub subagent: Option<SubagentContext>,
    pub cancellation: CancellationToken,
}
```

`SubagentContext` carries the current depth (Some = inside a subagent
turn at this depth; None = parent turn).

### `SubHost` construction

When the user-agents plugin's `task` handler fires:

1. Resolve `subagent_type` against the in-memory `AgentSpec` index.
   Unknown → return `ToolResult { is_error: true, content: "unknown subagent_type: <name>" }`. Parent loop continues.
2. Check depth against `SAVVAGENT_AGENT_MAX_DEPTH` (default 3, configurable).
   Exceeded → `is_error: true` ToolResult; no SubHost constructed.
3. Build a `SubHost`:

| Field | Source |
|---|---|
| Session state (messages, turn id) | Fresh per call; subagent turn IDs namespaced `<parent>.<sub>` |
| System prompt | Agent body with `@<path>` expanded at load time (cached on `AgentSpec`) |
| Model selection | Per-agent `model:` if set, else parent's active model |
| Tool view (the `ToolDef` slice passed to the provider) | Filtered by allowlist (§3) |
| `ProviderClient` | `Arc`-shared with parent — same connection, same creds |
| `ToolRegistry` | `Arc`-shared, wrapped in a `ScopedToolRegistry` per §3 |
| `PreToolUseGate` and hook chain | `Arc`-shared |
| Permissions cache, sandbox config, sensitive paths | `Arc`-shared |
| `CancellationToken` | Child of parent turn's token |

4. Run `SubHost::run_turn_inner(prompt_content, Some(subagent_event_tx))` to its own `end_turn`. Subagent streaming flows through a private channel (§5), not the parent's `mpsc::Sender<StreamEvent>`.
5. The last assistant `ContentBlock::Text` becomes the `task` tool's return: a single `ToolResult` content block. If the SubHost ended without any assistant text, the result is `is_error: true` with `subagent produced no output`.

### Depth and recursion

Subagents may call `task` again (their tool view always includes `task`
subject to depth — see §3). Depth lives on `SubagentContext` threaded
through the in-process tool path; exceeding `SAVVAGENT_AGENT_MAX_DEPTH`
returns `is_error: true` before constructing the next SubHost.
Default 3 (parent → child → grandchild → no further).

### Cancellation

Parent turn already holds a `CancellationToken` (Esc handler). The
SubHost takes a child token via `parent_token.child_token()`. Cancelling
the parent turn cancels all in-flight subagents. Subagent-only
cancellation is **not** v1.

A cancelled subagent returns `is_error: true` with reason `cancelled`;
`SubagentStop` does **not** fire for cancelled turns (it fires only on
clean `end_turn`).

### Workspace layout

- `crates/savvagent-host/src/subhost.rs` *(new)* — `SubHost` struct, `run_subagent` entry point, `SubagentContext`, depth handling
- `crates/savvagent-host/src/tools.rs` *(extension)* — `InProcessToolHandler` trait, in-process registration path, `ToolCallContext`
- `crates/savvagent-plugin/src/effect.rs` *(extension)* — `RegisterInProcessTool` variant
- `crates/savvagent/src/plugin/builtin/user_agents/` *(new)* — discovery, frontmatter, body inclusion, plugin manifest, `task` tool handler, `/reload-agents` command

### Effect surface

One new variant:

```rust
Effect::RegisterInProcessTool {
    spec: ToolDef,
    handler: Arc<dyn InProcessToolHandler>,
}
```

This stays savvagent-internal — it carries a non-portable `Arc<dyn Trait>`
and will **not** be exposed to WASM plugins (sub-project D). The WIT
surface for D explicitly excludes it; the design here is purely an
in-process extension point for built-in plugins.

## Section 3 — Tool scoping enforcement

Two enforcement layers, both required.

### Filter at the provider boundary

When the SubHost builds the `CompleteRequest` for its provider call, the
`tools` field carries only the `ToolDef`s whose names are in the agent's
allowlist. The subagent model never sees disallowed tool definitions.

Matching is **exact-string against the fully-qualified tool name** as it
appears in `ToolRegistry::tool_defs()` (e.g. `tool-fs:read_file`, not
`read_file`). Unknown names in the allowlist warn-log at agent load and
are dropped from the filter.

### Gate at `ToolRegistry::call`

The SubHost wraps the parent's `Arc<ToolRegistry>` in a
`ScopedToolRegistry`:

```rust
pub struct ScopedToolRegistry {
    inner: Arc<ToolRegistry>,
    allowed: Arc<HashSet<String>>,
}
```

`ScopedToolRegistry::call` intercepts: if `name` is not in `allowed`,
returns `ToolResult { is_error: true, content: format!("{name} not available to this subagent") }` **without** delegating.
Defense against a model that fabricates a name from its training data.

This is a per-SubHost wrapper, not a registry mutation — the parent's
view of `ToolRegistry` is untouched.

### Allowlist semantics

| Frontmatter | Subagent's tool view |
|---|---|
| `tools:` key absent | Parent's full tool set + `task` (subject to depth) |
| `tools: []` | Only `task` (subject to depth) — useful for pure-reasoning agents |
| `tools: [tool-fs:read_file, tool-grep:search]` | Just those two names + `task` (subject to depth) |

### `task` and depth

The subagent's tool view always includes `task` — but if depth ≥
`SAVVAGENT_AGENT_MAX_DEPTH`, the `task` ToolDef is **not** included in
the subagent's provider call (so the model doesn't see it as available).
Equivalent runtime gate fires if a model fabricates the name anyway.

No frontmatter knob to disable `task` in v1 — authors who want a
non-recursive subagent can say so in the prompt body.

### Bash

If the agent's allowlist includes `run` (the bash tool name), the
subagent gets bash with the **same** sandbox config and network resolver
the parent uses. Bash is not special-cased.

## Section 4 — Hook interop with sub-project B

Subagents are first-class consumers of the existing hook chain. No
bypasses.

### PreToolUse on subagent tool calls

`ScopedToolRegistry::call` consults the same `PreToolUseGate` chain the
parent uses before delegating to `ToolRegistry::call`. User hooks see
subagent calls indistinguishably from parent calls.

The stdin JSON contract from sub-project B is **extended by one optional
field**:

```json
{
  "session_id": "<uuid>",
  "transcript_path": "/home/.../transcripts/<unix>.json",
  "cwd": "/path/to/project",
  "hook_event_name": "PreToolUse",
  "tool_name": "tool-fs:write_file",
  "tool_input": { ... },
  "subagent": "code-reviewer"
}
```

`subagent` is **absent** for parent-turn calls (full backward compat).
**Present with the agent name** when a SubHost issued the call. Existing
`.claude/settings.json` hooks ignore the new field harmlessly; authors
who want subagent-aware policies can branch on its presence.

### PostToolUse

Same `subagent` field extension. Fires after the SubHost observes the
tool result, before control returns to the subagent's loop.

### SubagentStop

Promoted from reserved-and-never-fired to actually firing. Fires once
per SubHost turn, immediately after `run_turn_inner` reaches `end_turn`
and **before** the result is returned as a `task` ToolResult to the
parent.

stdin payload:

```json
{
  "session_id": "<uuid>",
  "transcript_path": "/home/.../transcripts/<unix>.json",
  "cwd": "/path/to/project",
  "hook_event_name": "SubagentStop",
  "subagent": "code-reviewer",
  "stop_hook_active": false
}
```

`stop_hook_active` works identically to B's `Stop` event: if a
SubagentStop hook returns `{"continue": false, "stopReason": "..."}` and
optionally injects `additionalContext`, the SubHost runs another turn
with `stop_hook_active=true` so a misbehaving hook can't infinite-loop.

`SubagentStop` does **not** fire for cancelled subagent turns.

### Events that do NOT fire for subagents

| Event | Rationale |
|---|---|
| `UserPromptSubmit` | The subagent's "prompt" is synthetic (the parent model generated it). Treating it as a user prompt would mislead hook authors and let user hooks rewrite parent-model content. A `SubagentPromptSubmit` event can land in a follow-up if demand emerges |
| `SessionStart` | Bound to TUI session lifecycle, not subagent execution |
| `TurnStart` / `TurnEnd` | Bound to user-visible turn lifecycle |
| `Stop` | The parent's `Stop` fires when the parent turn reaches end_turn; subagents have their own `SubagentStop` |

### Hook ordering

Project savvagent → project claude → user savvagent → user claude,
identical to B. Matcher syntax (globs against the tool name) is
unchanged. The `subagent` field is **not** part of the matcher syntax in
v1 — authors filter inside their hook script if they care.

## Section 5 — TUI surface

Subagent execution is visible but visually subordinate to the parent turn.

### Conversation log rendering

Each `task` tool call gets a dedicated **collapsible block** in the
parent's conversation log. Collapsed (running):

```
▸ task code-reviewer · "review the auth diff"          [running ⠋]
```

Expanded:

```
▾ task code-reviewer · "review the auth diff"          [4.2s · 3 tool calls]
  │ I'll start by reading the diff…
  │ [tool] tool-fs:read_file src/auth.rs
  │ [tool] tool-grep:search "TODO|FIXME" src/
  │ The migration step at line 42 is missing a NOT NULL backfill…
  │ ────────────────────────────────────────────────
  │ result: 2 issues found, see above
```

Reuses the tool-call summary widgets shipped in v0.10.0
(`tool_fs_summary`, `tool_grep_summary`, etc.) — the user-agents plugin
contributes no new rendering primitives.

### Streaming

The SubHost streams via a private `mpsc::Sender<SubagentStreamEvent>`
whose receiver is the rendering block. `SubagentStreamEvent` variants
mirror `StreamEvent` but carry a `subagent_block_id` so the TUI routes
updates to the right block. They do **not** flow into the parent's
`StreamEvent` channel.

### Default collapse state

- Running = **expanded** (live progress visible)
- Completed = **collapsed** (final one-line summary = subagent's last
  assistant text truncated to ~80 chars)
- User can toggle with click or keypress

### Cancellation in the TUI

Esc while a subagent runs cancels the **whole parent turn** (which
propagates to the subagent via the child cancellation token). No
subagent-only cancel UI in v1.

### Transcript inclusion

Subagent turns persist in the parent transcript JSON as nested objects
under the parent's `task` tool-call entry:

```json
{
  "type": "tool_call",
  "name": "task",
  "input": { "description": "...", "prompt": "...", "subagent_type": "code-reviewer" },
  "subagent_transcript": {
    "agent_name": "code-reviewer",
    "model": "claude-sonnet-4-6",
    "messages": [ ... ],
    "tool_calls": [ ... ]
  },
  "result": { ... }
}
```

`TRANSCRIPT_SCHEMA_VERSION` bumps **1 → 2**. Loading a v2 transcript
with an old binary warn-logs once and renders the parent without the
subagent body (graceful degrade). The current `load_transcript` shape
accommodates this — only a version-tolerant deserializer is needed.

### Headless mode

The headless example (`crates/savvagent-host/examples/headless.rs`) sees
subagent execution as a single `task` tool call that takes time. No
special handling needed.

## Section 6 — Errors, non-goals, versioning, tests

### Error matrix

| Failure | Behavior | User-visible |
|---|---|---|
| Malformed YAML frontmatter | Warn-log, skip file at discovery | Startup log; agent absent from `subagent_type` enum |
| Missing `description:` | Warn-log, skip file | Startup log |
| Unknown frontmatter key | Warn-log per key, keep file | Startup log; agent still works |
| Body empty after `---` | Warn-log, skip file | Startup log |
| Two project agents resolve to same name | Warn-log, keep first by lexicographic path | Startup log |
| Project + user agent same name | Project wins silently (documented precedence) | None |
| `tools:` lists a name not in current `ToolRegistry` | Warn-log per name, drop from filter; `tools: []` (explicitly empty) is treated as intentional | Startup log |
| `model:` names unknown model | Warn-log, fall back to parent's active model | Startup log |
| `@<path>` in body references missing file | Inline literal `@<path>` + warn-log; body still loads | Conversation log warn on first use |
| Parent model calls `task` with unknown `subagent_type` | `task` returns `is_error: true` ToolResult; parent loop continues | Tool error block |
| Subagent exceeds depth cap | `task` returns `is_error: true` before constructing SubHost | Tool error block |
| Subagent's provider call fails | `task` returns `is_error: true` with provider error string | Tool error block |
| Subagent cancelled via parent Esc | `task` returns `is_error: true` with reason `cancelled`; `SubagentStop` does **not** fire | Spinner clears; collapsed block shows "cancelled" |
| SubagentStop hook tries to re-prompt with `stop_hook_active=true` | Hook return ignored, subagent ends normally; warn-log | Log line |
| Discovery dir exists but is unreadable | Warn-log, treat as absent | Startup log |

Discovery never aborts startup. Zero agents discovered → `task` not
registered; parent model never sees the tool. Same fail-safe shape as A
and B.

### Non-goals (v1)

- **Tab-completion of agent names.** Parent model picks via the `subagent_type` enum the provider sees; no human-facing palette.
- **Per-agent provider override.** Frontmatter `provider:` parsed-but-ignored with warn-log; reserved for a future minor. All subagents run against the parent's active provider.
- **Subagent-only cancellation.** Esc cancels the whole parent turn; no UI to cancel just one of many in-flight subagents.
- **Parallel subagent fan-out.** The parent's tool-use loop is sequential — multiple `task` calls in a single round run one at a time. Intra-subagent parallelism is whatever the subagent's provider does with its own tool calls.
- **`!<cmd>` shell substitution in agent bodies.** Bodies are static prompts; only `@<path>` includes (resolved at load time) are expanded.
- **Hot file-watch reload.** `/reload-agents` only — same rationale as A and B.
- **Built-in curated agents.** User-defined only in v1.
- **`SubagentPromptSubmit` hook event.** The synthetic prompt the parent passes to `task` is not surfaced as a hook event.
- **Per-agent token-budget separate from parent.** Subagent's provider call uses the same context window the parent does.
- **Multi-turn user-visible subagent conversations.** A subagent runs to its own `end_turn` and that's the result; user can't directly chat with a running subagent.
- **WIT-portable subagent surface.** `RegisterInProcessTool` is savvagent-internal; sub-project D (WASM plugins) will define its own portable agent shape if needed.

### Dependencies

No new external crates. Reuses workspace-already-present:

- `serde_yaml_ng = "0.10"` — frontmatter parse (already used by A and B)
- `ignore = "0.4"` — directory walks (already used by `tool-fs` and `tool-grep`)
- `serde_json` — transcript v2 round-trip
- `tokio-util` — `CancellationToken::child_token()` (already in workspace)

### Versioning and release

Lands on the next minor after sub-project B. Per
[[feedback_phase_release_rollup]] the actual tag is decided at merge
time, but provisionally **v0.17.0**.

Same commit must:

- Bump `[workspace.package].version` and mirror into `[workspace.dependencies]` literals per [[feedback_semver]]
- Update README: new "User-defined agents" section under TUI features; add `.savvagent/agents/` to the on-disk paths reference; document the `task` tool in the tool list
- Update PRD: add a bullet to §3 Goals for the agent surface; add a paragraph to §4 Non-goals naming the v1 boundaries (per-agent provider, parallel fan-out, etc.)
- Add CHANGELOG entry per [[feedback_release_notes]]
- Bump `TRANSCRIPT_SCHEMA_VERSION` from 1 → 2

No tag pushed until release notes drafted per
[[feedback_release_docs]]; cargo-dist owns release publish on tag push
per [[feedback_cargo_dist_release]].

### Test matrix

| Test | Crate | File |
|------|-------|------|
| Discovery walks four paths with correct precedence | `savvagent` | `plugin/builtin/user_agents/discovery.rs` `#[cfg(test)]` |
| Discovery skips files with malformed YAML, missing description, empty body | `savvagent` | same |
| Frontmatter: `tools:` string vs YAML-list parsing; `tools: []` intent preserved | `savvagent` | `frontmatter.rs` |
| `@<path>` expansion at load time, missing-file warning | `savvagent` | `body.rs` |
| `task` tool not registered when zero agents discovered | `savvagent` | `plugin.rs` |
| `task` tool's `subagent_type` enum updates on `/reload-agents` | `savvagent` | `plugin.rs` |
| `Effect::RegisterInProcessTool` round-trip through plugin runtime | `savvagent` | `plugin/runtime` test |
| `SubHost` runs to `end_turn` and returns final assistant text | `savvagent-host` | `subhost.rs` |
| `SubHost` honors `tools: []` (only `task` available, subject to depth) | `savvagent-host` | same |
| `SubHost` honors `tools:` absent (inherits parent's full tool set) | `savvagent-host` | same |
| `ScopedToolRegistry` rejects out-of-allowlist names at runtime | `savvagent-host` | `tools.rs` |
| Depth cap aborts at configured limit; `SAVVAGENT_AGENT_MAX_DEPTH` env honored | `savvagent-host` | `subhost.rs` |
| Parent cancellation propagates to subagent via child token | `savvagent-host` | same |
| Cancelled subagent does NOT fire `SubagentStop` | `savvagent-host` | same |
| `subagent` field present in PreToolUse stdin during subagent tool call | `savvagent` | `plugin/builtin/user_hooks/` integration test |
| `subagent` field absent in parent-turn PreToolUse stdin (backward compat) | `savvagent` | same |
| `SubagentStop` fires after SubHost end_turn, before `task` result returns | `savvagent` | same |
| `SubagentStop` `stop_hook_active` loop guard | `savvagent` | same |
| `UserPromptSubmit` does NOT fire for subagent prompts | `savvagent` | same |
| Transcript v2 round-trip with nested subagent transcript | `savvagent-host` | `session.rs` |
| Transcript v1 still loads on v0.17.0 binary (with warn-log) | `savvagent-host` | same |

End-to-end smoke (integration test, runs against a stub provider):
parent model emits a `task` call → SubHost spawns → subagent emits a
tool call → PreToolUse hook fires with `subagent` field → subagent
ends → SubagentStop hook fires → result returned to parent → parent
ends.
