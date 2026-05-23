# User-defined hooks — design

Date: 2026-05-22
Status: drafted, awaiting user review before plan
Supersedes: nothing
Related:
- `docs/superpowers/specs/2026-05-12-v0.9.0-plugin-system-design.md` — `HookKind` and `HookDispatcher` this design extends
- `docs/superpowers/specs/2026-05-21-user-slash-commands-design.md` — sub-project A; this is sub-project B
- Issue [#92](https://github.com/robhicks/savvagent-rs/issues/92) — separate concern, unaffected

## Context: the multi-subsystem split

| Order | Sub-project | Status |
|-------|-------------|--------|
| A | User slash commands | shipped (PR #94, merged into v0.16.1's successor) |
| **B** | **User-defined hooks** | *this spec* |
| C | Agents (subagents via a built-in `task` MCP tool) | future |
| D | External plugins (WASM, honoring v0.9.0's deferred WIT promise) | future |

Sub-project A introduced markdown-defined slash commands plus the four-path
discovery pattern (`.savvagent/` and `.claude/` × project and user scopes).
Sub-project B reuses that same discovery shape for `settings.json` files
and adds the missing piece needed for agentic safety: **user shell hooks
that can block tool calls before they run**.

## Problem

Today, `HookKind` events fire only to **plugins** registered through the
v0.9.0 manifest pipeline. Users who want to gate file writes, lint a diff
before commit, or audit shell commands have no surface — they must either
fork savvagent or use the model itself as the policy layer (unreliable).

Every comparable agent exposes a shell-hook surface:

- Claude Code: `~/.claude/settings.json` with a `hooks` block.
- OpenCode: `opencode.json` plugins entry.
- Cursor: rules files.

Savvagent will ship the Claude-Code-compatible shape so existing
`.claude/settings.json` hook libraries work unmodified.

## Approach

A new built-in plugin `internal:user-hooks` discovers settings files
under the same four directories sub-project A uses, parses the
`hooks` block, and dispatches matching shell commands when events
fire. For observe-only events (`PostToolUse`, `SessionStart`, etc.) it
runs in the existing `HookDispatcher::on_event` pipeline. For
**`PreToolUse`** — the only event that must gate dispatch synchronously
— a new `PreToolUseGate` trait in `savvagent-host` is consulted by
`ToolRegistry::call_with_bash_net_override` before the tool runs. The
user-hooks plugin implements that trait and registers itself at
startup via a savvagent-internal effect (same pattern providers use).

No `Effect` surface broadens to the WIT-portable side beyond two new
string/content-only variants for prompt rewriting and turn cancellation.

## Section 1 — User-facing surface

### Discovery paths

Mirror sub-project A's four-path discovery. **Hooks merge across all
four files** rather than first-wins:

1. `<project>/.savvagent/settings.json`
2. `<project>/.claude/settings.json`
3. `~/.savvagent/settings.json`
4. `~/.claude/settings.json`

Sequential execution within an event respects this order: project-savvagent
hooks fire first, then project-claude, then user-savvagent, then user-claude.
Each file contributes its own hook list to the combined event index;
duplicates are NOT deduped (a matcher+command pair appearing in two files
fires twice).

"Project root" is the same root the `SAVVAGENT.md` loader resolves to (walk
up from cwd looking for `.git/`, `.savvagent/`, or root).

### Schema

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "tool-fs:write_file",
        "hooks": [
          { "type": "command", "command": "/path/to/check.sh", "timeout": 30 }
        ]
      }
    ],
    "PostToolUse": [...],
    "UserPromptSubmit": [...],
    "SessionStart": [...],
    "Stop": [...]
  }
}
```

Top-level keys other than `hooks` are ignored (forward-compat for future
fields). Each event maps to an array of **matcher groups**; each matcher
group has a `matcher` string and a `hooks` array of **hook commands**.

### Event mapping

Claude Code event name → savvagent internal `HookKind`:

| Claude Code event | `HookKind` | Block-capable | Notes |
|---|---|---|---|
| `PreToolUse` | new pre-dispatch seam (§2) | **yes** | The only synchronously-gating event |
| `PostToolUse` | `ToolCallEnd` | no | Observation only |
| `UserPromptSubmit` | `PromptSubmitted` | yes (suppresses or rewrites the prompt) | Also supports `additionalContext` injection |
| `SessionStart` | `HostStarting` | no | `source` field = `"startup"` in v1 |
| `Stop` | `TurnEnd` | yes (re-prompts the model) | `stop_hook_active` flag prevents loops |

Claude Code events **parsed but never fired** in v1 (reserved): `Notification`,
`SubagentStop` (re-enabled by sub-project C), `PreCompact` (no analog).
Configs that reference them parse cleanly with a single warn-log per file.

### Matcher syntax

Globs over the tool name, evaluated only for tool-related events:

| Pattern | Matches |
|---------|---------|
| `*` | every tool |
| `run` | exact match — the bash `run` tool |
| `tool-fs:*` | every `tool-fs` MCP server tool |
| `*_file` | suffix glob |

Compiled via the `globset` crate (added if not already in workspace).
Invalid patterns warn-log at parse and the matcher group is skipped.
For non-tool events (`UserPromptSubmit`, `SessionStart`, `Stop`), the
`matcher` field is parsed but ignored — those events have no tool name.

### stdin contract (hook → child process)

A JSON object on the hook's stdin. Common fields, every event:

```json
{
  "session_id": "<uuid>",
  "transcript_path": "/home/.../transcripts/<unix>.json",
  "cwd": "/path/to/project",
  "hook_event_name": "PreToolUse"
}
```

Event-specific additions:

| Event | Extra fields |
|---|---|
| `PreToolUse` | `tool_name: string`, `tool_input: object` |
| `PostToolUse` | `tool_name: string`, `tool_input: object`, `tool_response: object` |
| `UserPromptSubmit` | `prompt: string` |
| `SessionStart` | `source: "startup"` |
| `Stop` | `stop_hook_active: bool` |

`session_id` is per-TUI-process; persists across turns within one session.

### Environment

Hooks inherit the TUI's environment, plus:

- `SAVVAGENT_PROJECT_DIR` — same path the stdin's `cwd` carries; convenience for shell scripts that want it as `$SAVVAGENT_PROJECT_DIR`.

No other env knobs in v1.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Proceed; continue chain |
| 2 | **Block** (block-capable events only). Stderr → user-visible reason. Later hooks in the same event are skipped. For `PostToolUse` and `SessionStart` (not block-capable), exit-2 is logged as a non-blocking error |
| other non-zero | Non-blocking error; stderr logged via `tracing::warn`; chain continues |

### Structured JSON stdout (optional)

If a hook prints a JSON object to stdout AND exits 0, that JSON takes
precedence over the exit code for decision-making. Accepted fields in v1:

```json
{
  "continue": false,
  "stopReason": "secrets-check failed: AWS_ACCESS_KEY_ID in args",
  "suppressOutput": true,
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Writes to .git/ are forbidden"
  }
}
```

| Field | Meaning | Events |
|---|---|---|
| `continue: bool` | `false` blocks the chain (same as exit 2). `true` (default) proceeds | all |
| `stopReason: string` | User-visible reason; renders as `[blocked] <stopReason>` PushNote | all |
| `suppressOutput: bool` | Suppress echoing the hook's stdout into the conversation log (default `false`) | all |
| `hookSpecificOutput.hookEventName` | Must match the event firing; mismatch warn-logs and ignores `hookSpecificOutput` | all |
| `hookSpecificOutput.permissionDecision` | `"allow"` \| `"deny"` \| `"ask"` (treated as `"deny"` in v1) | `PreToolUse` only |
| `hookSpecificOutput.permissionDecisionReason` | Reason text for `permissionDecision` | `PreToolUse` only |
| `hookSpecificOutput.additionalContext` | Prepended to user prompt with `\n\n` separator | `UserPromptSubmit` only |
| legacy `decision` / `reason` | Accepted with deprecation warn; semantics match `continue: false` + `stopReason` | all |

Invalid JSON stdout → fall back to exit-code outcome (no decision-by-stdout).

### Per-hook `timeout`

Seconds; default 60. Hooks exceeding it are killed via `tokio::process::Command`'s
kill-on-drop. Timeout produces a non-blocking error with stderr
`hook timed out after Ns`.

### `/reload-hooks`

Rescans all four `settings.json` files and replaces the in-memory hook
index. Built-ins and other plugin contributions are untouched. The
slash command is contributed by the user-hooks plugin's manifest.

## Section 2 — Runtime architecture

### Crate layout

```
crates/savvagent/src/plugin/builtin/user_hooks/
    mod.rs                # Plugin impl + on_event dispatch for observe-only events
    discovery.rs          # walk + merge the four settings.json files
    config.rs             # serde types: HooksConfig, MatcherGroup, HookCommand
    matcher.rs            # globset compile + cached matchers
    payload.rs            # builds stdin JSON per HookKind
    runner.rs             # spawn shell child, write stdin, await with timeout, parse stdout
    decision.rs           # HookDecision = Continue | Block { reason } | Inject { text }
    pre_tool_gate.rs      # impl PreToolUseGate (savvagent-host trait)
    reload.rs             # /reload-hooks slash command handler
