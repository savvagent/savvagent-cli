# User-defined agents Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Claude-Code-compatible user-defined agents to savvagent — markdown files under `.savvagent/agents/` and `.claude/agents/` get discovered into an in-memory index; the parent model gets a `task` tool whose `subagent_type` enum is populated from that index; each `task` call constructs a Sub-Host that runs its own turn loop with a filtered tool set, returning the final assistant text back to the parent.

**Architecture:** New built-in plugin `internal:user-agents` discovers agent definition files via the same four-path scheme as sub-projects A and B. A new in-process tool registration path on `ToolRegistry` (Effect: `RegisterInProcessTool`) lets the plugin contribute a `task` handler that, when invoked, builds a `SubHost` — a value owning its own session state but sharing the parent's `ProviderClient`, `ToolRegistry`, `PreToolUseGate`, permissions, and sandbox via `Arc`. Tool scoping is enforced two ways: at the provider boundary (filtered `ToolDef` list) and at runtime (`ScopedToolRegistry` wrapper). Sub-project B's hooks gain an optional `subagent` field in their stdin payload, and the previously-reserved `SubagentStop` event lights up here. Transcript schema bumps 1 → 2 to embed nested subagent transcripts under the parent's `task` tool-call entry.

**Tech Stack:** Rust (edition 2024), Tokio, `serde_yaml_ng`, `ignore`, `tokio-util` (CancellationToken), `async_trait`, `rmcp` (transitively). No new external crates.

**Dependency:** This plan assumes sub-project B (user-defined hooks) has merged. B contributes `PreToolUseGate` (re-exported from `savvagent-host`), the `internal:user-hooks` plugin, `Effect::RegisterPreToolGate`, and the `HookContext` payload builder. If B is not yet on master at execution time, rebase this branch onto B's branch before starting.

**Spec:** `docs/superpowers/specs/2026-05-23-user-agents-design.md`. Defer to the spec for any clarification the plan doesn't cover.

---

## File structure (locked-in decomposition)

**New files:**

| Path | Responsibility |
|---|---|
| `crates/savvagent-host/src/subhost.rs` | `SubHost` struct, `SubagentContext`, depth handling, cancellation child-token wiring, subagent turn loop entry point |
| `crates/savvagent-host/src/scoped_registry.rs` | `ScopedToolRegistry` wrapper that filters tool calls against an allowlist |
| `crates/savvagent/src/plugin/builtin/user_agents/mod.rs` | Plugin manifest, `Plugin` impl, `/reload-agents` wiring, `Effect::RegisterInProcessTool` emission |
| `crates/savvagent/src/plugin/builtin/user_agents/spec.rs` | `AgentSpec` type — the parsed, in-memory representation of one agent file |
| `crates/savvagent/src/plugin/builtin/user_agents/frontmatter.rs` | YAML frontmatter parser: `name`, `description`, `tools` (string or list), `model` |
| `crates/savvagent/src/plugin/builtin/user_agents/body.rs` | `@<path>` include expansion at load time |
| `crates/savvagent/src/plugin/builtin/user_agents/discovery.rs` | Four-path walk, slug extraction, precedence dedup |
| `crates/savvagent/src/plugin/builtin/user_agents/task_tool.rs` | `TaskToolHandler` — `InProcessToolHandler` impl that resolves `subagent_type`, builds a `SubHost`, drives it, returns the final text |
| `crates/savvagent/src/plugin/builtin/user_agents/index.rs` | `AgentIndex` — `Arc<RwLock<HashMap<String, Arc<AgentSpec>>>>` shared between the plugin and the task handler |

**Modified files:**

| Path | Change |
|---|---|
| `crates/savvagent-plugin/src/event.rs` | Add `HookKind::SubagentStop` variant and `HostEvent::SubagentStop { agent_name, success }` variant |
| `crates/savvagent-plugin/src/effect.rs` | Add `Effect::RegisterInProcessTool { spec, handler }` variant |
| `crates/savvagent-host/src/tools.rs` | Add `InProcessToolHandler` trait, `ToolCallContext`, `SubagentContext` types; add in-process handler `HashMap` to `ToolRegistry`; update `call_with_bash_net_override` to check the in-process map first; add `register_in_process_tool` method |
| `crates/savvagent-host/src/lib.rs` | Re-export new types (`SubHost`, `InProcessToolHandler`, `ToolCallContext`, `SubagentContext`, `ScopedToolRegistry`) |
| `crates/savvagent-host/src/session.rs` | Bump `TRANSCRIPT_SCHEMA_VERSION` from 1 to 2; add nested `subagent_transcript` to tool-call serialization; version-tolerant deserializer that loads v1 with warn-log |
| `crates/savvagent/src/plugin/builtin/mod.rs` | Register `user_agents` plugin alongside the others |
| `crates/savvagent/src/plugin/builtin/user_hooks/payload.rs` | Accept optional `subagent: Option<&str>` in `pre_tool_use`, `post_tool_use`; add `subagent_stop` builder |
| `crates/savvagent/src/plugin/builtin/user_hooks/discovery.rs` | Add `HookEvent::SubagentStop` variant |
| `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs` | Subscribe to `HookKind::SubagentStop`; route to existing dispatch with `subagent_stop` payload |
| `crates/savvagent/src/app.rs` | Handle `Effect::RegisterInProcessTool` by calling `Host::register_in_process_tool` |
| `crates/savvagent/src/tui.rs` (or split into a new widget module) | Render `task` tool-call entries as collapsible blocks; receive `SubagentStreamEvent` updates routed by `subagent_block_id` |
| `README.md` | New "User-defined agents" section under TUI features; `.savvagent/agents/` in on-disk paths; `task` tool in tool list |
| `PRD.md` | Add agent surface bullet to §3 Goals; add v1 non-goals paragraph to §4 |
| `CHANGELOG.md` (top of `[Unreleased]` or new version section) | Entry describing the feature |
| `Cargo.toml` (workspace root) | Bump `[workspace.package].version` to the next minor (provisionally `0.17.0`); mirror into `[workspace.dependencies]` literals |

---

## Phase 1 — Foundation (Effect surface, hook kinds, tool registry primitives)

### Task 1: Add `HookKind::SubagentStop` and `HostEvent::SubagentStop`

**Files:**
- Modify: `crates/savvagent-plugin/src/event.rs`

- [ ] **Step 1: Write the failing test**

Open `crates/savvagent-plugin/src/event.rs` and append to the `#[cfg(test)] mod tests` block at the bottom of the file:

```rust
#[test]
fn subagent_stop_kind_round_trip() {
    let e = HostEvent::SubagentStop {
        agent_name: "code-reviewer".into(),
        success: true,
    };
    assert_eq!(e.kind(), HookKind::SubagentStop);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savvagent-plugin subagent_stop_kind_round_trip`
Expected: FAIL with `no variant or associated item named SubagentStop`.

- [ ] **Step 3: Add the `HookKind` variant**

Add to the `HookKind` enum (after `ActiveProviderChanged`):

```rust
    /// Emitted once per subagent (Sub-Host) turn after it reaches
    /// `end_turn`, before the result is returned to the parent as a
    /// `task` ToolResult. Does NOT fire for cancelled subagent turns.
    SubagentStop,
```

- [ ] **Step 4: Add the `HostEvent` variant**

Add to the `HostEvent` enum (next to other event variants):

```rust
    /// Fired when a Sub-Host (subagent) reaches `end_turn`. The result
    /// has not yet been returned to the parent's `task` tool call.
    /// Not fired on cancelled subagent turns.
    SubagentStop {
        /// The agent name (slug) that just finished.
        agent_name: String,
        /// Whether the subagent's turn ended cleanly.
        success: bool,
    },
```

- [ ] **Step 5: Wire the `kind()` arm**

In `impl HostEvent`'s `kind()` match, add:

```rust
            HostEvent::SubagentStop { .. } => HookKind::SubagentStop,
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p savvagent-plugin subagent_stop_kind_round_trip`
Expected: PASS.

- [ ] **Step 7: Run the full crate tests**

Run: `cargo test -p savvagent-plugin`
Expected: All pre-existing tests still PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent-plugin/src/event.rs
git commit -m "feat(plugin): HookKind::SubagentStop + HostEvent::SubagentStop"
```

---

### Task 2: Add `Effect::RegisterInProcessTool` variant

**Files:**
- Modify: `crates/savvagent-plugin/src/effect.rs`
- Modify: `crates/savvagent-plugin/src/lib.rs` (re-exports, if applicable — verify)

The handler in this Effect is a `dyn` trait object held by `Arc`. It must be the **same** trait `savvagent-host::InProcessToolHandler` defines (Task 4 introduces that). To avoid a `savvagent-plugin → savvagent-host` cycle, the trait lives in `savvagent-host` and is referenced from `savvagent-plugin` through a small marker re-export pattern: the Effect carries an `Arc<dyn InProcessToolHandler>` where the trait is **defined here** (`savvagent-plugin`) but its host-facing concrete handler implementation lives in the user_agents plugin module.

Decision: define `InProcessToolHandler` in `savvagent-plugin` (it's just a trait — no host imports needed because it consumes only `serde_json::Value` + `Arc<dyn Any>` for context). The host crate provides a concrete `ToolCallContext` value that the handler downcasts via `Any` — see Task 4.

- [ ] **Step 1: Write the failing test**

Append to `crates/savvagent-plugin/src/effect.rs`'s `#[cfg(test)] mod tests` block (or create one if it doesn't exist):

```rust
#[cfg(test)]
mod tests_in_process_tool {
    use super::*;
    use serde_json::Value;
    use std::sync::Arc;
    use async_trait::async_trait;

    struct Stub;

    #[async_trait]
    impl InProcessToolHandler for Stub {
        async fn call(
            &self,
            _input: Value,
            _ctx: Arc<dyn std::any::Any + Send + Sync>,
        ) -> Result<Value, String> {
            Ok(Value::String("ok".into()))
        }
    }

    #[test]
    fn register_in_process_tool_holds_handler() {
        let spec = ToolDef {
            name: "task".into(),
            description: "spawn a subagent".into(),
            input_schema: serde_json::json!({}),
        };
        let effect = Effect::RegisterInProcessTool {
            spec,
            handler: Arc::new(Stub),
        };
        match effect {
            Effect::RegisterInProcessTool { spec, .. } => {
                assert_eq!(spec.name, "task");
            }
            _ => panic!("wrong variant"),
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savvagent-plugin register_in_process_tool_holds_handler`
Expected: FAIL — `InProcessToolHandler` not found.

- [ ] **Step 3: Define `InProcessToolHandler` trait in `savvagent-plugin`**

Create a new module file `crates/savvagent-plugin/src/in_process_tool.rs`:

```rust
//! `InProcessToolHandler` — savvagent-internal trait for tools whose
//! implementation runs on the calling tokio runtime (no stdio child).
//!
//! Used by built-in plugins that need direct access to host state
//! (e.g. the `task` tool needs to construct a SubHost from the
//! parent's `Host`). The concrete context type is opaque here so this
//! crate does not depend on `savvagent-host`; handlers downcast the
//! `Arc<dyn Any>` to `savvagent_host::ToolCallContext`.

use async_trait::async_trait;
use serde_json::Value;
use std::any::Any;
use std::sync::Arc;

#[async_trait]
pub trait InProcessToolHandler: Send + Sync + 'static {
    /// Invoke the tool. `input` is the JSON argument object. `ctx` is
    /// an opaque host-owned value the host provides; handlers
    /// downcast to the concrete type they expect.
    async fn call(
        &self,
        input: Value,
        ctx: Arc<dyn Any + Send + Sync>,
    ) -> Result<Value, String>;
}
```

- [ ] **Step 4: Re-export from `lib.rs`**

In `crates/savvagent-plugin/src/lib.rs`, add:

```rust
mod in_process_tool;
pub use in_process_tool::InProcessToolHandler;
```

- [ ] **Step 5: Add the `Effect` variant**

In `crates/savvagent-plugin/src/effect.rs`, add to the `Effect` enum (next to `RegisterProvider` and `RegisterPreToolGate`):

```rust
    /// Register an in-process tool whose handler runs on the calling
    /// tokio runtime. Used by built-in plugins that need direct access
    /// to host state (the `task` tool from user-agents). The host
    /// stores the handler in `ToolRegistry`'s in-process map; the
    /// `spec.name` must be unique across both in-process and stdio
    /// tools.
    RegisterInProcessTool {
        spec: ToolDef,
        handler: std::sync::Arc<dyn crate::InProcessToolHandler>,
    },
```

`ToolDef` is already in scope from existing variants — verify the existing `use` block at the top of the file. If not present, add `use savvagent_protocol::ToolDef;`.

- [ ] **Step 6: Update `Debug` impl for `Effect` (if hand-rolled)**

If `Effect` derives `Debug`, the `Arc<dyn InProcessToolHandler>` won't implement `Debug`. Two options:
   - Add `#[derive(Debug)]` on the trait — won't work, the trait isn't `Debug`.
   - Hand-roll `Debug` for the new variant only.

Check whether `Effect` uses `#[derive(Debug)]`. If yes, replace with a manual impl. If `Effect` already has a manual `Debug`, add an arm for the new variant:

```rust
            Effect::RegisterInProcessTool { spec, .. } => f
                .debug_struct("RegisterInProcessTool")
                .field("spec", spec)
                .field("handler", &"<dyn InProcessToolHandler>")
                .finish(),
```

- [ ] **Step 7: Run test to verify it passes**

Run: `cargo test -p savvagent-plugin register_in_process_tool_holds_handler`
Expected: PASS.

- [ ] **Step 8: Run the full crate tests**

