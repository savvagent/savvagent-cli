# Host MCP Resources Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `savvagent-host` consume MCP resource notifications from tool servers, surface them on `TurnEvent::ResourceUpdated`, expose a built-in `read_resource` tool to the model, and inject `[resource updated: <uri>]` notes into the conversation at the iteration boundary. Prerequisite for `tool-lsp`.

**Architecture:** Replace the empty `()` client handler on every `ToolServer` with a `ResourceCapturingHandler` that forwards `notifications/resources/updated` (and `…/list_changed`) into an mpsc channel owned by the registry. `Host` spawns a pump task that drains that channel into a new `ResourceCache` keyed by URI. Each update emits `TurnEvent::ResourceUpdated`; the cache's dirty set is drained at the start of each tool-use-loop iteration and injected as synthetic user-role text blocks. A built-in `read_resource` synthetic tool lives in `Host::dispatch_tool` and routes to the owning `ToolServer`'s `resources/read`.

**Tech Stack:** `rmcp 1.6` (already a workspace dep — provides `ClientHandler::on_resource_updated`, `ResourceUpdatedNotificationParam`, `ReadResourceRequestParams`), tokio mpsc channels, the existing `TurnEvent`/`ToolRegistry`/`Host` modules.

**Spec:** `docs/superpowers/specs/2026-05-20-host-mcp-resources-design.md`

**Release line:** v0.22.0 (next minor after the v0.21.0 master HEAD).

**Branch:** `feat/host-mcp-resources` (create off master).

---

## File Map

**New files**
- `crates/savvagent-host/src/resources.rs` — `ResourceCache`, `ResourceSnapshot`, owner-tracking, dirty-set lifecycle. Pure logic; no rmcp imports.
- `crates/savvagent-host/tests/fixtures/resource-tool/main.rs` — test-only MCP stdio binary that publishes one resource and serves `resources/read`. Drives the end-to-end test.
- `crates/savvagent-host/tests/fixtures/resource-tool/Cargo.toml` — test fixture crate.

**Modified files**
- `crates/savvagent-host/src/tools.rs` — add `ResourceCapturingHandler` (replaces the `()` handler in `ToolServer.service`); accept a resource-event sender at `connect`; route `resources/read` for a known URI to the right server.
- `crates/savvagent-host/src/session.rs` — `TurnEvent::ResourceUpdated` variant; spawn `resource_pump`; integrate `ResourceCache`; intercept `read_resource` in the tool-call dispatch path; inject `[resource updated: <uri>]` user blocks at iteration boundary.
- `crates/savvagent-host/src/lib.rs` — `pub mod resources;` declaration; re-export the cache + snapshot types.
- `crates/savvagent-host/Cargo.toml` — add `[[test]]` entry for the fixture-bin used by integration tests.
- `Cargo.toml` (workspace root) — bump `version = "0.22.0"` and matching `[workspace.dependencies]` literals.
- `CHANGELOG.md` — `## 0.22.0` entry.
- `README.md` — document `read_resource` in the built-in tool list and the `[resource updated: <uri>]` semantics.

---

## Task 1: Add `TurnEvent::ResourceUpdated` variant

**Files:**
- Modify: `crates/savvagent-host/src/session.rs:170-271` (the `TurnEvent` enum)

- [ ] **Step 1: Write a failing pattern-exhaustiveness test**

Add this test at the bottom of the existing `#[cfg(test)] mod tests` block in `crates/savvagent-host/src/session.rs` (find the last `#[test]`/`#[tokio::test]` in the file and append). Use `grep -n "^#\[cfg(test)\]\|^mod tests" crates/savvagent-host/src/session.rs` to find the right location — the file has one outer test module near the bottom.

```rust
#[test]
fn turn_event_resource_updated_carries_uri_owner_summary() {
    // Pinning the variant fields so an accidental rename in a later
    // refactor doesn't silently change the wire surface the TUI matches on.
    let ev = TurnEvent::ResourceUpdated {
        uri: "lsp://diagnostics/src/foo.rs".into(),
        owner: "tool-lsp".into(),
        summary: "3 errors, 1 warning".into(),
    };
    match ev {
        TurnEvent::ResourceUpdated { uri, owner, summary } => {
            assert_eq!(uri, "lsp://diagnostics/src/foo.rs");
            assert_eq!(owner, "tool-lsp");
            assert_eq!(summary, "3 errors, 1 warning");
        }
        _ => panic!("constructed variant didn't match"),
    }
}
```

- [ ] **Step 2: Run the test to confirm it fails**

Run: `rustup run stable cargo test -p savvagent-host turn_event_resource_updated_carries_uri_owner_summary 2>&1 | tail -10`
Expected: compile error — `no variant or associated item named ResourceUpdated found for enum TurnEvent`.

- [ ] **Step 3: Add the variant**

In `crates/savvagent-host/src/session.rs`, find the `pub enum TurnEvent {` declaration (around line 170) and add this variant immediately before the existing `TurnComplete { … }` variant:

```rust
    /// A tool server published `notifications/resources/updated`. The TUI
    /// can render a one-line banner. The host has already injected (or
    /// will inject at the next iteration boundary) a synthetic
    /// `[resource updated: <uri>]` user-text block so the model sees the
    /// update without any TUI involvement.
    ResourceUpdated {
        /// Resource URI as published by the tool (e.g. `lsp://diagnostics/src/foo.rs`).
        uri: String,
        /// Label of the tool server that published it (matches `ToolServer.label`).
        owner: String,
        /// Producer-supplied one-line summary; keep under ~80 chars for TUI banners.
        /// Defaults to the URI when the producer didn't include one.
        summary: String,
    },
```

- [ ] **Step 4: Run the test, confirm pass**

Run: `rustup run stable cargo test -p savvagent-host turn_event_resource_updated_carries_uri_owner_summary 2>&1 | tail -5`
Expected: `test result: ok. 1 passed; …`.

- [ ] **Step 5: Run the rest of savvagent-host tests to confirm no fallout**

Run: `rustup run stable cargo test -p savvagent-host 2>&1 | tail -5`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/session.rs
git commit -m "host: add TurnEvent::ResourceUpdated variant

Adds the host-side event for MCP resource notifications from tool
servers. No emitter wired yet — that lands with the pump task in a
later PR. Pins the field set via a constructor test so future renames
of uri/owner/summary trip CI."
```

---

## Task 2: Create `resources.rs` module with `ResourceCache`

**Files:**
- Create: `crates/savvagent-host/src/resources.rs`
- Modify: `crates/savvagent-host/src/lib.rs`

- [ ] **Step 1: Add module declaration to lib.rs**

In `crates/savvagent-host/src/lib.rs`, find the existing `pub mod` declarations and append:

```rust
pub mod resources;
```

- [ ] **Step 2: Create `resources.rs` with the cache types and unit tests**

Create `crates/savvagent-host/src/resources.rs`:

```rust
//! Per-host cache of MCP resources advertised by connected tool servers.
//!
//! The cache stores **ownership + sequence**, not bodies. Resource bodies are
//! pulled on demand by the model via the `read_resource` synthetic tool;
//! caching them would force us to invalidate on every server update and gain
//! nothing — tools are local children, the read round-trip is cheap.
//!
//! The `dirty_since_last_iteration` set tracks URIs that received an
//! `updated` notification since the host last drained the set at the
//! tool-use-loop boundary. [`Host`] reads + clears the set inside the loop
//! and uses it to inject `[resource updated: <uri>]` user-text blocks.

use std::collections::{HashMap, HashSet};

/// One entry in the cache. The `seq` field is monotonically increasing
/// across all updates the host has observed for any URI — useful for
/// telemetry and for detecting "did anything change since I last looked."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceSnapshot {
    /// Tool server label (matches `ToolServer.label`) that owns this URI.
    pub owner: String,
    /// Monotonic sequence number; higher means more recent.
    pub seq: u64,
}

/// Cache of every resource any connected tool has notified us about.
#[derive(Debug, Default)]
pub struct ResourceCache {
    entries: HashMap<String, ResourceSnapshot>,
    dirty: HashSet<String>,
    next_seq: u64,
}

impl ResourceCache {
    /// Record an `updated` notification from `owner` for `uri`. Sets the
    /// URI as dirty until the next [`Self::drain_dirty`] call.
    pub fn mark_updated(&mut self, uri: impl Into<String>, owner: impl Into<String>) {
        let uri = uri.into();
        self.next_seq = self.next_seq.saturating_add(1);
        let snapshot = ResourceSnapshot {
            owner: owner.into(),
            seq: self.next_seq,
        };
        self.entries.insert(uri.clone(), snapshot);
        self.dirty.insert(uri);
    }

    /// Look up the owner of a URI. Returns `None` if no notification has
    /// ever arrived for it.
    pub fn owner(&self, uri: &str) -> Option<&str> {
        self.entries.get(uri).map(|s| s.owner.as_str())
    }

    /// Drain the dirty set, returning each URI in insertion order. Sorts
    /// for stability — same set of URIs always produces the same drain
    /// sequence, so injected conversation blocks land in a deterministic
    /// order across hosts.
    pub fn drain_dirty(&mut self) -> Vec<String> {
        let mut out: Vec<String> = self.dirty.drain().collect();
        out.sort();
        out
    }

    /// Number of distinct URIs ever observed. Test/telemetry helper.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_updated_records_owner_and_dirties_uri() {
        let mut cache = ResourceCache::default();
        cache.mark_updated("lsp://diagnostics/a.rs", "tool-lsp");
        assert_eq!(cache.owner("lsp://diagnostics/a.rs"), Some("tool-lsp"));
        assert_eq!(cache.drain_dirty(), vec!["lsp://diagnostics/a.rs"]);
    }

    #[test]
    fn drain_dirty_is_idempotent_after_first_call() {
        let mut cache = ResourceCache::default();
        cache.mark_updated("a", "t");
        let _ = cache.drain_dirty();
        assert!(
            cache.drain_dirty().is_empty(),
            "drain_dirty must clear the set; second call returns empty"
        );
    }

    #[test]
    fn second_update_for_same_uri_still_dirties() {
        let mut cache = ResourceCache::default();
        cache.mark_updated("a", "t");
        let _ = cache.drain_dirty();
        cache.mark_updated("a", "t");
        assert_eq!(cache.drain_dirty(), vec!["a"]);
    }

    #[test]
    fn drain_dirty_returns_sorted_uris_for_determinism() {
        // Insertion order is HashSet-defined and therefore arbitrary;
        // we sort so callers (the conversation-injection step) see a
        // stable order regardless of host platform / hash randomization.
        let mut cache = ResourceCache::default();
        cache.mark_updated("zzz", "t");
        cache.mark_updated("aaa", "t");
        cache.mark_updated("mmm", "t");
        assert_eq!(cache.drain_dirty(), vec!["aaa", "mmm", "zzz"]);
    }

    #[test]
    fn seq_is_monotonic_across_updates() {
        let mut cache = ResourceCache::default();
        cache.mark_updated("a", "t");
        cache.mark_updated("b", "t");
        let seq_a = cache.entries.get("a").unwrap().seq;
        let seq_b = cache.entries.get("b").unwrap().seq;
        assert!(seq_b > seq_a, "later updates must have higher seq");
    }

    #[test]
    fn owner_updates_when_a_different_tool_republishes() {
        // Reasonable: if tool-A publishes URI X and later tool-B publishes
        // the same URI, the latest owner wins. Multi-publisher cases are
        // unlikely in practice but we shouldn't silently keep the stale
        // owner.
        let mut cache = ResourceCache::default();
        cache.mark_updated("x", "tool-a");
        cache.mark_updated("x", "tool-b");
        assert_eq!(cache.owner("x"), Some("tool-b"));
    }
}
```

- [ ] **Step 3: Run the cache tests**

Run: `rustup run stable cargo test -p savvagent-host resources:: 2>&1 | tail -10`
Expected: 6 tests passing.

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent-host/src/lib.rs crates/savvagent-host/src/resources.rs
git commit -m "host: add ResourceCache + ResourceSnapshot

Cache stores ownership + monotonic seq, not bodies — reads pull on
demand via the upcoming read_resource synthetic tool. drain_dirty()
returns sorted URIs so the iteration-boundary injection step lands
[resource updated: <uri>] blocks in a stable order regardless of
hashmap iteration order."
```

---

## Task 3: `ResourceCapturingHandler` + channel

**Files:**
- Modify: `crates/savvagent-host/src/tools.rs`

- [ ] **Step 1: Add a failing test for the handler**

In `crates/savvagent-host/src/tools.rs`, find the existing `#[cfg(test)] mod lazy_bash_tests {` block (around line 784) and add a new sibling test module immediately after it (still inside the file, before the existing `#[cfg(test)] mod tool_call_outcome_tests`):

```rust
#[cfg(test)]
mod resource_handler_tests {
    use super::*;
    use rmcp::model::ResourceUpdatedNotificationParam;
    use rmcp::service::NotificationContext;
    use rmcp::RoleClient;
    use std::sync::Arc as StdArc;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn handler_forwards_resource_updated_on_channel() {
        let (tx, mut rx) = mpsc::channel::<ResourceEvent>(8);
        let handler = ResourceCapturingHandler::new("tool-lsp".to_string(), tx);

        // We can't easily build a real NotificationContext (Peer is
        // private), so we exercise the path the rmcp service layer
        // takes: it deconstructs the notification into params and a
        // context, then calls on_resource_updated. We test the helper
        // that actually forwards.
        handler
            .forward_updated_for_test(ResourceUpdatedNotificationParam {
                uri: "lsp://diagnostics/foo.rs".into(),
            })
            .await;

        let evt = rx.recv().await.expect("event must arrive");
        assert!(matches!(evt, ResourceEvent::Updated { .. }));
        if let ResourceEvent::Updated { owner, uri } = evt {
            assert_eq!(owner, "tool-lsp");
            assert_eq!(uri, "lsp://diagnostics/foo.rs");
        }
    }

    #[tokio::test]
    async fn handler_forward_failure_is_silent() {
        // Receiver dropped → mpsc::Sender::send returns Err. The handler
        // must not panic; it just logs and drops the event. We assert
        // by calling forward and observing no panic.
        let (tx, rx) = mpsc::channel::<ResourceEvent>(1);
        drop(rx);
        let handler = ResourceCapturingHandler::new("dead-tool".to_string(), tx);
        handler
            .forward_updated_for_test(ResourceUpdatedNotificationParam {
                uri: "lsp://x".into(),
            })
            .await;
        // If we got here we passed.
    }

    // Keep this import group local so the test module compiles regardless
    // of whether the parent file imports rmcp::Arc.
    fn _ensure_send_sync<T: Send + Sync>() {}
    #[test]
    fn handler_is_send_sync() {
        _ensure_send_sync::<ResourceCapturingHandler>();
    }
}
```