```

### Host-side seam for `PreToolUse` blocking

`HookDispatcher` is observe-only (post-fact). For `PreToolUse` we need
synchronous gating before tool dispatch. Add a savvagent-internal trait
to `savvagent-host` (NOT the WIT-portable surface, same pattern as
`BuiltinProviderPlugin` for providers):

```rust
// crates/savvagent-host/src/pre_tool_gate.rs (new)
use async_trait::async_trait;
use serde_json::Value;

#[async_trait]
pub trait PreToolUseGate: Send + Sync {
    async fn check(&self, tool_name: &str, input: &Value) -> PreToolDecision;
}

#[derive(Debug, Clone)]
pub enum PreToolDecision {
    Allow,
    Block(String),
}
```

`Host` holds an `Arc<tokio::sync::RwLock<Option<Arc<dyn PreToolUseGate>>>>`
field. `Host::set_pre_tool_gate(gate: Arc<dyn PreToolUseGate>)` is the
single setter. `ToolRegistry::call_with_bash_net_override` (the only tool
dispatch chokepoint, at `tools.rs:536`) consults the gate before dispatch:

```rust
// in ToolRegistry::call_with_bash_net_override, after arg validation
if let Some(gate) = self.pre_tool_gate.read().await.as_ref() {
    match gate.check(name, &Value::Object(args.clone())).await {
        PreToolDecision::Allow => {}
        PreToolDecision::Block(reason) => {
            return ToolCallOutcome::error(format!("blocked by user hook: {reason}"));
        }
    }
}
```

The existing `ToolCallStart`/`ToolCallEnd` `HookKind` events still fire
when the dispatch proceeds. On block, the `ToolCallEnd` event fires
with `success: false` so `PostToolUse` hooks observe the rejection.

### Plugin → host registration

A new savvagent-internal effect (NOT WIT-portable — it traffics a
`PluginId`, then the runtime resolves the trait object via the
registry):

```rust
// in crates/savvagent-plugin/src/effect.rs
/// Announce that this plugin provides a `PreToolUseGate`. The runtime
/// fetches the gate object via a savvagent-internal seam (not part of
/// the WIT-portable surface).
RegisterPreToolGate {
    /// Plugin id whose `BuiltinHookPlugin::take_pre_tool_gate()` will
    /// be invoked to materialize the gate.
    plugin_id: PluginId,
},
```

`BuiltinHookPlugin` is the savvagent-internal trait the user-hooks
plugin implements (in `crates/savvagent/src/plugin/builtin/`):

```rust
pub trait BuiltinHookPlugin: Plugin {
    fn take_pre_tool_gate(&self) -> Arc<dyn PreToolUseGate>;
}
```

`apply_effects::RegisterPreToolGate` looks up the plugin in the registry,
calls `take_pre_tool_gate`, and calls `host.set_pre_tool_gate(...)`.
Same architecture as `internal:provider-anthropic` → `take_client` →
`register_provider`.

### Dispatch flow for observe-only events

Existing `HookDispatcher` pipeline already routes `HostEvent`s to
subscribed plugins. `internal:user-hooks` subscribes to:

- `ToolCallEnd` → fires `PostToolUse` hooks
- `PromptSubmitted` → fires `UserPromptSubmit` hooks (block-capable)
- `HostStarting` → fires `SessionStart` hooks
- `TurnEnd` → fires `Stop` hooks (block-capable)

For each matching event the plugin's `on_event`:

1. Filters matcher groups (glob match on `tool_name` for tool events; all groups match for non-tool events).
2. Runs the hook commands sequentially (config order).
3. Accumulates a `Vec<Effect>` to return:
   - Stdout/stderr lines as `PushNote` effects (unless `suppressOutput`).
   - On block from `UserPromptSubmit` or `Stop`: emit `Effect::CancelPendingTurn { reason }`.
   - On `additionalContext` from `UserPromptSubmit`: emit `Effect::PrependToPendingPrompt { text }`.

### Two new effects (savvagent-plugin)

```rust
/// Replace the most-recently-submitted user prompt with `text` prepended
/// to its current content. Emitted by `UserPromptSubmit` hooks returning
/// `additionalContext`. The injected text appears BEFORE the user's
/// original prompt with a `\n\n` separator.
PrependToPendingPrompt {
    /// Text to prepend. Empty string is a no-op.
    text: String,
},
/// Abort the turn that's about to start. Emitted by a `UserPromptSubmit`
/// or `Stop` hook that blocked. Renders `reason` as a `[blocked] ...`
/// PushNote in the conversation log; the prompt or stop is not sent.
CancelPendingTurn {
    /// User-visible reason. Empty string falls back to `"blocked by user hook"`.
    reason: String,
},
```

Both are WIT-portable (string-only payloads). Apply arms live in
`crates/savvagent/src/plugin/effects.rs`.

### Settings load at startup

`App::new` calls `user_hooks::discovery::walk_all(project, home)` after
loading the trust file (sub-project A's slot). The resulting
`HooksIndex` is wrapped in `Arc<RwLock<…>>` and shared with the plugin
via constructor — same pattern as `trust_levels`.

### `/reload-hooks` slash command

Clears the cache mutex on the plugin (`Mutex<Option<HooksIndex>>`), runs
discovery again, emits `Effect::ReindexPlugin { id: internal:user-hooks }`
to rebuild the manifest indexes, and pushes a `[info] user-hooks: reloaded`
note. Reuses the `ReindexPlugin` machinery shipped in sub-project A.

### Dependencies

- `globset = "0.4"` (probably already in workspace via `ignore`; verify
  during plan).
- `serde_json` — already used.
- `tokio::process::Command` — already used by `template.rs::run_shell`.
- `async-trait` — already a workspace dep.

No new external deps anticipated.

## Section 3 — Errors, non-goals, versioning

### Error handling matrix

| Failure | Behavior | User-visible |
|---|---|---|
| Malformed `settings.json` (top-level not an object) | Warn-log, skip file at discovery | Startup `[warn]` PushNote |
| `hooks.<UnknownEvent>` (e.g. `Notification`, `SubagentStop`, `PreCompact`) | Warn-log once per file, ignore that block | Startup log line |
| Malformed matcher pattern | Warn-log, skip that matcher group | Startup log line |
| Hook command missing executable | exit-127 from shell; treat as non-blocking error | `[warn] hook /path/missing.sh: spawn failed` |
| Hook timeout | SIGKILL, treated as non-blocking error | `[warn] hook timed out after 60s: <command>` |
| Hook exit 2 on block-capable event | Chain stops; emit block | `[blocked] <stderr>` PushNote + tool-result short-circuit (PreToolUse) or prompt cancelled (UserPromptSubmit/Stop) |
| Hook exit 2 on non-block-capable event | Log-only; chain continues | `[warn] hook exited 2 on non-blocking event` |
| Hook exit non-zero non-2 | Non-blocking error; stderr logged | None |
| Hook stdout is invalid JSON | Fall back to exit-code outcome | None directly |
| `permissionDecision: "ask"` | Treated as `"deny"` in v1 | `[blocked] (ask not supported in v1) <reason>` |
| `hookSpecificOutput.hookEventName` doesn't match firing event | Warn-log, ignore `hookSpecificOutput`, continue with rest | Log line |
| `PreToolUseGate` panics | `catch_unwind`, log, return `Allow` (fail-open so TUI doesn't hang) | Log line; tool dispatch proceeds |
| `/reload-hooks` finds zero settings files | Empty index, succeed silently | None |
| Concurrent reload during in-flight `PreToolUse` | In-flight call uses the old gate snapshot (Arc clone taken before await); next call uses the new one | None — by design |

### Non-goals (v1)

- **Interactive `"ask"` permission** — UI flow deferred. `"ask"` → `"deny"`.
- **Subagent hooks** (`SubagentStop`, `PreCompact`) — reserved names; never fire.
- **`Notification` event** — savvagent has no analog. Reserved.
- **Hook priority / dependency ordering** — sequential in config-merge order is the only model. No `priority` field.
- **Per-hook env overrides** — hooks inherit TUI env plus `SAVVAGENT_PROJECT_DIR`. No knobs.
- **Hot reload via file watch** — `/reload-hooks` only.
- **Trust gate on hooks** — `settings.json` is deliberate user authoring, not project-drop-in content. The trust-gate justification from sub-project A doesn't apply here.
- **Hook from plugin** — plugins already subscribe to `HookKind` natively. Shell hooks are the new surface; a plugin re-exposing them would be redundant.
- **Output size cap on hook stdout/stderr** — bounded by `tokio::process::Command::output`'s buffering. Follow-up if real usage hits memory issues (same caveat applies to `!cmd` in sub-project A).
- **Per-call network or filesystem sandbox** — hooks run with the user's normal shell. The trust boundary is "you authored `settings.json`".

### Versioning + release

Lands as the next minor after v0.16.x (provisionally **v0.17.0**, but per
[[feedback_phase_release_rollup]] the actual tag is decided at merge time
based on what has accumulated since the last real tag).

Same commit must:

- Bump `[workspace.package].version` and mirror into `[workspace.dependencies]`
  per [[feedback_semver]].
- Update README: new "User-defined hooks" section under TUI features; add
  `.savvagent/settings.json` and `~/.savvagent/settings.json` to the
  on-disk paths reference.
- Add CHANGELOG entry per [[feedback_release_notes]].

No tag pushed until release notes ship per [[feedback_release_docs]].

### Test matrix

| Test | Crate | File |
|------|-------|------|
| Discovery walks all four paths; merges hook lists in precedence order | `savvagent` | `user_hooks/discovery.rs` |
| Malformed JSON warn-and-skip; other files still load | `savvagent` | `user_hooks/discovery.rs` |
| Unknown event names (`Notification`, `SubagentStop`) warn once per file, parse rest | `savvagent` | `user_hooks/config.rs` |
| Matcher: `*` / exact / `tool-fs:*` / suffix glob | `savvagent` | `user_hooks/matcher.rs` |
| Matcher: invalid pattern warn-and-skip | `savvagent` | `user_hooks/matcher.rs` |
| Payload round-trip per event type | `savvagent` | `user_hooks/payload.rs` |
| Runner: exit 0 / exit 2 (block) / exit 127 / non-zero non-2 / timeout | `savvagent` | `user_hooks/runner.rs` |
| Runner: structured JSON stdout takes precedence; invalid JSON falls back | `savvagent` | `user_hooks/runner.rs` |
| Runner: `hookEventName` mismatch warn-logs and ignores `hookSpecificOutput` | `savvagent` | `user_hooks/runner.rs` |
| `PreToolUseGate` Allow → tool runs | `savvagent-host` | `pre_tool_gate.rs` |
| `PreToolUseGate` Block → `ToolCallOutcome::error` with reason | `savvagent-host` | `pre_tool_gate.rs` |
| `PreToolUseGate` panic → fail-open with log | `savvagent-host` | `pre_tool_gate.rs` |
| `PreToolUseGate` Block also fires `ToolCallEnd` with `success: false` | `savvagent-host` | session integration |
| `UserPromptSubmit` hook with `additionalContext` → `PrependToPendingPrompt` effect | `savvagent` | `user_hooks/mod.rs` |
| `UserPromptSubmit` hook exit-2 → `CancelPendingTurn` effect | `savvagent` | `user_hooks/mod.rs` |
| `Stop` hook block → `CancelPendingTurn`; next turn does NOT auto-fire | `savvagent` | `user_hooks/mod.rs` |
| `/reload-hooks` replaces index without restart | `savvagent` | `user_hooks/reload.rs` |
| Concurrent `/reload-hooks` during in-flight `PreToolUse` — in-flight uses old gate snapshot | `savvagent` | `user_hooks/reload.rs` |
| End-to-end: `PreToolUse` hook on `tool-fs:write_file` denies a write; tool result is the deny reason | `savvagent` | `tests/user_hooks_e2e.rs` |
| `SubagentStop` parsed cleanly, never fires | `savvagent` | `user_hooks/config.rs` |

Tests touching `HOME` use `HOME_LOCK` + `HomeGuard` per
[[feedback_test_locale_isolation]]. Disk-asserting tests gate to
`#[cfg(unix)]` per the same Windows-isolation limitation sub-project A
documented; the follow-up issue to plumb a test-injectable `home_dir`
override applies equally here.