Run: `cargo test -p savvagent-plugin`
Expected: All tests PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/savvagent-plugin/src/{effect.rs,lib.rs,in_process_tool.rs}
git commit -m "feat(plugin): InProcessToolHandler trait + Effect::RegisterInProcessTool"
```

---

### Task 3: Add `SubagentContext` and `ToolCallContext` to `savvagent-host`

**Files:**
- Modify: `crates/savvagent-host/src/tools.rs`
- Modify: `crates/savvagent-host/src/lib.rs`

These are the concrete types host-side handlers downcast to from the opaque `Arc<dyn Any>` the plugin trait sees.

- [ ] **Step 1: Write the failing test**

In `crates/savvagent-host/src/tools.rs`, append to the `#[cfg(test)] mod tests` block (create one if absent):

```rust
#[test]
fn subagent_context_carries_depth_and_name() {
    let ctx = SubagentContext {
        depth: 1,
        agent_name: "code-reviewer".into(),
        parent_session_id: "abc-123".into(),
    };
    assert_eq!(ctx.depth, 1);
    assert_eq!(ctx.agent_name, "code-reviewer");
}

#[test]
fn tool_call_context_default_has_no_subagent() {
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    // Construction sanity — no real Host needed for this test
    let _ctx = ToolCallContextBuilder {
        subagent: None,
        cancellation: CancellationToken::new(),
    };
}
```

(Note: the second test exercises a builder shape we'll define in step 3 so we can test the type without constructing a full `Host`. The real `ToolCallContext` carries `Arc<Host>` which is impractical to construct in a unit test.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savvagent-host --lib subagent_context_carries_depth_and_name`
Expected: FAIL — type not found.

- [ ] **Step 3: Define the types**

Add to `crates/savvagent-host/src/tools.rs` (near the top, after existing public types):

```rust
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Per-subagent context passed through the in-process tool path.
/// `None` value of `SubagentContext` (carried as `Option<...>`) means
/// the call originates from the parent's main turn loop.
#[derive(Debug, Clone)]
pub struct SubagentContext {
    /// Nesting depth. Parent's first `task` call → depth 1; that
    /// subagent's `task` call → depth 2; and so on. Capped at
    /// `SAVVAGENT_AGENT_MAX_DEPTH` (default 3).
    pub depth: u8,
    /// The agent name (slug) currently executing.
    pub agent_name: String,
    /// The parent (top-level) session ID. Stable across subagent
    /// nesting so hooks can correlate across levels.
    pub parent_session_id: String,
}

/// Concrete context passed to `InProcessToolHandler::call`. Handlers
/// downcast `Arc<dyn Any>` to `Arc<ToolCallContext>` to obtain this.
pub struct ToolCallContext {
    /// The parent `Host`. In-process handlers use this to clone the
    /// provider client, tool registry, gate, permissions, etc.
    pub host: Arc<crate::Host>,
    /// `Some` iff this call originates from a Sub-Host.
    pub subagent: Option<SubagentContext>,
    /// Cancellation token; child of the parent turn's token.
    pub cancellation: CancellationToken,
}

/// Test-only builder. Real construction goes through `Host` and is
/// internal.
#[cfg(test)]
pub(crate) struct ToolCallContextBuilder {
    pub subagent: Option<SubagentContext>,
    pub cancellation: CancellationToken,
}
```

- [ ] **Step 4: Re-export from `lib.rs`**

In `crates/savvagent-host/src/lib.rs`, extend the existing `pub use tools::...` line:

```rust
pub use tools::{BashNetContext, BashNetResolver, NetOverride, SubagentContext, ToolCallContext};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p savvagent-host --lib subagent_context_carries_depth_and_name`
Run: `cargo test -p savvagent-host --lib tool_call_context_default_has_no_subagent`
Expected: Both PASS.

- [ ] **Step 6: Run the full crate tests**

Run: `cargo test -p savvagent-host`
Expected: All pre-existing tests still PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/savvagent-host/src/{tools.rs,lib.rs}
git commit -m "feat(host): SubagentContext + ToolCallContext types"
```

---

### Task 4: Add in-process tool registration to `ToolRegistry`

**Files:**
- Modify: `crates/savvagent-host/src/tools.rs`

`ToolRegistry` currently routes calls to stdio MCP children via the `routes`/`eager_servers`/`lazy_bash` machinery. Add a parallel in-process map and route checks accordingly.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `tools.rs`:

```rust
#[tokio::test]
async fn registry_routes_in_process_tool() {
    use async_trait::async_trait;
    use savvagent_plugin::InProcessToolHandler;
    use serde_json::{Value, json};
    use std::any::Any;
    use std::sync::Arc;

    struct Echo;

    #[async_trait]
    impl InProcessToolHandler for Echo {
        async fn call(
            &self,
            input: Value,
            _ctx: Arc<dyn Any + Send + Sync>,
        ) -> Result<Value, String> {
            Ok(input)
        }
    }

    let registry = ToolRegistry::empty_for_test();
    let spec = ToolDef {
        name: "echo".into(),
        description: "echo input".into(),
        input_schema: json!({"type": "object"}),
    };
    registry.register_in_process_tool(spec, Arc::new(Echo)).await;

    let ctx: Arc<dyn Any + Send + Sync> = Arc::new(()) as Arc<dyn Any + Send + Sync>;
    let outcome = registry
        .call_in_process("echo", json!({"hi": 1}), ctx)
        .await;
    assert!(outcome.is_ok());
    assert_eq!(outcome.unwrap(), json!({"hi": 1}));
}
```

`ToolRegistry::empty_for_test()` is a new test-only constructor we'll need to add (or expose via `pub(crate)`).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savvagent-host --lib registry_routes_in_process_tool`
Expected: FAIL — methods don't exist.

- [ ] **Step 3: Add the in-process handler map to `ToolRegistry`**

Find `pub struct ToolRegistry` in `tools.rs`. Add a new field:

```rust
    /// In-process tool handlers, registered by built-in plugins via
    /// `Effect::RegisterInProcessTool`. Looked up before the stdio
    /// children map.
    in_process: tokio::sync::RwLock<
        std::collections::HashMap<String, Arc<dyn savvagent_plugin::InProcessToolHandler>>,
    >,
```

In `ToolRegistry::connect` (the existing constructor), initialize:

```rust
            in_process: tokio::sync::RwLock::new(std::collections::HashMap::new()),
```

If there's a `pub(crate) fn empty_for_test()` already, add the same init. If not, add this method:

```rust
    #[cfg(test)]
    pub(crate) fn empty_for_test() -> Arc<Self> {
        Arc::new(Self {
            // ... existing fields default-empty ...
            in_process: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        })
    }
```

The `// existing fields default-empty` placeholder must be filled in by reading the struct — every field gets a sensible empty default. The exact field set is whatever `connect()` builds when given a `HostConfig` with no `tools`. Mirror that.

- [ ] **Step 4: Add `register_in_process_tool` method**

In `impl ToolRegistry`:

```rust
    pub async fn register_in_process_tool(
        self: &Arc<Self>,
        spec: ToolDef,
        handler: Arc<dyn savvagent_plugin::InProcessToolHandler>,
    ) {
        let mut guard = self.in_process.write().await;
        guard.insert(spec.name.clone(), handler);
        // Also make the spec visible via `tool_defs()` — see Step 5.
        self.in_process_defs.write().await.insert(spec.name.clone(), spec);
    }
```

This requires a second field, `in_process_defs: RwLock<HashMap<String, ToolDef>>`. Add it next to `in_process`:

```rust
    in_process_defs: tokio::sync::RwLock<
        std::collections::HashMap<String, ToolDef>,
    >,
```

Initialize in `connect()` and `empty_for_test()` the same way.

- [ ] **Step 5: Update `tool_defs()` to include in-process tools**

Find the existing `pub async fn tool_defs(&self) -> Vec<ToolDef>` (it currently aggregates stdio `eager_servers` definitions). Extend it:

```rust
    pub async fn tool_defs(&self) -> Vec<ToolDef> {
        let mut out: Vec<ToolDef> = /* existing stdio + bash aggregation */;
        let in_proc = self.in_process_defs.read().await;
        for def in in_proc.values() {
            out.push(def.clone());
        }
        out
    }
```

The `/* existing ... */` placeholder is whatever the current implementation does — read it and integrate the new branch.

- [ ] **Step 6: Add `call_in_process` method**

This is the path the SubHost / parent uses to dispatch to an in-process tool:

```rust
    pub async fn call_in_process(
        &self,
        name: &str,
        input: Value,
        ctx: Arc<dyn std::any::Any + Send + Sync>,
    ) -> Result<Value, String> {
        let guard = self.in_process.read().await;
        let Some(handler) = guard.get(name).cloned() else {
            return Err(format!("unknown in-process tool: {name}"));
        };
        drop(guard);
        handler.call(input, ctx).await
    }
```

- [ ] **Step 7: Update `call_with_bash_net_override` to check in-process first**

Find the existing `pub async fn call_with_bash_net_override`. Before the lazy-bash path, add:

```rust
        // In-process tools take precedence over stdio routes.
        if self.in_process.read().await.contains_key(name) {
            // The host wires `ctx` per call; this method's signature
            // does not carry one (legacy callers). In-process tools are
            // only called via `Host::dispatch_tool_call` which uses the
            // dedicated path. Returning an error here is a guardrail
            // against bypass.
            return ToolCallOutcome::error(format!(
                "in-process tool `{name}` must be dispatched via Host::dispatch_tool_call"
            ));
        }
```

Rationale: the existing stdio-path signature doesn't carry `ToolCallContext`, so we never want it routing to in-process tools. The SubHost / parent dispatcher uses `call_in_process` directly. The guardrail catches programming errors.

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test -p savvagent-host --lib registry_routes_in_process_tool`
Expected: PASS.

Run: `cargo test -p savvagent-host`
Expected: All tests PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/savvagent-host/src/tools.rs
git commit -m "feat(host): in-process tool registration + dispatch in ToolRegistry"
```

---

### Task 5: Add `ScopedToolRegistry` wrapper

**Files:**
- Create: `crates/savvagent-host/src/scoped_registry.rs`
- Modify: `crates/savvagent-host/src/lib.rs`

The SubHost uses this to gate tool dispatch by name against the agent's allowlist.

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent-host/src/scoped_registry.rs`:

```rust
//! `ScopedToolRegistry` — wraps `Arc<ToolRegistry>` and rejects calls
//! whose tool name is not in a per-subagent allowlist. Used by `SubHost`
//! to enforce the `tools:` frontmatter scoping at runtime, defending
//! against a model that fabricates a tool name from training data.

use crate::tools::ToolRegistry;
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Clone)]
pub struct ScopedToolRegistry {
    inner: Arc<ToolRegistry>,
    allowed: Arc<HashSet<String>>,
}

impl ScopedToolRegistry {
    pub fn new(inner: Arc<ToolRegistry>, allowed: HashSet<String>) -> Self {
        Self {
            inner,
            allowed: Arc::new(allowed),
        }
    }

    pub fn allows(&self, name: &str) -> bool {
        self.allowed.contains(name)
    }

    pub fn inner(&self) -> &Arc<ToolRegistry> {
        &self.inner
    }

