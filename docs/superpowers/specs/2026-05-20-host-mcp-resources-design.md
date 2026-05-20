# Host MCP Resources — design

Date: 2026-05-20
Status: pending review
Roadmap: prerequisite for `tool-lsp` (see `2026-05-20-tool-lsp-design.md`)
Related: nothing currently — host has no resource surface today.

## Problem

Tools today are strictly request/response: `ToolRegistry::call_with_bash_net_override`
sends a `tools/call`, waits for the `CallToolResult`, hands the payload
back to the model. Anything a tool wants to communicate must be wrapped
in a tool-call response.

This shape fits filesystem and shell tools well, but it doesn't fit
**stateful** tools that produce server-initiated updates. The two
concrete cases on the horizon:

- **LSP diagnostics.** Language servers push `publishDiagnostics` over
  their JSON-RPC link whenever they reanalyze. The model needs visibility
  into "the file you just edited now has 3 new errors" without polling.
- **Future: file watchers, build watchers, test runners.** All push
  data on their own schedule.

MCP's wire protocol already covers this through **resources**: a server
publishes `notifications/resources/list_changed` and
`notifications/resources/updated`, and clients fetch with
`resources/read`. The host doesn't consume any of it today — every
`ToolServer` is instantiated as `RunningService<RoleClient, ()>` where
the `()` handler silently drops every server-initiated message.

## Approach

Three pieces, each independently shippable but landing in order:

1. **Client handler that captures resource events.** Replace `()` in
   `ToolServer` with a small `ResourceCapturingHandler` struct that
   forwards `notifications/resources/updated` and
   `notifications/resources/list_changed` into an mpsc channel owned by
   the registry. Tools that don't publish resources cost nothing — the
   channel sees zero traffic.