### Open questions to resolve during plan

1. `globset` may not be a direct dep of `savvagent` even if it's transitively
   present via `ignore`. Plan inspects `crates/savvagent/Cargo.toml` and
   adds the direct dep if needed.
2. `BuiltinHookPlugin` trait — does it belong in
   `plugin/builtin/provider_common.rs` (alongside `BuiltinProviderPlugin`)
   or a new sibling module? Pick during plan based on file size.
3. The `additionalContext` prepend separator (`\n\n`) — confirm this matches
   what Claude Code does. If their convention differs (e.g. a system-role
   message), document the divergence in the README.
4. Hook process kill-on-drop semantics — tokio's `Command::kill_on_drop(true)`
   is the simplest path; verify it cleans up grandchildren correctly on
   Linux/macOS.

### Appendix — example hooks

A user's `~/.claude/settings.json` for a basic safety net:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "tool-fs:write_file",
        "hooks": [
          { "type": "command", "command": "~/bin/savvagent-write-check.sh" }
        ]
      },
      {
        "matcher": "run",
        "hooks": [
          { "type": "command", "command": "~/bin/savvagent-bash-allowlist.sh", "timeout": 5 }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "matcher": "*",
        "hooks": [
          { "type": "command", "command": "~/bin/savvagent-secrets-scan.sh" }
        ]
      }
    ],
    "Stop": [
      {
        "matcher": "*",
        "hooks": [
          { "type": "command", "command": "~/bin/savvagent-pr-checklist.sh" }
        ]
      }
    ]
  }
}
```

Where `savvagent-write-check.sh` might be:

```bash
#!/usr/bin/env bash
input=$(cat)
path=$(echo "$input" | jq -r '.tool_input.path')
case "$path" in
  *.git/*) echo "writes to .git/ forbidden" >&2; exit 2 ;;
  *.env*)  echo "writes to .env files forbidden" >&2; exit 2 ;;
  *)       exit 0 ;;
esac
```