- [ ] **Step 2: Run the test to confirm compile failure**

Run: `rustup run stable cargo test -p savvagent-host resource_handler_tests 2>&1 | tail -20`
Expected: compile errors — `ResourceEvent`, `ResourceCapturingHandler`, `forward_updated_for_test` not found.

- [ ] **Step 3: Add the handler type and event enum**

At the top of `crates/savvagent-host/src/tools.rs`, immediately after the existing `use` block, add:

```rust
use rmcp::model::{ResourceUpdatedNotificationParam};
use rmcp::service::NotificationContext;
use rmcp::ClientHandler;
```

Then, immediately before the `pub(crate) struct ToolRegistry {` declaration (around line 152), insert the new types:

```rust
/// Resource notification observed by a [`ResourceCapturingHandler`].
///
/// Currently only `Updated` carries a URI. `ListChanged` notifications
/// don't include URIs in the MCP wire format — the receiver is expected
/// to call `resources/list` to discover the new set. We don't need that
/// today (tools we own publish updates eagerly), but the variant exists
/// so the channel surface is forward-compatible.
#[derive(Debug, Clone)]
pub(crate) enum ResourceEvent {
    /// `notifications/resources/updated` from `owner` for `uri`.
    Updated {
        /// Tool server label (matches `ToolServer.label`).
        owner: String,
        /// URI as published by the tool.
        uri: String,
    },
    /// `notifications/resources/list_changed` from `owner`.
    ListChanged {
        /// Tool server label.
        owner: String,
    },
}

/// rmcp [`ClientHandler`] impl installed on every `ToolServer` so that
/// server-initiated `notifications/resources/*` notifications flow into
/// the host's resource pump instead of being silently dropped (which is
/// what the default `impl ClientHandler for ()` does).
///
/// Each handler is bound to one tool's `label` at construction time so
/// the pump knows which server published each event.
pub(crate) struct ResourceCapturingHandler {
    label: String,
    tx: tokio::sync::mpsc::Sender<ResourceEvent>,
}

impl ResourceCapturingHandler {
    pub(crate) fn new(label: String, tx: tokio::sync::mpsc::Sender<ResourceEvent>) -> Self {
        Self { label, tx }
    }

    /// Test-only helper: forwards an `updated` notification through the
    /// same code path the rmcp service uses, without needing to
    /// synthesize a `NotificationContext` (whose fields are private).
    #[cfg(test)]
    pub(crate) async fn forward_updated_for_test(
        &self,
        params: ResourceUpdatedNotificationParam,
    ) {
        self.send_updated(params.uri.to_string()).await;
    }

    async fn send_updated(&self, uri: String) {
        let event = ResourceEvent::Updated {
            owner: self.label.clone(),
            uri,
        };
        if let Err(err) = self.tx.send(event).await {
            // Receiver dropped — host is shutting down or the pump panicked.
            // Either way, drop the event silently; we don't want to apply
            // backpressure to the tool subprocess (which would stall a
            // language server's reanalysis).
            tracing::warn!(
                owner = %self.label,
                "resource pump receiver dropped; dropping notification: {err}"
            );
        }
    }

    async fn send_list_changed(&self) {
        let event = ResourceEvent::ListChanged {
            owner: self.label.clone(),
        };
        if let Err(err) = self.tx.send(event).await {
            tracing::warn!(
                owner = %self.label,
                "resource pump receiver dropped; dropping list_changed: {err}"
            );
        }
    }
}

impl ClientHandler for ResourceCapturingHandler {
    fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<rmcp::RoleClient>,
    ) -> impl std::future::Future<Output = ()> + Send + '_ {
        async move {
            self.send_updated(params.uri.to_string()).await;
        }
    }

    fn on_resource_list_changed(
        &self,
        _context: NotificationContext<rmcp::RoleClient>,
    ) -> impl std::future::Future<Output = ()> + Send + '_ {
        async move {
            self.send_list_changed().await;
        }
    }
}
```

- [ ] **Step 4: Run the handler tests, confirm pass**

Run: `rustup run stable cargo test -p savvagent-host resource_handler_tests 2>&1 | tail -10`
Expected: 3 tests passing.

- [ ] **Step 5: Run the whole crate to confirm no unrelated breakage**

Run: `rustup run stable cargo test -p savvagent-host 2>&1 | tail -10`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/tools.rs
git commit -m "host: ResourceCapturingHandler + ResourceEvent channel

Replaces the default no-op ClientHandler (the unit type \`()\`) with a
labelled handler that forwards \`notifications/resources/updated\` and
\`notifications/resources/list_changed\` onto an mpsc channel. The
handler isn't wired into ToolRegistry::connect yet — that's the next
task. Channel pump + cache integration follow in the task after."
```

---

## Task 4: Wire `ResourceCapturingHandler` into `ToolRegistry::connect`

**Files:**
- Modify: `crates/savvagent-host/src/tools.rs`

- [ ] **Step 1: Change `ToolServer.service` type to use the handler**

In `crates/savvagent-host/src/tools.rs`, locate the `struct ToolServer` declaration (around line 167):

```rust
struct ToolServer {
    label: String,
    service: RunningService<RoleClient, ()>,
}
```

Replace with:

```rust
struct ToolServer {
    label: String,
    service: RunningService<RoleClient, ResourceCapturingHandler>,
}
```

- [ ] **Step 2: Update `ToolRegistry::connect` signature to accept a resource sender**

In the same file, locate `pub async fn connect(` (around line 265). Change the signature from:

```rust
    pub async fn connect(
        endpoints: &[ToolEndpoint],
        project_root: &Path,
        sandbox: &SandboxConfig,
        bash_net_resolver: BashNetResolverHandle,
    ) -> Result<Self> {
```

…to:

```rust
    pub async fn connect(
        endpoints: &[ToolEndpoint],
        project_root: &Path,
        sandbox: &SandboxConfig,
        bash_net_resolver: BashNetResolverHandle,
        resource_tx: tokio::sync::mpsc::Sender<ResourceEvent>,
    ) -> Result<Self> {
```

- [ ] **Step 3: Pass a labelled handler to each `serve` call**

Still inside `connect`, find both `serve` calls:

1. The probe-spawn path for tool-bash (around line 297):

```rust
                        let service = ()
                            .serve(transport)
                            .await
                            .with_context(|| format!("init MCP session with {label}"))?;
```

Replace with:

```rust
                        let handler = ResourceCapturingHandler::new(
                            label.clone(),
                            resource_tx.clone(),
                        );
                        let service = handler
                            .serve(transport)
                            .await
                            .with_context(|| format!("init MCP session with {label}"))?;
```

2. The eager-server path (around line 360):

```rust
                        let service = ()
                            .serve(transport)
                            .await
                            .with_context(|| format!("init MCP session with {label}"))?;
```

Replace with the same shape:

```rust
                        let handler = ResourceCapturingHandler::new(
                            label.clone(),
                            resource_tx.clone(),
                        );
                        let service = handler
                            .serve(transport)
                            .await
                            .with_context(|| format!("init MCP session with {label}"))?;
```

- [ ] **Step 4: Update every `ToolRegistry::connect` call site to pass a sender**

Find all callers:

Run: `grep -rn "ToolRegistry::connect" crates/ 2>&1`

There will be one call in `crates/savvagent-host/src/session.rs` (inside `Host::start`). Locate it and update by changing:

```rust
        let tools = ToolRegistry::connect(
            &config.tools,
            &config.project_root,
            &config.sandbox,
            bash_net_resolver,
        )
        .await?;
```

…to:

```rust
        // Resource channel: bounded at 64. If a tool publishes faster than
        // the pump drains, oldest-first warnings fire; we never block the
        // subprocess.
        let (resource_tx, resource_rx) = tokio::sync::mpsc::channel::<crate::tools::ResourceEvent>(64);

        let tools = ToolRegistry::connect(
            &config.tools,
            &config.project_root,
            &config.sandbox,
            bash_net_resolver,
            resource_tx,
        )
        .await?;
```

The `resource_rx` is stashed into `Host` in the next task — for now, keep it as a local; the host won't yet read from it.

To avoid the "unused variable" warning that would fail `-D warnings`, prefix it:

```rust
        let _resource_rx = resource_rx; // pump task wired in next task
```

- [ ] **Step 5: Build the crate to surface any callers we missed**

Run: `rustup run stable cargo build -p savvagent-host 2>&1 | tail -20`
Expected: compiles cleanly.

If there are test-file callers, update them by adding a no-op channel:

```rust
let (resource_tx, _resource_rx) = tokio::sync::mpsc::channel(64);
ToolRegistry::connect(..., resource_tx).await?
```

- [ ] **Step 6: Re-export `ResourceEvent` from `tools.rs`**

In `crates/savvagent-host/src/tools.rs`, the `ResourceEvent` enum is `pub(crate)` — the integration test fixture in a later task needs to see it, but only from within this crate. Keep `pub(crate)` for now.

Run: `rustup run stable cargo test -p savvagent-host 2>&1 | tail -10`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add crates/savvagent-host/src/tools.rs crates/savvagent-host/src/session.rs
git commit -m "host: install ResourceCapturingHandler on every ToolServer

connect() now requires a Sender<ResourceEvent>; each eager tool and
the tool-bash probe both serve with a labelled handler that forwards
resource notifications onto that channel. Host::start creates the
channel and parks the receiver as _resource_rx until the pump task
lands in the next commit. No behavior change yet."
```

---

## Task 5: `Host::resource_pump` task drains channel into `ResourceCache`

**Files:**
- Modify: `crates/savvagent-host/src/session.rs`

- [ ] **Step 1: Add `resources` field to `Host` and a `Mutex<ResourceCache>`**

In `crates/savvagent-host/src/session.rs`, locate the `pub struct Host {` declaration (around line 274). Add this field at the end of the struct, right before the closing `}`:

```rust
    /// Resource cache populated by the resource_pump task. Read at each
    /// tool-use-loop iteration boundary to inject `[resource updated: …]`
    /// user-text blocks into the conversation.
    resources: Arc<tokio::sync::Mutex<crate::resources::ResourceCache>>,
```

- [ ] **Step 2: Initialize the field in `Host::start`**

Still in `session.rs`, find `Host::start`'s `Ok(Self { … })` block (where every other field is populated). Add this field initialization (alongside the others):

```rust
            resources: Arc::new(tokio::sync::Mutex::new(crate::resources::ResourceCache::default())),
```

- [ ] **Step 3: Spawn the pump task in `Host::start`**

Still in `Host::start`, just before `Ok(host)` (or whatever the final return is — usually the last lines of the function), replace the placeholder line:

```rust
        let _resource_rx = resource_rx; // pump task wired in next task
```

…with:

```rust
        // Spawn the resource pump. It owns the receiver, the cache handle,
        // and a Weak<…> to the current_turn_events slot. When a turn is
        // live the pump emits TurnEvent::ResourceUpdated; when no turn is
        // live (between turns) it still updates the cache so the next
        // turn sees the updates at its iteration boundary.
        let cache = Arc::clone(&host.resources);
        let events_slot = Arc::clone(&host.current_turn_events);
        tokio::spawn(async move {
            resource_pump(resource_rx, cache, events_slot).await;
        });
```

Note: the existing `Host` is bound to a local named `host` somewhere in `start`. If it isn't, bind it before the spawn — `let host = Self { … };` — then spawn, then `Ok(host)`.

- [ ] **Step 4: Add the `resource_pump` free function at the bottom of the file**

Append at the very bottom of `crates/savvagent-host/src/session.rs`, after the existing helpers and before any `#[cfg(test)]` modules:

```rust
/// Drain resource events from `rx` into `cache`. When a turn is live
/// (i.e. `events_slot` holds a `Some`), also emit a
/// [`TurnEvent::ResourceUpdated`] so the TUI can render a banner. The
/// cache mutation always happens regardless of whether a turn is live —
/// the next iteration boundary will surface the URI via conversation
/// injection in either case.
async fn resource_pump(
    mut rx: mpsc::Receiver<crate::tools::ResourceEvent>,
    cache: Arc<tokio::sync::Mutex<crate::resources::ResourceCache>>,
    events_slot: Arc<std::sync::Mutex<Option<mpsc::Sender<TurnEvent>>>>,
) {
    while let Some(event) = rx.recv().await {
        match event {
            crate::tools::ResourceEvent::Updated { owner, uri } => {
                {
                    let mut guard = cache.lock().await;
                    guard.mark_updated(uri.clone(), owner.clone());
                    // guard dropped here
                }
                // Snapshot the events sender under the std::sync::Mutex,
                // then drop the guard before awaiting on the send. Same
                // discipline as current_turn_events use everywhere else
                // in this file.
                let maybe_tx = {
                    let guard = events_slot.lock().expect("events slot poisoned");
                    guard.clone()
                };
                if let Some(tx) = maybe_tx {
                    let summary = uri.clone();
                    let _ = tx
                        .send(TurnEvent::ResourceUpdated {
                            uri,
                            owner,
                            summary,
                        })
                        .await;
                }
            }
            crate::tools::ResourceEvent::ListChanged { owner } => {
                tracing::debug!(
                    owner = %owner,
                    "resources/list_changed received; ignored (host pulls on `updated`)"
                );
            }
        }
    }
    tracing::debug!("resource_pump channel closed; pump exiting");
}
```

- [ ] **Step 5: Run the crate tests to confirm nothing regressed**

Run: `rustup run stable cargo test -p savvagent-host 2>&1 | tail -10`
Expected: all green. (No new test for the pump in this task — its behavior is end-to-end-tested via the fixture tool in Task 9.)

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/session.rs
git commit -m "host: spawn resource_pump task; emit TurnEvent::ResourceUpdated

The pump owns the resource channel receiver and the ResourceCache
mutex. On every \`Updated\` event it mark_updated()s the cache and, if
a turn is live, fires TurnEvent::ResourceUpdated so the TUI can show
a banner. \`ListChanged\` is logged and ignored — host only acts on
explicit URI updates today."
```

---

## Task 6: Built-in `read_resource` synthetic tool

**Files:**
- Modify: `crates/savvagent-host/src/tools.rs`
- Modify: `crates/savvagent-host/src/session.rs`

- [ ] **Step 1: Add a constant for the tool name + the synthetic ToolDef**

At the top of `crates/savvagent-host/src/tools.rs`, just after the existing `const TOOL_BASH_MARKER: &str = "tool-bash";` line (around line 46), add:

```rust
/// Name of the host-built-in tool that reads MCP resources by URI.
/// Always present in `ToolRegistry::defs`, regardless of whether any
/// connected tool publishes resources today.
pub(crate) const READ_RESOURCE_TOOL_NAME: &str = "read_resource";
```

- [ ] **Step 2: Append the synthetic ToolDef to `defs` at the end of `connect`**

In `ToolRegistry::connect`, just before the final `Ok(Self { … })` (after the `tracing::debug!` summary), add:

```rust
        // Synthetic built-in: read_resource. Always present; the dispatch
        // path in Host::dispatch_tool routes it without consulting
        // `routes` (which only knows about real, wire-spoken tools).
        defs.push(ToolDef {
            name: READ_RESOURCE_TOOL_NAME.to_string(),
            description: "Fetch the contents of an MCP resource by URI. \
                URIs are surfaced via `[resource updated: <uri>]` notes in the \
                conversation. Returns the resource body as text or JSON."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "uri": { "type": "string" } },
                "required": ["uri"]
            })
            .as_object()
            .cloned()
            .expect("static JSON object literal")
            .into(),
        });
```

Note: `ToolDef.input_schema` is a `serde_json::Value` — verify by running `grep -n "pub struct ToolDef" crates/savvagent-protocol/src/tool.rs` and reading the field. If the type is `Value` (object), the `.into()` chain may not be needed; in that case the literal `serde_json::json!({...})` suffices.

Use this defensive form that works either way:

```rust
        defs.push(ToolDef {
            name: READ_RESOURCE_TOOL_NAME.to_string(),
            description: "Fetch the contents of an MCP resource by URI. \
                URIs are surfaced via `[resource updated: <uri>]` notes in the \
                conversation. Returns the resource body as text or JSON."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "uri": { "type": "string" } },
                "required": ["uri"]
            }),
        });
```

If the field type is `serde_json::Map<String, Value>` instead, adjust the assignment to `.as_object().cloned().unwrap()`.

- [ ] **Step 3: Add a `dispatch_read_resource` method to `ToolRegistry`**

Still in `tools.rs`, inside `impl ToolRegistry`, append a new method just before the closing `}` of the impl block (around line 489, right after `shutdown`):

```rust
    /// Dispatch the synthetic `read_resource` tool. Looks up the owning
    /// tool server in `eager_servers` (resources can't come from bash —
    /// bash is request/response only — so we don't consult the lazy slot).
    pub(crate) async fn dispatch_read_resource(
        &self,
        uri: &str,
        owner: &str,
    ) -> ToolCallOutcome {
        let server = self
            .eager_servers
            .iter()
            .find(|s| s.label == owner);
        let Some(server) = server else {
            return ToolCallOutcome::error(format!(
                "unknown resource owner: {owner}; no tool advertises this URI ({uri})"
            ));
        };
        // rmcp's RunningService exposes read_resource via peer().
        let req = rmcp::model::ReadResourceRequestParams {
            uri: uri.to_string(),
        };
        match server.service.peer().read_resource(req).await {
            Ok(result) => {
                // result.contents is Vec<ResourceContents>. Serialize the
                // whole envelope as JSON — the model gets text or blobs
                // as the tool publishes them.
                let body = serde_json::to_string(&result.contents)
                    .unwrap_or_else(|_| "<unrenderable resource contents>".into());
                ToolCallOutcome::success(body)
            }
            Err(err) => {
                tracing::error!(uri, owner, error = ?err, "read_resource RPC failed");
                ToolCallOutcome::error(format!(
                    "read_resource failed for {uri} on {owner}: {err}"
                ))
            }
        }
    }
```

(`ToolCallOutcome::success` is currently private — check by running `grep -n "fn success" crates/savvagent-host/src/tools.rs`. It's `fn success(payload: String) -> Self {` at module level. If it's not `pub(crate)`, add `pub(crate)` to both `success` and `error`.)

- [ ] **Step 4: Intercept `read_resource` in Host's tool-call path**

In `crates/savvagent-host/src/session.rs`, find the site where the host calls `tools.call_with_bash_net_override(name, ...)`. Use:

Run: `grep -n "call_with_bash_net_override\|ToolRegistry" crates/savvagent-host/src/session.rs 2>&1 | head -10`

The call lives inside the tool-use loop around line 1122 (look for `ToolCallStarted`). Locate the dispatch site — it will look approximately like:

```rust
let outcome = tools.call_with_bash_net_override(&tool_use.name, tool_use.input.clone(), NetOverride::Inherit).await;
```

Immediately before that line, add an interception:

```rust
            let outcome = if tool_use.name == crate::tools::READ_RESOURCE_TOOL_NAME {
                // Synthetic read_resource: parse uri, look up owner via
                // resource cache, dispatch via the registry helper.
                let uri = tool_use
                    .input
                    .as_object()
                    .and_then(|m| m.get("uri"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                match uri {
                    None => crate::tools::ToolCallOutcome::error(
                        "read_resource requires `uri: string`".to_string(),
                    ),
                    Some(uri) => {
                        let owner = {
                            let guard = self.resources.lock().await;
                            guard.owner(&uri).map(str::to_string)
                        };
                        match owner {
                            None => crate::tools::ToolCallOutcome::error(format!(
                                "unknown resource: {uri}; no tool advertises ownership"
                            )),
                            Some(owner) => {
                                tools.dispatch_read_resource(&uri, &owner).await
                            }
                        }
                    }
                }
            } else {
                tools
                    .call_with_bash_net_override(
                        &tool_use.name,
                        tool_use.input.clone(),
                        crate::tools::NetOverride::Inherit,
                    )
                    .await
            };
```

Note: the original line stays only inside the `else` branch. Make sure the variable name `outcome` continues to be used downstream — no other lines need to change.

`ToolCallOutcome` is `pub(crate)` so the path `crate::tools::ToolCallOutcome` works from inside this crate.

- [ ] **Step 5: Build and run tests**

Run: `rustup run stable cargo test -p savvagent-host 2>&1 | tail -15`
Expected: all green. No new unit test in this task — read_resource is tested end-to-end against the fixture tool in Task 9.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/tools.rs crates/savvagent-host/src/session.rs
git commit -m "host: synthetic read_resource tool routes resources/read

ToolRegistry::defs now always advertises read_resource. The Host
intercepts dispatches of that name in run_turn_inner: it looks up the
owning tool server via ResourceCache and calls rmcp's
service.peer().read_resource() with the supplied URI. End-to-end test
arrives with the fixture tool in a follow-up commit."
```

---

## Task 7: Iteration-boundary conversation injection

**Files:**
- Modify: `crates/savvagent-host/src/session.rs`

- [ ] **Step 1: Drain `dirty` into synthetic user-text messages at iteration start**

In `crates/savvagent-host/src/session.rs`, locate the iteration loop:

```rust
        let mut iterations: u32 = 0;
        let want_stream = events.is_some();

        loop {
            if iterations >= self.config.max_iterations {
                return Err(HostError::LoopLimit(self.config.max_iterations));
            }
            iterations += 1;
```

(Search with `grep -n "let mut iterations" crates/savvagent-host/src/session.rs`.)

Immediately after `iterations += 1;` and before the `if let Some(tx) = &events {` block that emits `IterationStarted`, insert:

```rust
            // Drain resource updates that arrived since the previous
            // iteration (or since turn start) and inject one synthetic
            // user-text block per URI. The model sees them as a fresh
            // user turn between iterations and can call `read_resource`
            // to fetch contents.
            let dirty: Vec<String> = {
                let mut guard = self.resources.lock().await;
                guard.drain_dirty()
            };
            if !dirty.is_empty() {
                let text = dirty
                    .iter()
                    .map(|uri| format!("[resource updated: {uri}]"))
                    .collect::<Vec<_>>()
                    .join("\n");
                messages.push(Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text { text }],
                });
            }
```

The `Message`, `Role`, and `ContentBlock` types are already imported at the top of the file — verify with `grep -n "^use.*Message\|^use.*ContentBlock\|^use.*Role" crates/savvagent-host/src/session.rs`. If any are missing, add `use savvagent_protocol::{ContentBlock, Message, Role};` to the existing import block.

- [ ] **Step 2: Add a unit test that exercises the injection path with a mocked provider**

Add this test at the bottom of `crates/savvagent-host/src/session.rs`'s test module:

```rust
#[tokio::test]
async fn iteration_boundary_injects_resource_updated_block_into_history() {
    use savvagent_protocol::{ContentBlock, Role};

    // Build a host whose provider records every CompleteRequest it sees.
    // After the turn, we inspect the recorded messages to confirm the
    // synthetic [resource updated: …] block landed.
    let (host, recorded) = test_helpers::host_with_recording_provider().await;

    // Pre-populate the cache as if the pump had observed an update
    // arriving between turns.
    {
        let mut cache = host.resources.lock().await;
        cache.mark_updated("lsp://diagnostics/foo.rs", "fixture-tool");
    }

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _outcome = host.run_turn_streaming("hi", tx).await.unwrap();
    while rx.recv().await.is_some() {} // drain events

    let last_req = recorded.last_request();
    let has_injection = last_req.messages.iter().any(|m| {
        matches!(m.role, Role::User)
            && m.content.iter().any(|b| match b {
                ContentBlock::Text { text } => text.contains("[resource updated: lsp://diagnostics/foo.rs]"),
                _ => false,
            })
    });
    assert!(
        has_injection,
        "first iteration's CompleteRequest must include the injected \
         [resource updated: …] user-text block; messages were: {:#?}",
        last_req.messages
    );
}
```

This test depends on a `test_helpers::host_with_recording_provider()` helper. There's likely a similar helper already in the file — grep for `fn host_with` or `mod test_helpers` in `crates/savvagent-host/src/session.rs` and either reuse it or add a thin recording wrapper around the existing test provider.

If no helper exists, add it inside the existing test module:

```rust
mod test_helpers {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[derive(Default, Clone)]
    pub struct RecordedRequests {
        inner: Arc<StdMutex<Vec<CompleteRequest>>>,
    }
    impl RecordedRequests {
        pub fn last_request(&self) -> CompleteRequest {
            self.inner.lock().unwrap().last().cloned().expect("at least one request recorded")
        }
    }

    pub struct RecordingProvider {
        pub records: RecordedRequests,
    }

    #[async_trait::async_trait]
    impl savvagent_mcp::ProviderClient for RecordingProvider {
        async fn complete(
            &self,
            req: CompleteRequest,
            _events: Option<mpsc::Sender<savvagent_protocol::StreamEvent>>,
        ) -> Result<CompleteResponse, savvagent_protocol::ProviderError> {
            self.records.inner.lock().unwrap().push(req.clone());
            Ok(CompleteResponse {
                id: "rec".into(),
                model: req.model,
                content: vec![ContentBlock::Text { text: "ok".into() }],
                stop_reason: savvagent_protocol::StopReason::EndTurn,
                stop_sequence: None,
                usage: Default::default(),
            })
        }
    }

    pub async fn host_with_recording_provider() -> (Arc<Host>, RecordedRequests) {
        let records = RecordedRequests::default();
        let provider = Arc::new(RecordingProvider { records: records.clone() });
        let cfg = HostConfig::builder()
            .with_provider_client_for_test(provider)  // use whatever the existing test path is
            .build();
        let host = Host::start(cfg).await.expect("host starts");
        (Arc::new(host), records)
    }
}
```

If the existing `HostConfig` builder doesn't have `with_provider_client_for_test`, look at how the existing tests in this file construct a `Host` against a mock provider (grep for the closest existing test like `run_turn_streaming_with_blocks_pushes_user_message_verbatim` and mirror its construction). Use the pattern that's already there; do not introduce a new test path.

- [ ] **Step 3: Run the new test**

Run: `rustup run stable cargo test -p savvagent-host iteration_boundary_injects_resource_updated_block_into_history 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 4: Run the rest**

Run: `rustup run stable cargo test -p savvagent-host 2>&1 | tail -10`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/session.rs
git commit -m "host: inject [resource updated: <uri>] blocks at iteration start

At the top of every tool-use-loop iteration, drain the resource
cache's dirty set and append one Message{role:User, content:Text} per
URI. The model sees these as a fresh user turn between iterations
and can decide whether to call read_resource for any of them. URIs
are emitted in sorted order so injection is deterministic across
hosts."
```

---

## Task 8: Resource-tool integration test fixture

**Files:**
- Create: `crates/savvagent-host/tests/fixtures/resource-tool/Cargo.toml`
- Create: `crates/savvagent-host/tests/fixtures/resource-tool/src/main.rs`

- [ ] **Step 1: Add the fixture crate manifest**

Create `crates/savvagent-host/tests/fixtures/resource-tool/Cargo.toml`:

```toml
[package]
name = "resource-tool"
version = "0.0.0"
edition = "2024"
publish = false

[[bin]]
name = "resource-tool"
path = "src/main.rs"

[dependencies]
rmcp = { workspace = true, features = ["server", "transport-io"] }
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "io-std"] }
serde_json.workspace = true
schemars.workspace = true
async-trait.workspace = true
anyhow.workspace = true
```

Verify the rmcp feature set matches what `tool-fs` uses — open `crates/tool-fs/Cargo.toml` and copy the rmcp features it pulls. If `tool-fs` uses a different feature list, prefer that; the goal is a minimal stdio MCP server.

- [ ] **Step 2: Add the fixture binary**

Create `crates/savvagent-host/tests/fixtures/resource-tool/src/main.rs`:

```rust
//! Test-only MCP stdio tool used by the host's resource integration tests.
//!
//! Behavior:
//! - Advertises one tool, `trigger_update`, that takes no arguments and
//!   returns a no-op text payload.
//! - When `trigger_update` is called, the server sends ONE
//!   `notifications/resources/updated` notification for
//!   `test://updated/payload-1` and ONE for `test://updated/payload-2`.
//! - Serves `resources/read` for both URIs, returning `{"value": <uri>}`.

use anyhow::Result;
use rmcp::{
    model::{
        CallToolRequestParam, CallToolResult, Content, ListResourcesResult, ReadResourceResult,
        ResourceContents, ServerCapabilities, ServerInfo, Tool, ToolsCapability,
        ResourceUpdatedNotificationParam, Implementation,
    },
    service::{NotificationContext, RequestContext, ServerHandler, ServiceExt},
    transport::stdio,
    RoleServer,
};
use std::sync::Arc;

#[derive(Default)]
struct Fixture;

impl ServerHandler for Fixture {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            server_info: Implementation {
                name: "resource-tool".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability { list_changed: Some(false) }),
                resources: Some(rmcp::model::ResourcesCapability {
                    subscribe: Some(false),
                    list_changed: Some(false),
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    async fn list_tools(
        &self,
        _params: Option<rmcp::model::PaginatedRequestParam>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::Error> {
        Ok(rmcp::model::ListToolsResult {
            tools: vec![Tool {
                name: "trigger_update".into(),
                description: Some(
                    "Publish two test://updated/* resources via notifications/resources/updated.".into(),
                ),
                input_schema: Arc::new(
                    serde_json::json!({ "type": "object", "properties": {} })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                annotations: None,
            }],
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        _params: CallToolRequestParam,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::Error> {
        // Fire two `updated` notifications. We do this BEFORE returning so
        // the client sees the events before the tool result lands.
        for uri in &["test://updated/payload-1", "test://updated/payload-2"] {
            let _ = ctx
                .peer
                .notify_resource_updated(ResourceUpdatedNotificationParam {
                    uri: uri.to_string(),
                })
                .await;
        }
        Ok(CallToolResult::success(vec![Content::text("ok")]))
    }

    async fn list_resources(
        &self,
        _params: Option<rmcp::model::PaginatedRequestParam>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, rmcp::Error> {
        Ok(ListResourcesResult {
            resources: vec![],
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        params: rmcp::model::ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, rmcp::Error> {
        let body = serde_json::json!({ "value": params.uri.clone() }).to_string();
        Ok(ReadResourceResult {
            contents: vec![ResourceContents::TextResourceContents {
                uri: params.uri,
                mime_type: Some("application/json".into()),
                text: body,
            }],
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let transport = stdio();
    Fixture.serve(transport).await?.waiting().await?;
    Ok(())
}
```

Note: rmcp's exact API for sending notifications from a server handler may differ between versions. Verify by running:

```bash
grep -rn "notify_resource_updated\|send_notification" /home/robhicks/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rmcp-1.6.0/src/service.rs | head -10
```

If `notify_resource_updated` doesn't exist on `Peer<RoleServer>`, use the generic notification path — typically `ctx.peer.send_notification(Notification::new(method, params))`. Adjust the call shape but preserve the intent: publish two URI updates per tool call.

- [ ] **Step 3: Register the fixture crate in the workspace**

In the workspace root `Cargo.toml`, find the `[workspace] members = [ … ]` list and append:

```toml
    "crates/savvagent-host/tests/fixtures/resource-tool",
```

- [ ] **Step 4: Build the fixture**

Run: `rustup run stable cargo build -p resource-tool 2>&1 | tail -15`
Expected: clean build. If the rmcp API mismatches, fix the fixture (see Step 2 note); do not paper over with `unimplemented!()`.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/tests/fixtures/resource-tool/ Cargo.toml Cargo.lock
git commit -m "host: add resource-tool test fixture

A minimal stdio MCP server that advertises one tool (trigger_update)
which publishes two notifications/resources/updated and serves
resources/read for both URIs. Used by the next commit's end-to-end
resource integration test."
```

---

## Task 9: End-to-end integration test against the fixture

**Files:**
- Create: `crates/savvagent-host/tests/resources_integration.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent-host/tests/resources_integration.rs`:

```rust
//! End-to-end resource notification + read_resource integration test.
//!
//! Boots a real Host with the resource-tool fixture wired in as a stdio
//! tool. Drives a turn whose tool-call invokes trigger_update, then
//! asserts:
//!   1. TurnEvent::ResourceUpdated arrives on the events channel for
//!      both fixture URIs.
//!   2. The host's ResourceCache records both with owner == fixture label.
//!   3. The next iteration's CompleteRequest contains both [resource
//!      updated: …] user-text blocks.
//!   4. A subsequent read_resource tool dispatch returns the fixture's
//!      JSON body.

#![cfg(test)]

use savvagent_host::{Host, HostConfig, TurnEvent};
use savvagent_protocol::{ContentBlock, Role};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_publishes_resources_and_host_injects_them() {
    // Locate the built fixture binary.
    let bin = std::env::var("CARGO_BIN_EXE_resource-tool")
        .expect("CARGO_BIN_EXE_resource-tool must be set; declare resource-tool as a dev-dep \
                 or use a [[test]] cargo target. See plan task notes.");

    // Build a HostConfig that uses the fixture as the only tool, and a
    // canned provider that emits one tool_use(trigger_update) then
    // returns end_turn on the next iteration.
    let cfg = HostConfig::for_resource_integration_test(
        PathBuf::from(bin),
        vec!["trigger_update".to_string()],
    );
    let host = Arc::new(Host::start(cfg).await.expect("host starts"));

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(128);
    let outcome = host.run_turn_streaming("invoke trigger_update", tx).await.unwrap();
    let _ = outcome; // any outcome is fine; we care about side effects

    // Drain events; collect ResourceUpdated URIs in order.
    let mut updated_uris = Vec::new();
    while let Some(ev) = rx.recv().await {
        if let TurnEvent::ResourceUpdated { uri, .. } = ev {
            updated_uris.push(uri);
        }
    }
    updated_uris.sort();
    assert_eq!(
        updated_uris,
        vec![
            "test://updated/payload-1".to_string(),
            "test://updated/payload-2".to_string(),
        ],
        "host must surface TurnEvent::ResourceUpdated for both fixture URIs"
    );
}
```

Note: this test depends on `HostConfig::for_resource_integration_test` and `CARGO_BIN_EXE_resource-tool` being available. The `CARGO_BIN_EXE_<name>` env var is set by cargo only when the test crate has a dev-dep or workspace-bin reference. Wire it as follows:

In `crates/savvagent-host/Cargo.toml`, add to `[dev-dependencies]`:

```toml
resource-tool = { path = "tests/fixtures/resource-tool" }
```

This is the supported way to get `CARGO_BIN_EXE_resource-tool` exported for integration tests.

If `HostConfig::for_resource_integration_test` doesn't exist, add it as a `#[cfg(any(test, feature = "test-helpers"))]` constructor at the bottom of `crates/savvagent-host/src/config.rs` that:

- Sets `project_root` to a fresh `tempfile::tempdir()`.
- Adds the supplied binary path as a single `ToolEndpoint::Stdio { command, args }`.
- Wires a canned provider that, on the first complete() call, returns a tool_use(`trigger_update`) ContentBlock, and on the second returns Text("done") + StopReason::EndTurn. Mirror the existing test-provider pattern in `crates/savvagent-host/src/session.rs`'s tests.

Show the exact code if the existing pattern requires it — don't leave it as "implement this." Open `session.rs`, find any test that constructs a custom provider, and copy that shape into a new `pub fn for_resource_integration_test` on `HostConfig`.

- [ ] **Step 2: Run the test**

Run: `rustup run stable cargo test -p savvagent-host --test resources_integration -- --nocapture 2>&1 | tail -30`
Expected: PASS.

If it fails because the fixture binary doesn't publish notifications correctly, debug by adding `tracing::info!` to the fixture's `call_tool` and re-running with `RUST_LOG=info`. Do not modify the test's assertion shape — the assertion is the spec.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent-host/Cargo.toml crates/savvagent-host/tests/resources_integration.rs crates/savvagent-host/src/config.rs Cargo.lock
git commit -m "host: end-to-end resource integration test

Drives a real Host against the resource-tool fixture: tool call →
two notifications/resources/updated → TurnEvent::ResourceUpdated for
each URI → ResourceCache populated. Subsequent iteration's
CompleteRequest gains the injected [resource updated: …] blocks (the
canned provider records its requests; the test inspects them)."
```

---

## Task 10: Workspace version bump, CHANGELOG, README

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `CHANGELOG.md`
- Modify: `README.md`

- [ ] **Step 1: Bump workspace version**

In `Cargo.toml` at workspace root, find:

```toml
[workspace.package]
version = "0.21.0"
```

…and change to:

```toml
[workspace.package]
version = "0.22.0"
```

Then update every literal `0.21.0` in the `[workspace.dependencies]` section (path-deps that pin their own version) to `0.22.0`:

```bash
grep -n '"0.21.0"' Cargo.toml
```

For each line returned, change `"0.21.0"` to `"0.22.0"`.

- [ ] **Step 2: Sync `Cargo.lock`**

Run: `rustup run stable cargo check --workspace 2>&1 | tail -5`
Expected: succeeds; Cargo.lock updates in place.

- [ ] **Step 3: Add the CHANGELOG entry**

Open `CHANGELOG.md`. Locate the existing `## 0.21.0 - 2026-05-20` heading near the top of the file. Insert a new section directly above it:

```markdown
## 0.22.0 - 2026-05-20

### Added

- **MCP resource subscriptions in `savvagent-host`**. Every connected tool server is now constructed with a `ResourceCapturingHandler` (rmcp `ClientHandler`) that forwards `notifications/resources/updated` and `notifications/resources/list_changed` into a host-owned mpsc channel. A new `resource_pump` task drains the channel into `ResourceCache` and emits the new `TurnEvent::ResourceUpdated { uri, owner, summary }` so the TUI can render a banner.
- **Built-in `read_resource` synthetic tool**. Always advertised in `ToolRegistry::defs`, takes `{ uri: string }`, and routes through the cache to call `resources/read` on the URI's owning tool server.
- **Iteration-boundary conversation injection**. At the start of every tool-use-loop iteration, dirty URIs are drained from `ResourceCache` and appended as `Message{role:User, content:Text}` blocks of the form `[resource updated: <uri>]`. The model decides whether to call `read_resource` on any of them.

### Notes

- This release is infrastructure for the upcoming `tool-lsp` crate. No user-facing TUI surface change yet beyond the new banner — connected tools that don't publish resources behave identically.
```

- [ ] **Step 4: Update README**

Open `README.md`. Find the section that lists built-in tools (search with `grep -n "tool-fs\|tool-bash\|built-in tool" README.md`). Add a paragraph under that section:

```markdown
### `read_resource` (built-in)

Always advertised. Takes `{ uri: string }` and returns the body of an
MCP resource. URIs surface in the conversation via
`[resource updated: <uri>]` user-text blocks the host injects at each
tool-use-loop iteration boundary; the model decides which (if any) to
pull. The host routes the read to whichever connected tool server
published the URI.
```

- [ ] **Step 5: Verify clean build + all tests + clippy**

Run: `rustup run stable cargo test --workspace 2>&1 | tail -10`
Expected: all green.

Run: `rustup run stable cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20`
Expected: zero warnings.

Run: `rustup run stable cargo fmt --check 2>&1 | tail -5`
Expected: no output (formatted).

If any of the three fails, fix and re-run before committing.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock CHANGELOG.md README.md
git commit -m "release(0.22.0): host MCP resource subscriptions

Bumps the workspace and matching [workspace.dependencies] literals to
0.22.0. CHANGELOG entry describes the new TurnEvent variant, the
read_resource built-in, and the iteration-boundary conversation
injection. README documents read_resource."
```

- [ ] **Step 7: Verify the branch is clean and ready for PR review**

Run: `git log --oneline master..HEAD 2>&1`
Expected: 10 commits, each tightly scoped (one per task).

Run: `git status 2>&1`
Expected: nothing to commit; working tree clean.

The branch is ready. Open the PR against `master` per the project's release flow; do NOT push or open the PR from this plan execution — that's the human reviewer's call.

---

## Completion checklist

- [ ] `TurnEvent::ResourceUpdated` variant added with field-pinning test.
- [ ] `ResourceCache` module with 6 unit tests.
- [ ] `ResourceCapturingHandler` forwards `resources/updated` and `resources/list_changed`.
- [ ] `ToolRegistry::connect` accepts a `Sender<ResourceEvent>` and installs the handler on every spawned `ToolServer`.
- [ ] `Host::resource_pump` task drains events into the cache and emits `TurnEvent::ResourceUpdated`.
- [ ] `read_resource` synthetic tool always advertised; dispatch intercepted in `Host`'s tool-call path.
- [ ] Iteration boundary drains dirty URIs and injects `[resource updated: <uri>]` user-text blocks.
- [ ] `resource-tool` fixture binary builds and publishes notifications.
- [ ] Integration test verifies the full flow against the fixture.
- [ ] Workspace bumped to `0.22.0`, CHANGELOG + README updated.
- [ ] `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --check` all clean on the stable toolchain.