    pub fn allowed(&self) -> &HashSet<String> {
        &self.allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_known_name() {
        let inner = ToolRegistry::empty_for_test();
        let mut allowed = HashSet::new();
        allowed.insert("tool-fs:read_file".to_string());
        let scoped = ScopedToolRegistry::new(inner, allowed);
        assert!(scoped.allows("tool-fs:read_file"));
        assert!(!scoped.allows("tool-fs:write_file"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savvagent-host --lib scoped_registry`
Expected: FAIL — module not found.

- [ ] **Step 3: Wire the module into the crate**

Add to `crates/savvagent-host/src/lib.rs` (near other `mod ...;` declarations):

```rust
mod scoped_registry;
pub use scoped_registry::ScopedToolRegistry;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p savvagent-host --lib scoped_registry`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/{scoped_registry.rs,lib.rs}
git commit -m "feat(host): ScopedToolRegistry wrapper for per-subagent allowlists"
```

---

## Phase 2 — SubHost runtime

### Task 6: Define the `SubHost` struct skeleton

**Files:**
- Create: `crates/savvagent-host/src/subhost.rs`
- Modify: `crates/savvagent-host/src/lib.rs`

This task only introduces the type and a stub `run_subagent` that errors with "unimplemented" — Task 7 wires the real loop. Splitting keeps each task small.

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent-host/src/subhost.rs`:

```rust
//! `SubHost` — a subagent execution context. Owns its own session
//! state, system prompt, model selection, and tool filter; shares the
//! parent's `ProviderClient`, `ToolRegistry`, `PreToolUseGate`,
//! permissions, and sandbox config via `Arc`.
//!
//! See `docs/superpowers/specs/2026-05-23-user-agents-design.md` §2.

use std::collections::HashSet;
use std::sync::Arc;

use savvagent_protocol::ToolDef;
use tokio_util::sync::CancellationToken;

use crate::scoped_registry::ScopedToolRegistry;
use crate::tools::SubagentContext;
use crate::Host;

/// Sub-Host configuration. Built by `TaskToolHandler` from an
/// `AgentSpec` and a parent `ToolCallContext`.
pub struct SubHost {
    pub(crate) parent: Arc<Host>,
    pub(crate) ctx: SubagentContext,
    pub(crate) system_prompt: String,
    pub(crate) model: Option<String>,
    pub(crate) tools: ScopedToolRegistry,
    pub(crate) tool_defs: Vec<ToolDef>,
    pub(crate) cancellation: CancellationToken,
}

impl SubHost {
    pub fn new(
        parent: Arc<Host>,
        ctx: SubagentContext,
        system_prompt: String,
        model: Option<String>,
        allowed_names: HashSet<String>,
        tool_defs: Vec<ToolDef>,
        cancellation: CancellationToken,
    ) -> Self {
        let registry = parent.tool_registry_arc();
        let tools = ScopedToolRegistry::new(registry, allowed_names);
        Self {
            parent,
            ctx,
            system_prompt,
            model,
            tools,
            tool_defs,
            cancellation,
        }
    }

    /// Drive the subagent loop to its `end_turn`. Returns the final
    /// assistant text or an error.
    pub async fn run_subagent(&self, prompt: String) -> Result<String, SubHostError> {
        let _ = prompt;
        Err(SubHostError::Unimplemented)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SubHostError {
    #[error("subagent loop not yet implemented")]
    Unimplemented,
    #[error("subagent cancelled")]
    Cancelled,
    #[error("subagent depth limit exceeded")]
    DepthExceeded,
    #[error("subagent produced no output")]
    EmptyOutput,
    #[error("provider error: {0}")]
    Provider(String),
    #[error("tool error: {0}")]
    Tool(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_host_error_variants_compile() {
        // Smoke test: each variant constructs.
        let _ = SubHostError::Unimplemented;
        let _ = SubHostError::Cancelled;
        let _ = SubHostError::DepthExceeded;
        let _ = SubHostError::EmptyOutput;
        let _ = SubHostError::Provider("p".into());
        let _ = SubHostError::Tool("t".into());
    }
}
```

The `parent.tool_registry_arc()` method doesn't exist yet — we add it in step 3.

- [ ] **Step 2: Wire module + re-exports**

In `crates/savvagent-host/src/lib.rs`:

```rust
mod subhost;
pub use subhost::{SubHost, SubHostError};
```

- [ ] **Step 3: Add `Host::tool_registry_arc()` accessor**

In `crates/savvagent-host/src/session.rs`, on `impl Host`, add:

```rust
    /// Clone the underlying `Arc<ToolRegistry>` for sharing with a
    /// `SubHost`. Internal API surfaced for the in-process tool path.
    pub fn tool_registry_arc(&self) -> Arc<crate::tools::ToolRegistry> {
        self.tools.clone()
    }
```

(The field is `tools: Arc<ToolRegistry>` — verify by reading the `Host` struct. If named differently, adapt.)

- [ ] **Step 4: Build the crate**

Run: `cargo build -p savvagent-host`
Expected: Compiles cleanly.

- [ ] **Step 5: Run the smoke test**

Run: `cargo test -p savvagent-host --lib subhost`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/{subhost.rs,lib.rs,session.rs}
git commit -m "feat(host): SubHost skeleton + tool_registry_arc accessor"
```

---

### Task 7: Implement `SubHost::run_subagent` (subagent turn loop)

**Files:**
- Modify: `crates/savvagent-host/src/subhost.rs`

The simplest version uses the parent's `ProviderClient` directly — same `CompleteRequest`/`CompleteResponse` types — and runs its own mini-loop instead of trying to reuse `Host::run_turn_inner` (which carries too much main-session state to be easily reparameterized).

- [ ] **Step 1: Write the failing test**

Append to `subhost.rs`'s test module:

```rust
    use crate::ScopedToolRegistry;
    // The real test requires a stub provider — use the existing
    // `MockProviderClient` from `crates/savvagent-host/src/test_support.rs`
    // (verify it exists; if not, this test will need a small stub).

    // NOTE: this is an integration test, not a unit test — it will
    // live in `crates/savvagent-host/tests/subhost_loop.rs` once we
    // have the wiring. Keep the placeholder here so reviewers see
    // intent during this task; the real assertion lands in Task 8.
    #[tokio::test]
    async fn run_subagent_returns_error_when_unimplemented_removed() {
        // Once Step 3 lands, calling run_subagent on a real Host stub
        // must NOT return SubHostError::Unimplemented. We assert by
        // looking at the variant name — anything except Unimplemented
        // means the loop is wired.
        // Placeholder: returns Ok(()) until we have a Host stub here.
    }
```

(For the real assertion we need a host stub, which the project may or may not have. Read `crates/savvagent-host/src/` for any `test_support.rs` or in-tree stub provider. If absent, create one in step 4.)

- [ ] **Step 2: Add the loop body**

Replace the stub `run_subagent` body in `subhost.rs` with:

```rust
    pub async fn run_subagent(&self, prompt: String) -> Result<String, SubHostError> {
        use savvagent_protocol::{CompleteRequest, ContentBlock, Message, MessageRole, StopReason};

        let mut messages: Vec<Message> = vec![Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: prompt }],
        }];

        let provider = self.parent.provider_client();
        let model = self
            .model
            .clone()
            .unwrap_or_else(|| self.parent.active_model_blocking().unwrap_or_default());

        loop {
            if self.cancellation.is_cancelled() {
                return Err(SubHostError::Cancelled);
            }

            let req = CompleteRequest {
                model: model.clone(),
                system: Some(self.system_prompt.clone()),
                messages: messages.clone(),
                tools: self.tool_defs.clone(),
                stream: false,
            };

            let resp = provider
                .complete(req)
                .await
                .map_err(|e| SubHostError::Provider(e.to_string()))?;

            messages.push(Message {
                role: MessageRole::Assistant,
                content: resp.content.clone(),
            });

            match resp.stop_reason {
                StopReason::EndTurn => {
                    let text = collect_assistant_text(&resp.content);
                    return if text.is_empty() {
                        Err(SubHostError::EmptyOutput)
                    } else {
                        Ok(text)
                    };
                }
                StopReason::ToolUse => {
                    let tool_calls = extract_tool_calls(&resp.content);
                    if tool_calls.is_empty() {
                        // Provider quirk — coerce to end_turn.
                        let text = collect_assistant_text(&resp.content);
                        return if text.is_empty() {
                            Err(SubHostError::EmptyOutput)
                        } else {
                            Ok(text)
                        };
                    }
                    let mut tool_results = Vec::with_capacity(tool_calls.len());
                    for call in tool_calls {
                        let result = self.dispatch_tool_call(&call).await;
                        tool_results.push(result);
                    }
                    messages.push(Message {
                        role: MessageRole::User,
                        content: tool_results,
                    });
                }
                other => {
                    return Err(SubHostError::Provider(format!(
                        "subagent: unexpected stop_reason {other:?}"
                    )));
                }
            }
        }
    }
```

Helper functions (in the same file, below `impl SubHost`):

```rust
fn collect_assistant_text(content: &[savvagent_protocol::ContentBlock]) -> String {
    use savvagent_protocol::ContentBlock;
    let mut out = String::new();
    for block in content {
        if let ContentBlock::Text { text } = block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
}

struct PendingToolCall {
    id: String,
    name: String,
    input: serde_json::Value,
}

fn extract_tool_calls(content: &[savvagent_protocol::ContentBlock]) -> Vec<PendingToolCall> {
    use savvagent_protocol::ContentBlock;
    content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some(PendingToolCall {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        })
        .collect()
}

impl SubHost {
    async fn dispatch_tool_call(
        &self,
        call: &PendingToolCall,
    ) -> savvagent_protocol::ContentBlock {
        use savvagent_protocol::ContentBlock;

        // Allowlist check
        if !self.tools.allows(&call.name) {
            return ContentBlock::ToolResult {
                tool_use_id: call.id.clone(),
                content: vec![ContentBlock::Text {
                    text: format!("{} not available to this subagent", call.name),
                }],
                is_error: true,
            };
        }

        // PreToolUse hook gate, identical to parent path
        let gate = self.parent.pre_tool_gate_snapshot().await;
        if let Some(gate) = gate {
            match gate.check(&call.name, &call.input).await {
                crate::PreToolDecision::Allow => {}
                crate::PreToolDecision::Block(reason) => {
                    return ContentBlock::ToolResult {
                        tool_use_id: call.id.clone(),
                        content: vec![ContentBlock::Text {
                            text: format!("[blocked] {reason}"),
                        }],
                        is_error: true,
                    };
                }
            }
        }

        // In-process or stdio dispatch
        if self
            .parent
            .tool_registry()
            .in_process_has(&call.name)
            .await
        {
            let ctx = std::sync::Arc::new(crate::ToolCallContext {
                host: self.parent.clone(),
                subagent: Some(self.ctx.clone()),
                cancellation: self.cancellation.clone(),
            }) as std::sync::Arc<dyn std::any::Any + Send + Sync>;
            let outcome = self
                .parent
                .tool_registry()
                .call_in_process(&call.name, call.input.clone(), ctx)
                .await;
            match outcome {
                Ok(v) => ContentBlock::ToolResult {
                    tool_use_id: call.id.clone(),
                    content: vec![ContentBlock::Text { text: v.to_string() }],
                    is_error: false,
                },
                Err(e) => ContentBlock::ToolResult {
                    tool_use_id: call.id.clone(),
                    content: vec![ContentBlock::Text { text: e }],
                    is_error: true,
                },
            }
        } else {
            let outcome = self
                .parent
                .tool_registry()
                .call_with_bash_net_override(
                    &call.name,
                    call.input.clone(),
                    crate::NetOverride::default(),
                )
                .await;
            // Convert ToolCallOutcome → ContentBlock::ToolResult
            outcome.into_tool_result_block(&call.id)
        }
    }
}
```

This requires:
- `Host::provider_client()` accessor — add it to `session.rs`:

  ```rust
      pub fn provider_client(&self) -> Arc<dyn ProviderClient> {
          self.provider.clone()
      }
  ```

- `Host::active_model_blocking()` — returns the model name without awaiting. If the existing pattern uses an `Arc<RwLock<String>>`, add a `try_read` version, or use the sync `active_model` if available. If the project's pattern is async-only, change the call site to `.await`.

- `Host::pre_tool_gate_snapshot()` — async accessor returning `Option<Arc<dyn PreToolUseGate>>`. Sub-project B added the gate field; expose a getter:

  ```rust
      pub async fn pre_tool_gate_snapshot(&self) -> Option<Arc<dyn PreToolUseGate>> {
          self.pre_tool_gate.read().await.clone()
      }
  ```

- `ToolRegistry::in_process_has()`:

  ```rust
      pub async fn in_process_has(&self, name: &str) -> bool {
          self.in_process.read().await.contains_key(name)
      }
  ```

- `Host::tool_registry()` accessor (likely already exists; if not, add).
- `ToolCallOutcome::into_tool_result_block(&self, id: &str) -> ContentBlock` — adapter that converts the host's `ToolCallOutcome` into the protocol's `ContentBlock::ToolResult`. Add to wherever `ToolCallOutcome` lives.

- [ ] **Step 3: Add the helper accessors and adapters above to `session.rs` and `tools.rs`**

Add each missing accessor as listed above. Keep them small, each a few lines. If any clashes with an existing name, prefer the existing.

- [ ] **Step 4: Build and run the existing tests**

Run: `cargo build -p savvagent-host`
Expected: Compiles. If `MessageRole`, `StopReason`, or `ContentBlock` variant names differ in this codebase, adapt — read `crates/savvagent-protocol/src/content.rs` and `crates/savvagent-protocol/src/lib.rs` to verify.

Run: `cargo test -p savvagent-host`
Expected: All pre-existing tests still PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/{subhost.rs,session.rs,tools.rs}
git commit -m "feat(host): SubHost::run_subagent — minimal turn loop"
```

---

### Task 8: Integration test — SubHost runs to end_turn against a stub provider

**Files:**
- Create: `crates/savvagent-host/tests/subhost_basic.rs`

End-to-end check that the loop wires correctly. Uses a stub `ProviderClient` that emits `end_turn` immediately with a fixed text block, and asserts the returned String.

- [ ] **Step 1: Write the integration test**

Create `crates/savvagent-host/tests/subhost_basic.rs`:

```rust
//! End-to-end smoke for SubHost.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::{Host, HostConfig, SubHost, SubagentContext};
use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ContentBlock, ListModelsResponse, ModelInfo,
    ProviderError, StopReason,
};
use tokio_util::sync::CancellationToken;

struct StubProvider {
    reply: String,
}

#[async_trait]
impl ProviderClient for StubProvider {
    async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, ProviderError> {
        Ok(CompleteResponse {
            content: vec![ContentBlock::Text { text: self.reply.clone() }],
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
        })
    }

    async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
        Ok(ListModelsResponse {
            models: vec![ModelInfo {
                id: "stub-model".into(),
                display_name: Some("stub".into()),
            }],
            default: Some("stub-model".into()),
        })
    }
}

#[tokio::test]
async fn subhost_returns_text_on_end_turn() {
    // Build a Host with the stub provider (no tools, no real connection).
    let mut config = HostConfig::default();
    // `HostConfig::default()` builds a host that uses default provider;
    // we'd ordinarily construct it via `with_components`. Use whichever
    // entry point the existing host tests use — `Host::with_components`
    // typically.
    let provider: Arc<dyn ProviderClient> = Arc::new(StubProvider {
        reply: "hello from subagent".into(),
    });

    let host = Host::with_components(
        config,
        provider,
        // tool registry — empty
        savvagent_host::tools::ToolRegistry::empty_for_test(),
    )
    .await
    .expect("host construction");

    let ctx = SubagentContext {
        depth: 1,
        agent_name: "test-agent".into(),
        parent_session_id: "session-1".into(),
    };
    let cancellation = CancellationToken::new();

    let sub = SubHost::new(
        host.clone(),
        ctx,
        "You are a test agent.".into(),
        None,
        HashSet::new(),
        vec![],
        cancellation,
    );

    let result = sub.run_subagent("hi".into()).await.expect("subagent ok");
    assert_eq!(result, "hello from subagent");
}
```

The exact `Host::with_components` signature must match what's on master after sub-project B. If the existing test crate (`crates/savvagent-host/tests/...`) already has a host construction helper, prefer that.

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test -p savvagent-host --test subhost_basic`
Expected: PASS. If the construction helper signature differs, fix the test to match — do not modify the production `Host` signature for a test.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent-host/tests/subhost_basic.rs
git commit -m "test(host): integration smoke for SubHost end_turn path"
```

---

### Task 9: Add depth cap with env-configurable max

**Files:**
- Modify: `crates/savvagent-host/src/subhost.rs`

- [ ] **Step 1: Write the failing test**

Append to the test module in `subhost.rs`:

```rust
    #[test]
    fn depth_limit_env_default_is_three() {
        // Don't set the env var — should default.
        let limit = crate::subhost::max_depth_from_env();
        assert_eq!(limit, 3);
    }

    #[test]
    fn depth_limit_env_override_parses() {
        std::env::set_var("SAVVAGENT_AGENT_MAX_DEPTH", "5");
        assert_eq!(crate::subhost::max_depth_from_env(), 5);
        std::env::remove_var("SAVVAGENT_AGENT_MAX_DEPTH");
    }

    #[test]
    fn depth_limit_env_invalid_falls_back() {
        std::env::set_var("SAVVAGENT_AGENT_MAX_DEPTH", "not-a-number");
        assert_eq!(crate::subhost::max_depth_from_env(), 3);
        std::env::remove_var("SAVVAGENT_AGENT_MAX_DEPTH");
    }
```

(Note: the env-var tests serialize via `#[serial]` if `serial_test` is in the workspace, or use a `Mutex`. If not, they race — accept that and rely on test isolation. The memory note `[rust_i18n locale leaks between parallel tests]` is the same pattern; check whether the project already has a `HOME_LOCK`-style mutex and use it.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savvagent-host --lib depth_limit_env_default_is_three`
Expected: FAIL — `max_depth_from_env` not defined.

- [ ] **Step 3: Add the function**

In `subhost.rs`, near the top:

```rust
const DEFAULT_MAX_DEPTH: u8 = 3;

pub fn max_depth_from_env() -> u8 {
    std::env::var("SAVVAGENT_AGENT_MAX_DEPTH")
        .ok()
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(DEFAULT_MAX_DEPTH)
}
```

- [ ] **Step 4: Enforce the cap in `SubHost::new`**

Update `SubHost::new` to return `Result<Self, SubHostError>` and reject construction at depth > limit:

```rust
    pub fn new(
        parent: Arc<Host>,
        ctx: SubagentContext,
        system_prompt: String,
        model: Option<String>,
        allowed_names: HashSet<String>,
        tool_defs: Vec<ToolDef>,
        cancellation: CancellationToken,
    ) -> Result<Self, SubHostError> {
        if ctx.depth > max_depth_from_env() {
            return Err(SubHostError::DepthExceeded);
        }
        let registry = parent.tool_registry_arc();
        let tools = ScopedToolRegistry::new(registry, allowed_names);
        Ok(Self {
            parent,
            ctx,
            system_prompt,
            model,
            tools,
            tool_defs,
            cancellation,
        })
    }
```

Update Task 8's integration test to `.expect("construction")` on the new `Result`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p savvagent-host --lib depth_limit`
Run: `cargo test -p savvagent-host --test subhost_basic`
Expected: All PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/subhost.rs crates/savvagent-host/tests/subhost_basic.rs
git commit -m "feat(host): SubHost depth cap via SAVVAGENT_AGENT_MAX_DEPTH"
```

---

### Task 10: Emit `HostEvent::SubagentStop` after subagent end_turn

**Files:**
- Modify: `crates/savvagent-host/src/subhost.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent-host/tests/subhost_stop_event.rs`:

```rust
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use savvagent_host::{Host, SubHost, SubagentContext};
use savvagent_mcp::ProviderClient;
use savvagent_plugin::{HookKind, HostEvent};
use savvagent_protocol::{CompleteRequest, CompleteResponse, ContentBlock, StopReason};
use tokio_util::sync::CancellationToken;

struct CapturingPlugin {
    seen: Arc<Mutex<Vec<HookKind>>>,
}

// Implement Plugin and subscribe to SubagentStop. See the user_hooks
// plugin tests for a working subscription example.
// The exact Plugin trait shape lives in savvagent-plugin; mirror that
// crate's test helpers.

#[tokio::test]
async fn subagent_stop_event_fires_after_end_turn() {
    // 1. Build Host with stub provider that immediately end-turns
    // 2. Register CapturingPlugin
    // 3. Build + run a SubHost
    // 4. Assert CapturingPlugin saw HookKind::SubagentStop
    // 5. Assert it saw it AFTER any tool dispatch and BEFORE the
    //    SubHost::run_subagent return.
    //
    // Implement using the same patterns sub-project B used in
    // crates/savvagent/tests/user_hooks_*.
}
```

Fill the test body with the same plugin-subscription patterns used by `user_hooks` integration tests (look under `crates/savvagent/tests/` for examples). If those tests aren't present, the simpler shape is to put the capture into the `Host`'s plugin runtime directly via `Effect::Stack` and a custom plugin registered through `HostConfig::with_plugin`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savvagent-host --test subhost_stop_event`
Expected: FAIL — `SubagentStop` not emitted.

- [ ] **Step 3: Emit the event in `run_subagent`**

In `subhost.rs`, just before the `return Ok(text)` in the `StopReason::EndTurn` arm:

```rust
                    self.parent
                        .emit_host_event(HostEvent::SubagentStop {
                            agent_name: self.ctx.agent_name.clone(),
                            success: true,
                        })
                        .await;
```

`emit_host_event` is the existing host method that dispatches `HostEvent` to subscribed plugins. Verify its name (`broadcast_host_event` / `dispatch_host_event` / similar) by reading `session.rs`.

Do NOT emit `SubagentStop` from the cancellation branch — cancelled subagents don't fire the event per spec §4.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p savvagent-host --test subhost_stop_event`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/subhost.rs crates/savvagent-host/tests/subhost_stop_event.rs
git commit -m "feat(host): SubHost emits HostEvent::SubagentStop on clean end_turn"
```

---

### Task 11: Extend user_hooks payload with optional `subagent` field

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_hooks/payload.rs`

The existing `pre_tool_use` and `post_tool_use` builders take `ctx`, `tool_name`, `tool_input`. Extend them to accept an optional agent name and append the field when present.

- [ ] **Step 1: Write the failing test**

Append to `payload.rs`'s test module:

```rust
    #[test]
    fn pre_tool_use_payload_omits_subagent_for_parent_turn() {
        use std::path::Path;
        let ctx = HookContext {
            session_id: "s",
            transcript_path: Path::new("/tmp/t.json"),
            cwd: Path::new("/proj"),
        };
        let payload = pre_tool_use(&ctx, "tool-fs:read_file", &json!({"path": "x"}), None);
        assert!(payload.get("subagent").is_none());
    }

    #[test]
    fn pre_tool_use_payload_includes_subagent_when_set() {
        use std::path::Path;
        let ctx = HookContext {
            session_id: "s",
            transcript_path: Path::new("/tmp/t.json"),
            cwd: Path::new("/proj"),
        };
        let payload = pre_tool_use(
            &ctx,
            "tool-fs:read_file",
            &json!({"path": "x"}),
            Some("code-reviewer"),
        );
        assert_eq!(payload.get("subagent"), Some(&json!("code-reviewer")));
    }

    #[test]
    fn subagent_stop_payload_shape() {
        use std::path::Path;
        let ctx = HookContext {
            session_id: "s",
            transcript_path: Path::new("/tmp/t.json"),
            cwd: Path::new("/proj"),
        };
        let payload = subagent_stop(&ctx, "code-reviewer", false);
        assert_eq!(payload.get("hook_event_name"), Some(&json!("SubagentStop")));
        assert_eq!(payload.get("subagent"), Some(&json!("code-reviewer")));
        assert_eq!(payload.get("stop_hook_active"), Some(&json!(false)));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p savvagent --lib user_hooks::payload`
Expected: FAIL — signatures don't match / `subagent_stop` not defined.

- [ ] **Step 3: Update existing builders**

In `payload.rs`, update `pre_tool_use`:

```rust
pub fn pre_tool_use(
    ctx: &HookContext<'_>,
    tool_name: &str,
    tool_input: &Value,
    subagent: Option<&str>,
) -> Value {
    let mut fields: Vec<(&str, Value)> = vec![
        ("tool_name", json!(tool_name)),
        ("tool_input", tool_input.clone()),
    ];
    if let Some(name) = subagent {
        fields.push(("subagent", json!(name)));
    }
    base(ctx, HookEvent::PreToolUse).extend(&fields)
}
```

Update `post_tool_use` the same way: add `subagent: Option<&str>` as the last parameter, append `("subagent", json!(name))` when set.

- [ ] **Step 4: Add `HookEvent::SubagentStop` variant**

In `crates/savvagent/src/plugin/builtin/user_hooks/discovery.rs`, find `pub enum HookEvent` and add `SubagentStop`. In `event_name` in `payload.rs`, add the arm:

```rust
        HookEvent::SubagentStop => "SubagentStop",
```

- [ ] **Step 5: Add `subagent_stop` builder**

In `payload.rs`:

```rust
pub fn subagent_stop(
    ctx: &HookContext<'_>,
    agent_name: &str,
    stop_hook_active: bool,
) -> Value {
    base(ctx, HookEvent::SubagentStop).extend(&[
        ("subagent", json!(agent_name)),
        ("stop_hook_active", json!(stop_hook_active)),
    ])
}
```

- [ ] **Step 6: Update all call sites of `pre_tool_use` / `post_tool_use`**

`grep -rn "pre_tool_use(\|post_tool_use(" crates/savvagent/src/plugin/builtin/user_hooks/`
to find every caller and pass `None` (parent-turn calls). The SubHost path passes `Some(&self.ctx.agent_name)` — that wiring lands in Task 12.

- [ ] **Step 7: Run tests**

Run: `cargo test -p savvagent --lib user_hooks::payload`
Run: `cargo test -p savvagent --lib user_hooks`
Expected: All PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_hooks/
git commit -m "feat(plugin/user-hooks): optional subagent field + subagent_stop payload"
```

---

### Task 12: Wire subagent context through PreToolUse dispatch

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_hooks/pre_tool_gate.rs` (or wherever the `UserHooksPreToolGate::check` impl lives)

The `PreToolUseGate::check` signature is `(name, input) -> PreToolDecision`. It needs to know whether the call came from a subagent. Two options:

A) Extend the `PreToolUseGate` trait with an optional context parameter.
B) Pass subagent context via task-local storage (`tokio::task_local!`).

Option A is more explicit; B keeps the trait signature stable. Choose B: the `SubHost::dispatch_tool_call` sets a task-local before calling the gate, the gate reads it and threads `subagent` into payload construction.

- [ ] **Step 1: Write the failing test**

In `pre_tool_gate.rs` or a sibling test file, add a test that:
1. Constructs a `UserHooksPreToolGate` with one PreToolUse hook
2. Calls `check` outside any task-local scope → asserts payload had `subagent` absent
3. Calls `check` inside `SUBAGENT_CONTEXT.scope(Some("code-reviewer"), ...)` → asserts the hook child process received `"subagent":"code-reviewer"` in stdin

The hook child process verifies stdin by writing the parsed payload to a tempfile. The test reads the tempfile back and asserts.

(For brevity here, write the test using a `MockShellRunner` if the existing user_hooks code has one; otherwise use a real `/bin/sh -c "cat > /tmp/savvagent-test-N"` hook.)

- [ ] **Step 2: Define the task-local**

In `crates/savvagent/src/plugin/builtin/user_hooks/payload.rs` or a sibling `context.rs`:

```rust
tokio::task_local! {
    pub static SUBAGENT_CONTEXT: Option<String>;
}
```

- [ ] **Step 3: Update gate to read the task-local**

In `UserHooksPreToolGate::check`, when building the payload:

```rust
        let subagent = SUBAGENT_CONTEXT
            .try_with(|v| v.clone())
            .ok()
            .flatten();
        let payload = pre_tool_use(&ctx, name, input, subagent.as_deref());
```

(`try_with` returns `Err(_)` if outside the scope, which we map to `None`.)

- [ ] **Step 4: Wrap subagent tool dispatch in the task-local scope**

In `crates/savvagent-host/src/subhost.rs`, the `dispatch_tool_call` needs to set the task-local. But `subhost.rs` can't reach the user_hooks plugin's task-local directly (crate boundary).

Resolution: define the task-local in a neutral place that both crates can reference. Two options:
- (a) Define it in `savvagent-host` and have `user_hooks` read from it.
- (b) Make the task-local a plain string carried via `tracing` span fields or `tokio::task_local!` in `savvagent-host`, then re-export.

Pick (a): define the task-local in `savvagent-host/src/subhost.rs`:

```rust
tokio::task_local! {
    pub static SUBAGENT_NAME: Option<String>;
}
```

Wrap dispatch in the scope. Replace the gate call in `dispatch_tool_call`:

```rust
        let gate = self.parent.pre_tool_gate_snapshot().await;
        if let Some(gate) = gate {
            let agent = Some(self.ctx.agent_name.clone());
            let check = SUBAGENT_NAME.scope(agent, async {
                gate.check(&call.name, &call.input).await
            });
            match check.await {
                // ... same Allow/Block handling as before
            }
        }
```

In `user_hooks/pre_tool_gate.rs`:

```rust
        let subagent = savvagent_host::subhost::SUBAGENT_NAME
            .try_with(|v| v.clone())
            .ok()
            .flatten();
```

This requires `pub mod subhost` in `savvagent-host/src/lib.rs` (already public). Promote `SUBAGENT_NAME` to `pub`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p savvagent user_hooks`
Run: `cargo test -p savvagent-host subhost`
Expected: All PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/subhost.rs crates/savvagent/src/plugin/builtin/user_hooks/
git commit -m "feat: thread subagent name through PreToolUse via tokio task-local"
```

---

### Task 13: Wire SubagentStop event into user_hooks dispatch

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs`

The existing plugin already subscribes to `HookKind` events. Add `SubagentStop` to its subscription list and the dispatch logic that maps it to `subagent_stop` payload + the user's `SubagentStop` hook list.

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent/tests/user_hooks_subagent_stop.rs`:

```rust
//! End-to-end: a user SubagentStop hook fires when a SubHost reaches
//! end_turn. The hook's stdin payload includes `subagent` and
//! `stop_hook_active`. The hook can re-prompt by emitting structured
//! JSON (`continue: false` + `additionalContext`); a second firing has
//! `stop_hook_active: true`.

// Test body uses the same fixture pattern as the other
// user_hooks_*.rs tests in this directory.
```

Implement the test body using the project's existing user_hooks fixture machinery (look at `crates/savvagent/tests/user_hooks_*.rs` for templates).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savvagent --test user_hooks_subagent_stop`
Expected: FAIL.

- [ ] **Step 3: Subscribe to SubagentStop in the plugin manifest**

In `user_hooks/mod.rs`, find where the plugin declares `Manifest { subscriptions: vec![HookKind::PreToolUse, ...] }` (or similar). Add `HookKind::SubagentStop`.

- [ ] **Step 4: Dispatch SubagentStop in the plugin's event handler**

In the `on_event` impl for the plugin, add:

```rust
            HostEvent::SubagentStop { agent_name, success: _ } => {
                let payload = subagent_stop(&ctx, agent_name, self.stop_hook_active);
                self.dispatch_event(HookEvent::SubagentStop, payload).await
            }
```

The `stop_hook_active` field is the same flag B uses for `Stop` — set true when re-firing after a hook re-prompt, false otherwise. Track it on the plugin per-subagent (HashMap by agent_name → bool) if you want per-agent independence, or reuse the existing single boolean if scoped loosely is acceptable. Per spec §4: the spec doesn't require per-agent isolation; reuse the single boolean.

- [ ] **Step 5: Run tests**

Run: `cargo test -p savvagent --test user_hooks_subagent_stop`
Run: `cargo test -p savvagent`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_hooks/mod.rs crates/savvagent/tests/user_hooks_subagent_stop.rs
git commit -m "feat(plugin/user-hooks): dispatch SubagentStop event with stop_hook_active loop guard"
```

---

## Phase 3 — Transcript v2

### Task 14: Bump `TRANSCRIPT_SCHEMA_VERSION` and add nested subagent_transcript field

**Files:**
- Modify: `crates/savvagent-host/src/session.rs`

- [ ] **Step 1: Write the failing test**

In `session.rs`, append to the test module:

```rust
#[tokio::test]
async fn transcript_schema_version_is_two() {
    assert_eq!(TRANSCRIPT_SCHEMA_VERSION, 2);
}

#[tokio::test]
async fn transcript_round_trip_with_nested_subagent_transcript() {
    // Round-trip a TranscriptFile that includes a tool_call with
    // a subagent_transcript field, assert the deserializer preserves
    // the nested data.
    //
    // Use a temp file; tempfile crate is already in dev-deps.
}

#[tokio::test]
async fn transcript_v1_loads_with_warn_log() {
    // Construct a TranscriptFile JSON with version=1 (the old schema),
    // write it, call `load_transcript`, assert it returns Ok with the
    // messages intact (no subagent transcripts, since v1 didn't have
    // them).
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p savvagent-host --lib transcript_schema_version_is_two`
Expected: FAIL — version is 1.

- [ ] **Step 3: Bump the constant**

In `session.rs`:

```rust
pub const TRANSCRIPT_SCHEMA_VERSION: u32 = 2;
```

- [ ] **Step 4: Extend the tool-call serialization to embed subagent_transcript**

The exact serialization shape depends on the current `Message` / `ContentBlock::ToolUse` storage in transcripts. Strategy: add an optional `subagent_transcript: Option<SubagentTranscript>` field on the per-tool-call record, where `SubagentTranscript` is a new struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentTranscript {
    pub agent_name: String,
    pub model: Option<String>,
    pub messages: Vec<Message>,
    // Optional: tool calls list, if we track them separately
}
```

How this hangs off the existing transcript format depends on how `save_transcript` / `load_transcript` serializes. Find the serialization point (look for `serde_json::to_string_pretty` or similar in `session.rs`) and inject the optional field next to the `tool_call` entries.

If the current shape is just `messages: Vec<Message>` and `ContentBlock::ToolUse` doesn't carry a sidecar, this requires a new sibling vector `subagent_transcripts: Vec<(call_id, SubagentTranscript)>` on `TranscriptFile`. Choose whichever fits cleaner — minimize churn.

- [ ] **Step 5: Wire SubHost to write its transcript into the parent**

After `SubHost::run_subagent` returns, the `TaskToolHandler` (Task 21) is the natural place to write the subagent's transcript snapshot into the parent's pending tool-call record. Add a method on `SubHost`:

```rust
    pub fn snapshot_transcript(&self) -> SubagentTranscript {
        SubagentTranscript {
            agent_name: self.ctx.agent_name.clone(),
            model: self.model.clone(),
            messages: self.messages_snapshot.clone(), // see below
        }
    }
```

This requires keeping the messages in a field rather than a local — refactor `run_subagent` to push into `self.messages` (with interior mutability via `RwLock`). The `TaskToolHandler` calls `snapshot_transcript()` after `run_subagent()` returns, and attaches it to whatever transcript-write mechanism Task 14 establishes.

- [ ] **Step 6: Version-tolerant `load_transcript`**

In `load_transcript`, before parsing, peek at the `version` field:

```rust
    let version = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("version").and_then(|v| v.as_u64()))
        .unwrap_or(0);

    if version == 1 {
        tracing::warn!(
            "loading transcript {:?} written with schema v1; subagent transcripts will be absent",
            path
        );
        // Parse as the new shape with default-empty subagent_transcripts.
        // Use #[serde(default)] on the new fields so v1 documents parse.
    }
```

Add `#[serde(default)]` on every new `TranscriptFile` field. v1 docs round-trip with those fields empty.

- [ ] **Step 7: Run tests**

Run: `cargo test -p savvagent-host transcript`
Expected: All three new tests PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent-host/src/session.rs
git commit -m "feat(host): transcript v2 with nested subagent transcripts, v1-tolerant loader"
```

---

## Phase 4 — user-agents plugin

### Task 15: Plugin scaffold — manifest, Plugin impl, registration

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_agents/mod.rs`
- Modify: `crates/savvagent/src/plugin/builtin/mod.rs`

- [ ] **Step 1: Write the failing test**

In `crates/savvagent/src/plugin/builtin/user_agents/mod.rs`, add at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_id_is_internal_user_agents() {
        let plugin = UserAgentsPlugin::new(PathBuf::from("/tmp"), PathBuf::from("/tmp"));
        assert_eq!(plugin.manifest().id.0, "internal:user-agents");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savvagent --lib user_agents`
Expected: FAIL — module not found.

- [ ] **Step 3: Write the scaffold**

Create `crates/savvagent/src/plugin/builtin/user_agents/mod.rs`:

```rust
//! `internal:user-agents` — discovers user-defined agent definition
//! files and exposes them via an in-process `task` tool. See
//! `docs/superpowers/specs/2026-05-23-user-agents-design.md`.

mod body;
mod discovery;
mod frontmatter;
mod index;
mod spec;
mod task_tool;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, HookKind, Manifest, Plugin, PluginError, PluginId, PluginKind,
    SlashSpec,
};

pub use index::AgentIndex;
pub use spec::AgentSpec;

pub struct UserAgentsPlugin {
    project_root: PathBuf,
    user_home: PathBuf,
    index: AgentIndex,
}

impl UserAgentsPlugin {
    pub fn new(project_root: PathBuf, user_home: PathBuf) -> Self {
        Self {
            project_root,
            user_home,
            index: AgentIndex::empty(),
        }
    }
}

#[async_trait]
impl Plugin for UserAgentsPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: PluginId("internal:user-agents".into()),
            kind: PluginKind::Builtin,
            subscriptions: vec![HookKind::HostStarting],
            contributions: Contributions {
                slash_commands: vec![SlashSpec {
                    name: "reload-agents".into(),
                    description: "Rescan agent definition files and re-register the task tool".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        }
    }

    async fn on_event(
        &mut self,
        event: savvagent_plugin::HostEvent,
    ) -> Result<Vec<Effect>, PluginError> {
        use savvagent_plugin::HostEvent::*;

        match event {
            HostStarting => {
                let entries = discovery::discover(&self.project_root, &self.user_home);
                self.index.replace(entries);
                Ok(self.register_task_tool_effects())
            }
            _ => Ok(vec![]),
        }
    }

    async fn handle_slash(
        &mut self,
        cmd: &str,
        _args: &[String],
    ) -> Result<Vec<Effect>, PluginError> {
        if cmd == "reload-agents" {
            let entries = discovery::discover(&self.project_root, &self.user_home);
            self.index.replace(entries);
            Ok(self.register_task_tool_effects())
        } else {
            Ok(vec![])
        }
    }
}

impl UserAgentsPlugin {
    fn register_task_tool_effects(&self) -> Vec<Effect> {
        if self.index.is_empty() {
            return vec![];
        }
        let spec = task_tool::build_tool_def(&self.index);
        let handler: Arc<dyn savvagent_plugin::InProcessToolHandler> =
            Arc::new(task_tool::TaskToolHandler::new(self.index.clone()));
        vec![Effect::RegisterInProcessTool { spec, handler }]
    }
}
```

Stub the sub-modules so the file compiles. We'll fill them in subsequent tasks. Create empty files:

`crates/savvagent/src/plugin/builtin/user_agents/body.rs`:
```rust
//! `@<path>` include expansion. Implemented in Task 17.
```

`crates/savvagent/src/plugin/builtin/user_agents/discovery.rs`:
```rust
//! Four-path agent discovery. Implemented in Task 18.

use std::path::Path;
use crate::plugin::builtin::user_agents::spec::AgentSpec;

pub fn discover(_project: &Path, _user_home: &Path) -> Vec<AgentSpec> {
    Vec::new()
}
```

`crates/savvagent/src/plugin/builtin/user_agents/frontmatter.rs`:
```rust
//! YAML frontmatter parser. Implemented in Task 16.
```

`crates/savvagent/src/plugin/builtin/user_agents/index.rs`:
```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::plugin::builtin::user_agents::spec::AgentSpec;

#[derive(Clone, Default)]
pub struct AgentIndex {
    inner: Arc<RwLock<HashMap<String, Arc<AgentSpec>>>>,
}

impl AgentIndex {
    pub fn empty() -> Self {
        Self::default()
    }

    pub async fn get(&self, name: &str) -> Option<Arc<AgentSpec>> {
        self.inner.read().await.get(name).cloned()
    }

    pub fn replace(&self, agents: Vec<AgentSpec>) {
        let map: HashMap<String, Arc<AgentSpec>> = agents
            .into_iter()
            .map(|spec| (spec.name.clone(), Arc::new(spec)))
            .collect();
        *self.inner.blocking_write() = map;
    }

    pub fn is_empty(&self) -> bool {
        self.inner
            .try_read()
            .map(|g| g.is_empty())
            .unwrap_or(true)
    }

    pub fn names_snapshot(&self) -> Vec<String> {
        self.inner
            .try_read()
            .map(|g| g.keys().cloned().collect())
            .unwrap_or_default()
    }
}
```

`crates/savvagent/src/plugin/builtin/user_agents/spec.rs`:
```rust
use std::collections::HashSet;

/// In-memory representation of one parsed agent definition file.
#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub name: String,
    pub description: String,
    pub tools: ToolsScope,
    pub model: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolsScope {
    /// `tools:` key absent — inherit parent's full tool set.
    Inherit,
    /// `tools: []` — only the `task` tool available.
    Empty,
    /// Explicit allowlist.
    Allowed(HashSet<String>),
}
```

`crates/savvagent/src/plugin/builtin/user_agents/task_tool.rs`:
```rust
//! `task` in-process tool handler. Implemented in Task 21.

use std::sync::Arc;
use async_trait::async_trait;
use savvagent_plugin::InProcessToolHandler;
use savvagent_protocol::ToolDef;
use serde_json::Value;

use crate::plugin::builtin::user_agents::index::AgentIndex;

pub struct TaskToolHandler {
    _index: AgentIndex,
}

impl TaskToolHandler {
    pub fn new(index: AgentIndex) -> Self {
        Self { _index: index }
    }
}

#[async_trait]
impl InProcessToolHandler for TaskToolHandler {
    async fn call(
        &self,
        _input: Value,
        _ctx: Arc<dyn std::any::Any + Send + Sync>,
    ) -> Result<Value, String> {
        Err("task tool not yet implemented".into())
    }
}

pub fn build_tool_def(index: &AgentIndex) -> ToolDef {
    let names = index.names_snapshot();
    ToolDef {
        name: "task".into(),
        description: "Spawn a subagent to handle a focused task. Returns the subagent's final response.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["description", "prompt", "subagent_type"],
            "properties": {
                "description": { "type": "string" },
                "prompt": { "type": "string" },
                "subagent_type": { "type": "string", "enum": names }
            }
        }),
    }
}
```

- [ ] **Step 4: Register the plugin in builtin/mod.rs**

In `crates/savvagent/src/plugin/builtin/mod.rs`, find the existing list where plugins are added and append:

```rust
pub mod user_agents;
```

And in the function that builds the plugin list (look for where `user_slash_commands`, `user_hooks`, etc. are added), append:

```rust
    plugins.push(Box::new(user_agents::UserAgentsPlugin::new(
        project_root.clone(),
        user_home.clone(),
    )));
```

- [ ] **Step 5: Run the test**

Run: `cargo test -p savvagent --lib user_agents`
Expected: PASS.

- [ ] **Step 6: Build the whole workspace**

Run: `cargo build`
Expected: Compiles. (TUI requires `savvagent-tool-fs` binary at runtime, but `cargo build` doesn't require it.)

- [ ] **Step 7: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/
git commit -m "feat(plugin/user-agents): scaffold internal:user-agents plugin"
```

---

### Task 16: Frontmatter parser

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_agents/frontmatter.rs`

- [ ] **Step 1: Write the failing tests**

Replace the placeholder in `frontmatter.rs` with:

```rust
//! YAML frontmatter parser for agent definition files.

use std::collections::HashSet;

use serde::Deserialize;

use crate::plugin::builtin::user_agents::spec::{AgentSpec, ToolsScope};

#[derive(Debug)]
pub struct FrontmatterResult {
    pub spec: AgentSpec,
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawFrontmatter {
    name: Option<String>,
    description: Option<String>,
    tools: Option<ToolsField>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ToolsField {
    Str(String),
    List(Vec<String>),
}

pub fn parse(raw: &str, filename_slug: &str) -> Result<FrontmatterResult, String> {
    // Splits frontmatter block from body, parses YAML, validates,
    // returns AgentSpec.
    // ... implementation in step 3
    let _ = (raw, filename_slug);
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "---\nname: code-reviewer\ndescription: Reviews diffs.\ntools: tool-fs:read_file, tool-grep:search\nmodel: claude-sonnet-4-6\n---\nYou are a reviewer.";

    #[test]
    fn parses_full_frontmatter() {
        let r = parse(FULL, "code-reviewer").expect("parse");
        assert_eq!(r.spec.name, "code-reviewer");
        assert_eq!(r.spec.description, "Reviews diffs.");
        assert_eq!(r.spec.model.as_deref(), Some("claude-sonnet-4-6"));
        assert!(matches!(r.spec.tools, ToolsScope::Allowed(_)));
        assert_eq!(r.spec.body.trim(), "You are a reviewer.");
    }

    #[test]
    fn tools_as_yaml_list() {
        let raw = "---\ndescription: x\ntools:\n  - tool-fs:read_file\n  - tool-grep:search\n---\nbody";
        let r = parse(raw, "agent").expect("parse");
        match r.spec.tools {
            ToolsScope::Allowed(set) => {
                assert!(set.contains("tool-fs:read_file"));
                assert!(set.contains("tool-grep:search"));
            }
            _ => panic!("expected Allowed"),
        }
    }

    #[test]
    fn empty_tools_list_is_empty_scope() {
        let raw = "---\ndescription: x\ntools: []\n---\nbody";
        let r = parse(raw, "agent").expect("parse");
        assert_eq!(r.spec.tools, ToolsScope::Empty);
    }

    #[test]
    fn missing_tools_is_inherit() {
        let raw = "---\ndescription: x\n---\nbody";
        let r = parse(raw, "agent").expect("parse");
        assert_eq!(r.spec.tools, ToolsScope::Inherit);
    }

    #[test]
    fn missing_description_fails() {
        let raw = "---\nname: x\n---\nbody";
        let err = parse(raw, "agent").unwrap_err();
        assert!(err.contains("description"));
    }

    #[test]
    fn empty_body_fails() {
        let raw = "---\ndescription: x\n---\n";
        let err = parse(raw, "agent").unwrap_err();
        assert!(err.contains("body"));
    }

    #[test]
    fn name_mismatch_warns_but_filename_wins() {
        let raw = "---\nname: mismatched\ndescription: x\n---\nbody";
        let r = parse(raw, "agent").expect("parse");
        assert_eq!(r.spec.name, "agent");
        assert!(r.warnings.iter().any(|w| w.contains("name")));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p savvagent --lib frontmatter`
Expected: FAIL — `todo!()` panic.

- [ ] **Step 3: Implement `parse`**

Replace the `todo!()` body with:

```rust
pub fn parse(raw: &str, filename_slug: &str) -> Result<FrontmatterResult, String> {
    let (front, body) = split_frontmatter(raw)?;
    let raw_front: RawFrontmatter = serde_yaml_ng::from_str(front)
        .map_err(|e| format!("malformed frontmatter: {e}"))?;

    let mut warnings = Vec::new();

    let description = raw_front
        .description
        .ok_or_else(|| "missing required field: description".to_string())?;

    if body.trim().is_empty() {
        return Err("empty body".into());
    }

    let name = match raw_front.name {
        Some(n) if n != filename_slug => {
            warnings.push(format!(
                "frontmatter name `{n}` disagrees with filename slug `{filename_slug}`; filename wins"
            ));
            filename_slug.to_string()
        }
        _ => filename_slug.to_string(),
    };

    let tools = match raw_front.tools {
        None => ToolsScope::Inherit,
        Some(ToolsField::List(list)) if list.is_empty() => ToolsScope::Empty,
        Some(ToolsField::List(list)) => ToolsScope::Allowed(list.into_iter().collect()),
        Some(ToolsField::Str(s)) => {
            let set: HashSet<String> = s
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            if set.is_empty() {
                ToolsScope::Empty
            } else {
                ToolsScope::Allowed(set)
            }
        }
    };

    Ok(FrontmatterResult {
        spec: AgentSpec {
            name,
            description,
            tools,
            model: raw_front.model,
            body: body.to_string(),
        },
        warnings,
    })
}

fn split_frontmatter(raw: &str) -> Result<(&str, &str), String> {
    if !raw.starts_with("---") {
        return Err("no frontmatter delimiter".into());
    }
    let rest = &raw[3..];
    let Some(end) = rest.find("\n---") else {
        return Err("unterminated frontmatter".into());
    };
    let front = &rest[..end];
    let body = &rest[end + 4..]; // skip "\n---"
    let body = body.strip_prefix('\n').unwrap_or(body);
    Ok((front, body))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p savvagent --lib frontmatter`
Expected: All tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_agents/frontmatter.rs
git commit -m "feat(plugin/user-agents): YAML frontmatter parser"
```

---

### Task 17: `@<path>` body include expansion

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_agents/body.rs`

Mirror sub-project A's `@<path>` semantics: a single-pass include at load time. `@<path>` lines (with path relative to the agent file or absolute) get substituted with the file contents. Missing files leave the literal `@<path>` in place and emit a warning.

- [ ] **Step 1: Write the failing tests**

Replace `body.rs` content:

```rust
//! Expands `@<path>` includes in agent body text at load time.
//!
//! Single-pass: an included file containing `@<other>` is NOT
//! recursively expanded. Missing files leave the literal `@<path>`
//! in place and emit a warning.

use std::path::Path;

pub struct BodyResult {
    pub body: String,
    pub warnings: Vec<String>,
}

pub fn expand(body: &str, base_dir: &Path) -> BodyResult {
    let mut out = String::with_capacity(body.len());
    let mut warnings = Vec::new();

    for line in body.lines() {
        if let Some(rest) = line.strip_prefix('@') {
            let path = rest.trim();
            if path.is_empty() {
                out.push_str(line);
                out.push('\n');
                continue;
            }
            let resolved = if Path::new(path).is_absolute() {
                Path::new(path).to_path_buf()
            } else {
                base_dir.join(path)
            };
            match std::fs::read_to_string(&resolved) {
                Ok(contents) => {
                    out.push_str(&contents);
                    if !contents.ends_with('\n') {
                        out.push('\n');
                    }
                }
                Err(e) => {
                    warnings.push(format!("@{path}: {e}"));
                    out.push_str(line);
                    out.push('\n');
                }
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }

    BodyResult { body: out, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn no_includes_passthrough() {
        let r = expand("hello world\n", &std::env::temp_dir());
        assert_eq!(r.body.trim(), "hello world");
    }

    #[test]
    fn expands_relative_path() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("snippet.md"), "INCLUDED").unwrap();
        let body = "intro\n@snippet.md\noutro";
        let r = expand(body, dir.path());
        assert!(r.body.contains("INCLUDED"));
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn missing_file_warns_and_keeps_literal() {
        let dir = tempdir().unwrap();
        let r = expand("@nonexistent.md", dir.path());
        assert!(r.body.contains("@nonexistent.md"));
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn single_pass_no_recursive_expansion() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "@b.md").unwrap();
        fs::write(dir.path().join("b.md"), "FINAL").unwrap();
        let r = expand("@a.md", dir.path());
        // a.md's contents include literal "@b.md" — should NOT expand.
        assert!(r.body.contains("@b.md"));
        assert!(!r.body.contains("FINAL"));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p savvagent --lib body`
Expected: All PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_agents/body.rs
git commit -m "feat(plugin/user-agents): @<path> include expansion at load time"
```

---

### Task 18: Four-path discovery

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_agents/discovery.rs`

- [ ] **Step 1: Write the failing tests**

Replace `discovery.rs` with:

```rust
//! Four-path discovery for agent definition files. Mirrors sub-project A
//! and B precedence: project beats user, savvagent beats claude.

use std::path::{Path, PathBuf};

use crate::plugin::builtin::user_agents::body::expand;
use crate::plugin::builtin::user_agents::frontmatter::parse;
use crate::plugin::builtin::user_agents::spec::AgentSpec;

/// Discover agent definitions across the four standard paths.
pub fn discover(project_root: &Path, user_home: &Path) -> Vec<AgentSpec> {
    let paths = [
        project_root.join(".savvagent").join("agents"),
        project_root.join(".claude").join("agents"),
        user_home.join(".savvagent").join("agents"),
        user_home.join(".claude").join("agents"),
    ];

    let mut out: Vec<AgentSpec> = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = Default::default();

    for dir in &paths {
        let walker = walk_dir(dir);
        for path in walker {
            let Some(slug) = slug_from_path(&path) else { continue };
            if !seen_names.insert(slug.clone()) {
                // First-wins by path precedence.
                continue;
            }
            match load_agent(&path, &slug) {
                Ok(spec) => out.push(spec),
                Err(e) => {
                    tracing::warn!("agent {path:?} skipped: {e}");
                }
            }
        }
    }

    out
}

fn walk_dir(dir: &Path) -> Vec<PathBuf> {
    use ignore::WalkBuilder;
    let mut out = Vec::new();
    if !dir.exists() {
        return out;
    }
    for entry in WalkBuilder::new(dir).build().flatten() {
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false)
            && entry.path().extension().and_then(|s| s.to_str()) == Some("md")
        {
            out.push(entry.into_path());
        }
    }
    out
}

fn slug_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem().and_then(|s| s.to_str())?;
    if stem.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        Some(stem.to_string())
    } else {
        tracing::warn!("agent {path:?} skipped: invalid slug `{stem}` (must be lowercase-kebab-case)");
        None
    }
}

fn load_agent(path: &Path, slug: &str) -> Result<AgentSpec, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let result = parse(&raw, slug)?;
    for w in &result.warnings {
        tracing::warn!("agent {path:?}: {w}");
    }
    let base = path.parent().unwrap_or(Path::new("."));
    let expanded = expand(&result.spec.body, base);
    for w in &expanded.warnings {
        tracing::warn!("agent {path:?}: {w}");
    }
    let mut spec = result.spec;
    spec.body = expanded.body;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_agent(dir: &Path, slug: &str, body: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(format!("{slug}.md")), body).unwrap();
    }

    const MINIMAL: &str = "---\ndescription: test agent\n---\nbody";

    #[test]
    fn precedence_project_savvagent_beats_user_claude() {
        let project = tempdir().unwrap();
        let user = tempdir().unwrap();
        write_agent(
            &project.path().join(".savvagent/agents"),
            "shared",
            "---\ndescription: project version\n---\nproject body",
        );
        write_agent(
            &user.path().join(".claude/agents"),
            "shared",
            "---\ndescription: user version\n---\nuser body",
        );
        let agents = discover(project.path(), user.path());
        assert_eq!(agents.len(), 1);
        assert!(agents[0].body.contains("project body"));
    }

    #[test]
    fn malformed_file_skipped_gracefully() {
        let project = tempdir().unwrap();
        let user = tempdir().unwrap();
        write_agent(
            &project.path().join(".savvagent/agents"),
            "bad",
            "not even close to YAML",
        );
        write_agent(
            &project.path().join(".savvagent/agents"),
            "good",
            MINIMAL,
        );
        let agents = discover(project.path(), user.path());
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"good"));
        assert!(!names.contains(&"bad"));
    }

    #[test]
    fn invalid_slug_skipped() {
        let project = tempdir().unwrap();
        let user = tempdir().unwrap();
        write_agent(
            &project.path().join(".savvagent/agents"),
            "BadCaps",
            MINIMAL,
        );
        let agents = discover(project.path(), user.path());
        assert!(agents.is_empty());
    }

    #[test]
    fn nonexistent_dirs_ok() {
        let project = tempdir().unwrap();
        let user = tempdir().unwrap();
        let agents = discover(project.path(), user.path());
        assert!(agents.is_empty());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p savvagent --lib discovery`
Expected: All PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_agents/discovery.rs
git commit -m "feat(plugin/user-agents): four-path discovery with precedence + slug validation"
```

---

### Task 19: `AgentIndex` async-friendly wrapper

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_agents/index.rs`

Refactor: the scaffold version used `blocking_write` / `try_read` to keep the API sync. The plugin hot path is async; switch to async methods.

- [ ] **Step 1: Write the failing test**

Append to `index.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::builtin::user_agents::spec::ToolsScope;

    fn agent(name: &str) -> AgentSpec {
        AgentSpec {
            name: name.into(),
            description: format!("{name} agent"),
            tools: ToolsScope::Inherit,
            model: None,
            body: format!("you are {name}"),
        }
    }

    #[tokio::test]
    async fn replace_makes_agents_visible() {
        let index = AgentIndex::empty();
        index.replace(vec![agent("a"), agent("b")]).await;
        assert_eq!(index.len().await, 2);
        let a = index.get("a").await.expect("agent a");
        assert_eq!(a.description, "a agent");
    }

    #[tokio::test]
    async fn names_snapshot_returns_sorted_list() {
        let index = AgentIndex::empty();
        index.replace(vec![agent("b"), agent("a"), agent("c")]).await;
        let names = index.names_snapshot().await;
        assert_eq!(names, vec!["a", "b", "c"]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p savvagent --lib user_agents::index`
Expected: FAIL — `replace` is sync, `len` doesn't exist.

- [ ] **Step 3: Refactor `AgentIndex`**

Replace `index.rs` content:

```rust
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::plugin::builtin::user_agents::spec::AgentSpec;

#[derive(Clone, Default)]
pub struct AgentIndex {
    inner: Arc<RwLock<HashMap<String, Arc<AgentSpec>>>>,
}

impl AgentIndex {
    pub fn empty() -> Self {
        Self::default()
    }

    pub async fn replace(&self, agents: Vec<AgentSpec>) {
        let map: HashMap<String, Arc<AgentSpec>> = agents
            .into_iter()
            .map(|spec| (spec.name.clone(), Arc::new(spec)))
            .collect();
        *self.inner.write().await = map;
    }

    pub async fn get(&self, name: &str) -> Option<Arc<AgentSpec>> {
        self.inner.read().await.get(name).cloned()
    }

    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }

    pub async fn names_snapshot(&self) -> Vec<String> {
        let mut names: Vec<String> = self.inner.read().await.keys().cloned().collect();
        names.sort();
        names
    }
}
```

- [ ] **Step 4: Update callers in `user_agents/mod.rs`**

The `register_task_tool_effects` and `on_event` paths now need async. Refactor:

```rust
    async fn register_task_tool_effects(&self) -> Vec<Effect> {
        if self.index.is_empty().await {
            return vec![];
        }
        let spec = task_tool::build_tool_def(&self.index).await;
        let handler: Arc<dyn savvagent_plugin::InProcessToolHandler> =
            Arc::new(task_tool::TaskToolHandler::new(self.index.clone()));
        vec![Effect::RegisterInProcessTool { spec, handler }]
    }
```

And update `on_event`:

```rust
            HostStarting => {
                let entries = discovery::discover(&self.project_root, &self.user_home);
                self.index.replace(entries).await;
                Ok(self.register_task_tool_effects().await)
            }
```

Update `handle_slash` similarly. Update `task_tool::build_tool_def` to be async and `.await` `names_snapshot()`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p savvagent --lib user_agents`
Expected: All PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_agents/
git commit -m "feat(plugin/user-agents): async AgentIndex; rewire register_task_tool_effects"
```

---

### Task 20: `task` tool — the handler that builds and drives a SubHost

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_agents/task_tool.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent/tests/user_agents_task_tool.rs`:

```rust
//! End-to-end: the `task` in-process tool resolves a subagent, builds
//! a SubHost, and returns the subagent's final text wrapped as a
//! ToolResult.
//!
//! Uses a stub provider that emits a fixed end_turn text. Asserts
//! the JSON returned by `TaskToolHandler::call` is exactly that text.

// Test body uses the same Host + stub provider fixtures as
// crates/savvagent-host/tests/subhost_basic.rs.
```

Implement against the existing test fixtures.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savvagent --test user_agents_task_tool`
Expected: FAIL.

- [ ] **Step 3: Implement `TaskToolHandler::call`**

Replace `task_tool.rs` with:

```rust
//! `task` in-process tool handler.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::{SubHost, SubagentContext, ToolCallContext};
use savvagent_plugin::InProcessToolHandler;
use savvagent_protocol::ToolDef;
use serde::Deserialize;
use serde_json::Value;

use crate::plugin::builtin::user_agents::index::AgentIndex;
use crate::plugin::builtin::user_agents::spec::ToolsScope;

#[derive(Deserialize)]
struct TaskInput {
    description: String,
    prompt: String,
    subagent_type: String,
}

pub struct TaskToolHandler {
    index: AgentIndex,
}

impl TaskToolHandler {
    pub fn new(index: AgentIndex) -> Self {
        Self { index }
    }
}

#[async_trait]
impl InProcessToolHandler for TaskToolHandler {
    async fn call(
        &self,
        input: Value,
        ctx: Arc<dyn std::any::Any + Send + Sync>,
    ) -> Result<Value, String> {
        let input: TaskInput = serde_json::from_value(input)
            .map_err(|e| format!("task: invalid input: {e}"))?;

        let tool_ctx = ctx
            .downcast_ref::<ToolCallContext>()
            .ok_or_else(|| "task: ToolCallContext missing".to_string())?;

        let spec = self
            .index
            .get(&input.subagent_type)
            .await
            .ok_or_else(|| format!("unknown subagent_type: {}", input.subagent_type))?;

        let parent_depth = tool_ctx.subagent.as_ref().map(|s| s.depth).unwrap_or(0);
        let next_depth = parent_depth + 1;

        let parent_session_id = tool_ctx
            .subagent
            .as_ref()
            .map(|s| s.parent_session_id.clone())
            .unwrap_or_else(|| tool_ctx.host.session_id());

        let sub_ctx = SubagentContext {
            depth: next_depth,
            agent_name: spec.name.clone(),
            parent_session_id,
        };

        // Build the tool_defs + allowlist
        let parent_defs = tool_ctx.host.tool_registry().tool_defs().await;
        let (allowed, defs) = filter_tools(&spec.tools, &parent_defs, next_depth);

        let cancellation = tool_ctx.cancellation.child_token();

        let sub = SubHost::new(
            tool_ctx.host.clone(),
            sub_ctx,
            spec.body.clone(),
            spec.model.clone(),
            allowed,
            defs,
            cancellation,
        )
        .map_err(|e| format!("task: {e}"))?;

        let _label = input.description; // currently unused; surfaced by the TUI in Task 24

        match sub.run_subagent(input.prompt).await {
            Ok(text) => Ok(Value::String(text)),
            Err(e) => Err(format!("subagent {}: {e}", input.subagent_type)),
        }
    }
}

fn filter_tools(
    scope: &ToolsScope,
    parent: &[ToolDef],
    depth: u8,
) -> (HashSet<String>, Vec<ToolDef>) {
    let max_depth = savvagent_host::subhost::max_depth_from_env();
    let include_task = depth < max_depth;

    match scope {
        ToolsScope::Inherit => {
            let allowed: HashSet<String> = parent
                .iter()
                .filter(|d| include_task || d.name != "task")
                .map(|d| d.name.clone())
                .collect();
            let defs = parent
                .iter()
                .filter(|d| include_task || d.name != "task")
                .cloned()
                .collect();
            (allowed, defs)
        }
        ToolsScope::Empty => {
            let mut allowed = HashSet::new();
            let mut defs = Vec::new();
            if include_task {
                allowed.insert("task".into());
                if let Some(t) = parent.iter().find(|d| d.name == "task") {
                    defs.push(t.clone());
                }
            }
            (allowed, defs)
        }
        ToolsScope::Allowed(set) => {
            let mut allowed: HashSet<String> = set.iter().cloned().collect();
            if include_task {
                allowed.insert("task".into());
            }
            let defs = parent
                .iter()
                .filter(|d| allowed.contains(&d.name))
                .cloned()
                .collect();
            (allowed, defs)
        }
    }
}

pub async fn build_tool_def(index: &AgentIndex) -> ToolDef {
    let names = index.names_snapshot().await;
    ToolDef {
        name: "task".into(),
        description: "Spawn a subagent to handle a focused task. Returns the subagent's final response.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["description", "prompt", "subagent_type"],
            "properties": {
                "description": { "type": "string", "description": "3-5 word task label" },
                "prompt": { "type": "string", "description": "The task for the subagent" },
                "subagent_type": { "type": "string", "enum": names }
            }
        }),
    }
}
```

This requires `Host::session_id()` accessor and `Host::tool_registry()` accessor — add if missing in `session.rs`:

```rust
    pub fn session_id(&self) -> String {
        self.session_id.clone()
    }

    pub fn tool_registry(&self) -> &Arc<crate::tools::ToolRegistry> {
        &self.tools
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p savvagent --test user_agents_task_tool`
Run: `cargo build`
Expected: PASS / compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_agents/task_tool.rs crates/savvagent-host/src/session.rs crates/savvagent/tests/user_agents_task_tool.rs
git commit -m "feat(plugin/user-agents): task tool handler — builds and drives SubHost"
```

---

### Task 21: Wire `Effect::RegisterInProcessTool` handling in the TUI app

**Files:**
- Modify: `crates/savvagent/src/app.rs`

When a plugin emits `RegisterInProcessTool`, the app must register it on the active `Host`. The existing pattern for `RegisterProvider` is the closest analog.

- [ ] **Step 1: Find the existing Effect handler**

`grep -n "Effect::RegisterProvider\|Effect::RegisterPreToolGate" crates/savvagent/src/app.rs`

Locate the `match effect { ... }` block that handles plugin effects.

- [ ] **Step 2: Add the new arm**

```rust
                Effect::RegisterInProcessTool { spec, handler } => {
                    if let Some(host) = host_arc.read().await.as_ref().cloned() {
                        host.tool_registry()
                            .register_in_process_tool(spec, handler)
                            .await;
                    } else {
                        tracing::warn!(
                            "RegisterInProcessTool emitted before host connected; ignoring"
                        );
                    }
                }
```

The `host_arc` name is whatever the existing code uses; mirror.

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: Compiles.

- [ ] **Step 4: Smoke test — verify `task` tool appears in tool_defs**

Run the TUI smoke test that already exists for tool registration, or add a small one:

```rust
// crates/savvagent/tests/user_agents_e2e.rs
#[tokio::test]
async fn task_tool_registered_when_agents_present() {
    // Build app with a project root containing a single agent file.
    // After HostStarting fires, host.tool_registry().tool_defs() must
    // include a ToolDef with name "task".
}
```

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/app.rs crates/savvagent/tests/user_agents_e2e.rs
git commit -m "feat(app): handle Effect::RegisterInProcessTool"
```

---

## Phase 5 — TUI surface

### Task 22: Render `task` tool calls as collapsible blocks

**Files:**
- Modify: `crates/savvagent/src/tui.rs` (or wherever conversation-log rendering is split — read the file to confirm)
- Likely create: `crates/savvagent/src/plugin/builtin/tool_task_summary/` (mirror existing `tool_fs_summary`, `tool_grep_summary`)

Sub-project A's tool-call rendering work introduced per-tool summary plugins. Mirror that pattern for the `task` tool.

- [ ] **Step 1: Find the existing tool-call summary pattern**

`ls crates/savvagent/src/plugin/builtin/tool_*_summary/`
Read one (e.g. `tool_fs_summary/mod.rs`) to understand the contract.

- [ ] **Step 2: Add `tool_task_summary`**

Create `crates/savvagent/src/plugin/builtin/tool_task_summary/mod.rs` following the existing pattern. The summary should render:
- Collapsed: `task <agent_name> · "<description>"`
- Expanded: streaming text + nested tool calls

If the existing summary plugins are passive (just provide a one-line collapsed view) and the expansion is universal, the work here is just the one-line summary builder. Verify that.

- [ ] **Step 3: Register the new summary plugin**

In `crates/savvagent/src/plugin/builtin/mod.rs`, append `pub mod tool_task_summary;` and push it into the plugin list.

- [ ] **Step 4: Add expansion routing for `subagent_transcript`**

The transcript v2 schema (Task 14) stores the subagent's messages under the parent's `task` tool-call entry. The conversation-log renderer should detect a `task` tool call and render the embedded subagent transcript as a nested block.

The exact integration depends on how the existing rendering deals with `ContentBlock::ToolUse` and `ContentBlock::ToolResult`. Read the renderer to identify the point where it would benefit from a "subagent transcript expansion" branch. Add that branch.

- [ ] **Step 5: Smoke test**

Build the TUI:
```bash
cargo build -p savvagent
```

Run it interactively against a project with one agent file (manual smoke; not a unit test):

```bash
mkdir -p /tmp/savvagent-c-smoke/.savvagent/agents
cat > /tmp/savvagent-c-smoke/.savvagent/agents/echo.md <<'EOF'
---
description: Returns the prompt verbatim, capitalized.
---
Return exactly the prompt the user gave, capitalized. No other text.
EOF

cd /tmp/savvagent-c-smoke
cargo run -p savvagent
```

In the TUI, ask: "Use the task tool with subagent_type=echo and prompt='hello world'". Confirm a collapsible block appears, expanded while running, collapsed on completion.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/tool_task_summary/ crates/savvagent/src/plugin/builtin/mod.rs crates/savvagent/src/tui.rs
git commit -m "feat(tui): collapsible task-call block + subagent transcript expansion"
```

---

### Task 23: Subagent streaming via private channel

**Files:**
- Modify: `crates/savvagent-host/src/subhost.rs`
- Modify: `crates/savvagent/src/tui.rs` or app.rs

If you want **live** content streaming inside the collapsible block (versus just status), add a `SubagentStreamEvent` enum and a `tokio::sync::mpsc::Sender<SubagentStreamEvent>` parameter to `SubHost::new` / `run_subagent`.

- [ ] **Step 1: Define `SubagentStreamEvent`**

In `subhost.rs`:

```rust
#[derive(Debug, Clone)]
pub enum SubagentStreamEvent {
    /// New text fragment from the subagent's provider.
    TextDelta { block_id: String, fragment: String },
    /// Subagent issued a tool call.
    ToolCall { block_id: String, name: String, input: serde_json::Value },
    /// Tool call completed.
    ToolResult { block_id: String, name: String, success: bool },
    /// Subagent reached end_turn with final text.
    Completed { block_id: String, text: String },
    /// Subagent failed.
    Failed { block_id: String, error: String },
}
```

- [ ] **Step 2: Add an mpsc sender field to `SubHost`**

```rust
pub struct SubHost {
    // ... existing fields ...
    events: Option<tokio::sync::mpsc::Sender<SubagentStreamEvent>>,
    block_id: String,
}
```

`block_id` is a UUID generated at construction (`uuid::Uuid::new_v4().to_string()`).

`SubHost::new` gains a new parameter `events: Option<Sender<...>>`. Callers in tests pass `None`; the `TaskToolHandler` (Task 20) passes a Some — wired via `ToolCallContext`.

- [ ] **Step 3: Emit events from the loop**

In `run_subagent`'s `match resp.stop_reason` arms, emit appropriately. For text deltas, the simplest version emits one `Completed` event with the final text (sub-second response). For real streaming, wire `provider.complete_stream` if available — defer until users complain.

- [ ] **Step 4: Wire the TUI to receive**

In the TUI's main loop, accept a new `mpsc::Receiver<SubagentStreamEvent>` and route updates to the collapsible block matching `block_id`.

- [ ] **Step 5: Smoke test**

Same as Task 22 — confirm live updates appear in the block.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/subhost.rs crates/savvagent/src/{tui.rs,app.rs}
git commit -m "feat: SubagentStreamEvent channel + TUI live updates"
```

---

## Phase 6 — Docs and release prep

### Task 24: README — User-defined agents section

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Find the existing "User-defined slash commands" section**

`grep -n "User-defined slash commands\|User-defined hooks" README.md`

Locate the TUI features region. Add a new section after the hooks section:

```markdown
### User-defined agents

Drop markdown files into `.savvagent/agents/` (project) or `~/.savvagent/agents/` (user)
to make them available as subagents the model can spawn via the built-in `task` tool.
`.claude/agents/` is also supported for drop-in compatibility with existing Claude Code
agent libraries.

Example `~/.savvagent/agents/code-reviewer.md`:

\`\`\`markdown
---
description: Reviews staged diffs for correctness bugs. Use after writing code, before commit.
tools: tool-fs:read_file, tool-fs:glob, tool-grep:search
model: claude-sonnet-4-6
---

You are a senior code reviewer. When invoked, ...
\`\`\`

Frontmatter keys:

| Key | Required | Purpose |
|---|---|---|
| `description` | yes | Tells the parent model when to pick this agent (shown in the `task` tool's `subagent_type` enum) |
| `tools` | no | Comma-separated or YAML list of fully-qualified tool names (e.g. `tool-fs:read_file`). Omit to inherit the parent's full tool set. `[]` means only `task` is available. Defends against the model fabricating tool names |
| `model` | no | Per-agent model override; falls back to the active model |
| `name` | no | Defaults to filename slug; warn-log if mismatched |

Agent body files may use `@<path>` to inline another file at load time (single-pass, no recursion).

Reload agents at runtime: `/reload-agents`.

Subagent depth cap: `SAVVAGENT_AGENT_MAX_DEPTH=3` (env-configurable).
```

- [ ] **Step 2: Update the on-disk paths reference**

Find the existing "On-disk paths" section and add:

```markdown
- `<project>/.savvagent/agents/**.md` and `~/.savvagent/agents/**.md` — user-defined subagent definitions
- `.claude/agents/` — Claude-Code compat path, same shape
```

- [ ] **Step 3: Update the tool list**

Find the existing tool list and add:

```markdown
- `task` — spawn a subagent (only registered when ≥1 agent file is discovered). See "User-defined agents"
```

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs(readme): user-defined agents section + on-disk paths + task tool"
```

---

### Task 25: PRD — goals and non-goals updates

**Files:**
- Modify: `PRD.md`

- [ ] **Step 1: Update §3 Goals**

Find the existing goals list and add:

```markdown
- **User-defined subagents.** Users drop markdown files into `.savvagent/agents/` (or `.claude/agents/` for compat) to make them available as subagents the model can spawn via the built-in `task` tool, with per-agent tool scoping and optional model override.
```

- [ ] **Step 2: Update §4 Non-goals**

Add:

```markdown
- **Per-agent provider override.** Subagents run against the parent's active provider. Cross-provider subagent routing is a future concern.
- **Parallel subagent fan-out.** The parent's tool-use loop is sequential; multiple `task` calls in a single round run one at a time.
- **Subagent-only cancellation.** Esc cancels the whole parent turn (and all in-flight subagents). No UI for cancelling just one subagent.
```

- [ ] **Step 3: Commit**

```bash
git add PRD.md
git commit -m "docs(prd): add user-defined subagents to goals + v1 non-goals"
```

---

### Task 26: CHANGELOG entry

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Add a new version section**

At the top of the file (under any `[Unreleased]` heading, or as a new heading dated today):

```markdown
## v0.17.0 — 2026-05-XX

### Added

- **User-defined agents** (sub-project C of the Claude-Code / OpenCode parity rollup).
  Drop markdown files into `.savvagent/agents/`, `.claude/agents/`, `~/.savvagent/agents/`,
  or `~/.claude/agents/` to expose them as subagents the model can spawn via a new
  built-in `task` tool. Each agent file carries Claude-Code-compatible frontmatter
  (`description`, optional `tools`, optional `model`); its body becomes the subagent's
  system prompt. `@<path>` includes are expanded at load time.
- `task` in-process tool — registered only when ≥1 agent is discovered. The
  `subagent_type` enum is populated from the discovered set and refreshed by
  `/reload-agents`.
- Sub-Host runtime — subagent turns get their own session state, system prompt,
  model selection, and tool view, while sharing the parent's `ProviderClient`,
  `ToolRegistry`, `PreToolUseGate`, permissions, and sandbox.
- Tool scoping: two-layer enforcement (provider-boundary filter + `ScopedToolRegistry`
  runtime gate) with exact-name matching.
- `HookKind::SubagentStop` event — fires after each subagent reaches `end_turn`,
  with `stop_hook_active` loop guard and a `subagent` field in the stdin payload.
- PreToolUse / PostToolUse stdin payloads gain an optional `subagent` field
  (backward-compatible: absent for parent-turn calls).
- Transcript schema v2 — embeds nested subagent transcripts under the parent's
  `task` tool-call entry. v1 transcripts still load (with a one-time warn-log).
- `SAVVAGENT_AGENT_MAX_DEPTH` env var (default 3) caps subagent recursion.

### Changed

- `Effect` gains `RegisterInProcessTool` variant (savvagent-internal; not part of
  the WIT-portable plugin surface).
- `ToolRegistry` accepts in-process tool handlers via
  `register_in_process_tool` alongside its existing stdio children.

### Migration

- Transcripts from earlier versions load cleanly with empty subagent sections.
  No action required.
- Existing `.claude/agents/*.md` files are picked up automatically. Project
  files outrank user files; `.savvagent/` outranks `.claude/`.
```

- [ ] **Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs(changelog): v0.17.0 — user-defined agents"
```

---

### Task 27: Version bump in workspace + dependencies

**Files:**
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Bump `[workspace.package].version`**

Find:

```toml
[workspace.package]
version = "0.16.1"
```

(Or whatever the current value is. The exact number depends on B's release decision — per [[feedback_phase_release_rollup]] the actual tag is decided at merge time. Use 0.17.0 if B shipped as 0.16.x, otherwise the next-minor-after-B.)

Change to:

```toml
[workspace.package]
version = "0.17.0"
```

- [ ] **Step 2: Mirror into `[workspace.dependencies]` literals**

Find lines like:

```toml
savvagent-protocol = { path = "crates/savvagent-protocol", version = "0.16.1" }
```

Bump every `version` literal in `[workspace.dependencies]` to match. The memory note [[feedback_semver]] is explicit about this.

- [ ] **Step 3: Verify**

Run: `cargo build`
Expected: Compiles. (Cargo will complain if a literal disagrees.)

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml
git commit -m "release(0.17.0): user-defined agents (sub-project C)"
```

---

### Task 28: Final sweep — fmt, clippy, full test

**Files:** —

- [ ] **Step 1: Run rustfmt with stable toolchain**

Per [[feedback_match_ci_toolchain_locally]]:

```bash
rustup run stable cargo fmt --check
```

If anything is unformatted:

```bash
rustup run stable cargo fmt
git add -A
git commit -m "style: rustfmt"
```

- [ ] **Step 2: Clippy across the workspace**

```bash
rustup run stable cargo clippy --workspace --all-targets -- -D warnings
```

Fix anything that comes up. Common patterns in this repo (per [[feedback_dead_code_in_binary_crate]]): `pub` items in the binary crate need a non-test consumer or `#[allow(dead_code)]`.

- [ ] **Step 3: Full test sweep**

```bash
cargo test --workspace
```

All tests PASS.

- [ ] **Step 4: Headless smoke**

If providers are configured locally:

```bash
cargo run -p savvagent-host --example headless -- "Use the task tool with subagent_type=<some-discovered-agent> and prompt='hello'"
```

Verify the subagent runs and the final text comes back.

- [ ] **Step 5: Push**

Per memory [[feedback_verify_ci_after_push.md]]: after pushing, verify CI green via `gh run watch` or equivalent. Do not say "push is good" until that's confirmed.

```bash
git push -u origin worktree-user-agents
gh pr create --base master --title "feat: user-defined agents (sub-project C)" --body "$(cat <<'EOF'
## Summary

Third of four sub-projects toward Claude-Code / OpenCode parity. Adds user-defined agents discoverable from `.savvagent/agents/` and `.claude/agents/`, plus a built-in `task` tool the parent model uses to spawn subagents.

- Sub-Host runtime with own session state, shared provider/registry/gate
- Two-layer tool scoping (provider-boundary filter + ScopedToolRegistry)
- `HookKind::SubagentStop` lights up + `subagent` field added to PreToolUse/PostToolUse stdin
- Transcript schema v2 (nested subagent transcripts, v1-compatible loader)

Spec: `docs/superpowers/specs/2026-05-23-user-agents-design.md`
Plan: `docs/superpowers/plans/2026-05-23-user-agents.md`

## Test plan

- [ ] `cargo test --workspace` green locally
- [ ] CI green for pushed SHA
- [ ] Manual smoke: TUI run with a sample agent file, parent model calls `task`, subagent returns expected text
- [ ] Manual smoke: PreToolUse hook in a subagent context sees `subagent` field in stdin
- [ ] Manual smoke: SubagentStop hook fires and can re-prompt once (then `stop_hook_active=true`)
- [ ] Old transcript loads with warn-log (no crash)
EOF
)"
```

- [ ] **Step 6: Wait for CI green**

```bash
gh run watch
```

Confirm the green checkmark for the pushed SHA before claiming done.

- [ ] **Step 7: Do NOT push a tag**

Per [[feedback_phase_release_rollup]] and [[project_release_plan]]: this is one of multiple sub-projects bundled into a future tag. No `git tag` or `git push --tags` in this PR. The tag lands after sub-project D (or whatever is the last sub-project shipped), with consolidated release notes.

- [ ] **Step 8: Update issue tracker**

Per [[feedback_keep_issue_updated]]: post a comment on the roadmap issue (the one that tracks A/B/C/D) indicating C has shipped its PR. Do not close the issue.

---

## Final notes

- **No tag in this PR.** Sub-project C is one of four; the tag and cargo-dist release come later (per [[feedback_cargo_dist_release]] and [[feedback_phase_release_rollup]]).
- **Local-only commits during execution.** Push happens once at the end (Task 28). Do not push intermediate commits. Per [[feedback_git_expert_explicit_push_framing]]: if delegating to `git-expert`, explicitly frame as "local only, zero git push" and require return confirmation.
- **Backward-compat in stdin payloads.** The new `subagent` field is optional; existing `.claude/settings.json` hooks that ignore unknown fields continue to work. Hook authors who care can branch on its presence.
- **`SubagentStop` ordering.** The event fires **after** the subagent's `end_turn` and **before** the `task` tool returns. If a `SubagentStop` hook re-prompts (`continue: false` + `additionalContext`), the SubHost runs another turn with `stop_hook_active=true`.
- **Type names introduced (cross-task reference):**
  - `savvagent_plugin::HookKind::SubagentStop`
  - `savvagent_plugin::HostEvent::SubagentStop`
  - `savvagent_plugin::Effect::RegisterInProcessTool`
  - `savvagent_plugin::InProcessToolHandler`
  - `savvagent_host::SubagentContext`
  - `savvagent_host::ToolCallContext`
  - `savvagent_host::ScopedToolRegistry`
  - `savvagent_host::SubHost`, `SubHostError`
  - `savvagent_host::subhost::SUBAGENT_NAME` (tokio task-local)
  - `savvagent_host::subhost::max_depth_from_env`
  - `crate::plugin::builtin::user_agents::{UserAgentsPlugin, AgentIndex, AgentSpec, ToolsScope, TaskToolHandler}`
  - `crate::plugin::builtin::user_hooks::discovery::HookEvent::SubagentStop`

If any of these names already exists in your tree (e.g. because sub-project B added something with the same name), reconcile by reading the existing definition before adding — don't shadow.