2. **Host-side resource cache + `TurnEvent::ResourceUpdated`.** The
   registry's resource channel feeds into `Host`, which maintains a
   `HashMap<Uri, ResourceSnapshot>` keyed by URI. Each `updated`
   notification emits a `TurnEvent::ResourceUpdated { uri, summary }`
   so the TUI can render a banner ("3 new diagnostics in
   src/foo.rs"), and stamps the cache with "needs refresh on next
   read."

3. **Synthetic `read_resource` tool.** The host registers a built-in
   tool named `read_resource` that takes `{ uri }`, looks up the URI's
   owning tool server, calls `resources/read` on it, and returns the
   contents as a tool result. This is the model's pull path. Pairs
   with…

4. **Conversation injection on update.** When a `resources/updated`
   arrives between iterations of the tool-use loop (i.e., the model
   has finished its turn and the host is computing the next prompt),
   the host injects one synthetic `user`-role text block:

   > `[resource updated: lsp://diagnostics/src/foo.rs]`

   …into the next iteration's history. The model decides whether to
   call `read_resource` on it. Updates that arrive *during* a model
   round-trip are queued and injected at the next iteration boundary —
   we never race a streaming response.

The fourth piece is the load-bearing design choice: it makes resources
**model-visible** without changing the SPP wire format. The model sees
a normal text input, decides whether to act, and pulls via a normal
tool call. No new content-block kind. No streaming-format changes.

## Why not extend `StreamEvent`?

Considered and rejected. `StreamEvent` is the **provider→host**
streaming vocabulary; resources are a **tool→host** push. Mixing them
would conflate two unrelated streams (the provider's token stream and
the tool subsystem's notification stream) on the same enum and force
every provider impl to think about resource events it doesn't produce.
Keeping resources on `TurnEvent` (host→TUI) and on conversation
injection (host→model) preserves the layering.

## Module layout

```
crates/savvagent-host/src/
├── tools.rs               # ToolRegistry (existing) — gains ResourceCapturingHandler
├── resources.rs           # NEW: ResourceCache, ResourceSnapshot, owner-tracking
└── session.rs             # Host — owns ResourceCache; TurnEvent::ResourceUpdated;
                             # injects updated-resource notes into history
```

A focused new module (`resources.rs`) keeps `tools.rs` from sprouting a
second responsibility. The synthetic `read_resource` tool is dispatched
from `session.rs`'s tool-call path before falling through to the
registry — same shape as how a future `tool-bash` synthetic tool would
work.

## Data flow

```
tool-server child (e.g. tool-lsp)
        │  notifications/resources/updated { uri }
        ▼
ResourceCapturingHandler::on_notification
        │  (channel send)
        ▼
ToolRegistry::resource_rx  →  Host::resource_pump task
        │
        ├──▶ resource_cache.mark_dirty(uri)
        ├──▶ resource_cache.set_owner(uri, tool_label)
        └──▶ TurnEvent::ResourceUpdated { uri, summary }

Next iteration boundary in run_turn_streaming:
        │
        └──▶ drain dirty_since_last_iteration → inject one
             "[resource updated: <uri>]" user-text block per URI

Model calls read_resource { uri }:
        │
        ▼
Host::dispatch_tool intercepts name == "read_resource"
        │
        ▼
resource_cache.owner(uri) → tool_label
        │
        ▼
ToolServer::service.read_resource(uri)  (rmcp's ReadResourceRequest)
        │
        ▼
ToolCallOutcome::success(contents serialized)
```

## Wire surface

New built-in tool exposed to providers:

```json
{
  "name": "read_resource",
  "description": "Fetch the contents of an MCP resource by URI. URIs are
                  surfaced via '[resource updated: <uri>]' notes in the
                  conversation. Returns the resource body as text or
                  JSON.",
  "input_schema": {
    "type": "object",
    "properties": { "uri": { "type": "string" } },
    "required": ["uri"]
  }
}
```

This tool is **always present** in `Host`'s `ToolRegistry::defs`,
regardless of whether any connected tool actually publishes resources.
The cost is one stale tool the model never uses; the benefit is the
tool slot has stable visibility — the model can call it the moment a
resource notification arrives, without needing to learn that it
"appeared."

### New `TurnEvent` variant

```rust
TurnEvent::ResourceUpdated {
    uri: String,
    /// Tool that owns the resource (e.g. "tool-lsp").
    owner: String,
    /// Short human-readable summary; the TUI renders this in a banner.
    /// Producers should keep this under 80 chars.
    summary: String,
}
```

`#[non_exhaustive]` is already enforced on `TurnEvent` (per the existing
pattern); no breaking-change concern.

### Resource cache shape

```rust
pub struct ResourceCache {
    /// URI → snapshot. `dirty_since_last_iteration` is reset by Host
    /// each time the tool-use loop boundary fires.
    entries: HashMap<Uri, ResourceSnapshot>,
    dirty_since_last_iteration: HashSet<Uri>,
}

pub struct ResourceSnapshot {
    /// Tool label (matches a ToolServer.label) — used to route reads.
    pub owner: String,
    /// Last `updated_at` we observed (notification ordering, not wall clock).
    pub seq: u64,
}
```

We deliberately do NOT cache bodies. The model fetches on demand; the
cache only knows "where to ask." Body caching is a future optimization
once we have evidence it matters.

## Error handling

- **Unknown URI on `read_resource`.** Return a tool-error outcome with
  payload `"unknown resource: <uri>; no tool advertises ownership"`.
  The model can recover by either retrying after another notification
  or moving on.
- **Owner tool crashed between update and read.** Surface the
  transport error as a tool-error outcome, same shape as today's
  bash-net transport errors. Drop the cache entry — a re-published
  notification will re-add it.
- **Update channel saturated.** `mpsc::Sender::send` from the rmcp
  notification handler is awaited; if the receiver has fallen behind
  beyond the bounded channel's capacity (we'll start with `64`),
  notifications are dropped with a `tracing::warn!`. The model misses
  one update but the cache still has the previous snapshot. We don't
  want to apply backpressure to the tool subprocess — a slow host
  shouldn't stall the language server's reanalysis.

## Permission model

`read_resource` is a host-built-in tool, so it bypasses the
`tool_overrides` map. Per-resource read permissions are out of scope —
if a tool exposes a resource the user has authorized via consenting to
that tool, reads of it inherit that consent. Resources are pull-only;
no side effects.

## Testing

- **Unit (resources.rs).** `ResourceCache::mark_dirty`,
  `set_owner`, `drain_dirty` round-trips. Sequence-number monotonicity.
- **Unit (tools.rs).** A new `mock_tool_resource_handler_test` boots a
  `ResourceCapturingHandler` against an in-process rmcp peer that
  emits one `ResourceUpdatedNotification`; assert the handler forwards
  it on the channel.
- **Integration (session.rs).** End-to-end: a fixture tool server
  (under `tests/fixtures/`) that publishes one resource on tool-call,
  drive `Host::run_turn_streaming`, assert the next iteration's
  conversation history contains the `[resource updated: …]` text
  block and that a follow-up `read_resource` call returns the
  expected body.
- **Negative path.** Unknown URI returns a tool-error outcome; the
  loop continues; no panic.

## PR slicing

1. **PR 1 — handler + channel.** Add `ResourceCapturingHandler`,
   thread the mpsc channel through `ToolRegistry::connect`. No
   `Host` consumption yet; channel drains into a logging sink behind
   a `tracing::debug!`. Unit-tested against an rmcp in-process peer.
2. **PR 2 — cache + `TurnEvent`.** New `resources.rs` module,
   `Host::resource_pump` task, `TurnEvent::ResourceUpdated`. TUI gets
   a one-line banner (gated on log-screen visibility, like the
   existing route badge). No model-side surface yet.
3. **PR 3 — synthetic `read_resource` tool.** Register the built-in,
   route dispatch in `Host`, plumb to `service.read_resource`. No
   conversation injection yet — model has to call it blind.
4. **PR 4 — conversation injection.** Drain
   `dirty_since_last_iteration` at the loop boundary, inject the
   text block. End-to-end integration test against a fixture tool
   server. Workspace version bump to `0.22.0`, CHANGELOG entry,
   README note that adds `read_resource` to the built-in tool list.

This split is intentional: PRs 1–3 are infrastructure with no
user-visible behavior. PR 4 is the smallest change that makes the
feature actually work, with the smallest risk surface because the
plumbing is already in.

## What's explicitly out of scope

- **Body caching in `ResourceCache`.** Pull-on-demand only for v1.
- **Resource list pagination.** `resources/list` is not used by the
  host today — we rely on `updated` notifications for discovery.
- **Per-resource permissions.** Reads inherit the owning tool's consent.
- **Subscribe/unsubscribe to specific URIs.** We listen to every
  notification the tool sends; tools manage their own publish lists.
- **MCP `resources/templates/list`.** Templates (parameterized
  URI schemes) require model-side reasoning we don't need yet;
  diagnostics URIs are concrete.

## Migration / compatibility

`StreamEvent` is unchanged. `CompleteRequest`/`Response` are unchanged.
`ToolDef::input_schema` for `read_resource` is the only new public
surface — additive, no breaking change. SPP wire version stays where
it is.
