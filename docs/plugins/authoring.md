# Authoring external plugins

This is the long-form guide to writing a savvagent external plugin. The
canonical contract is the design spec at
[`docs/superpowers/specs/2026-05-25-external-plugins-design.md`](../superpowers/specs/2026-05-25-external-plugins-design.md);
this document is meant to be read end-to-end by anyone shipping a
plugin.

External plugins are WebAssembly Component-Model modules that load into
Savvagent at startup and adapt — via per-world adapters in
`savvagent-plugin-wasm` — into the same `Box<dyn Plugin>` slot the
built-in plugins live in. The host doesn't know which of its plugins
are built in and which were loaded from `.wasm`.

> Targets v0.18.0 of Savvagent. The WIT package version is
> `savvagent:plugin@0.1.0`. Manifests should pin `savvagent = "^0.18"`.

## Contents

1. [Quickstart: ship a static plugin in 10 minutes](#quickstart-ship-a-static-plugin-in-10-minutes)
2. [WIT contract reference](#wit-contract-reference)
3. [Capabilities by world](#capabilities-by-world)
4. [Trust and install flow](#trust-and-install-flow)
5. [Three-strikes recovery](#three-strikes-recovery)
6. [Limitations in v0.18.0](#limitations-in-v0180)

---

## Quickstart: ship a static plugin in 10 minutes

This walkthrough mirrors [`examples/plugin-hello-static/`](../../examples/plugin-hello-static/).
At the end you will have a `.wasm` that contributes a `/hello` slash
command which pushes a `Hello from WASM!` toast.

### 0. Prerequisites

- Rust toolchain (stable, edition 2024 capable).
- `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`.
- `cargo-component`: `cargo install cargo-component --locked` (pinned to
  `0.21.1` in the in-tree examples; later versions tend to work but
  the bindings emitter occasionally re-shapes).
- Savvagent v0.18.0 or later installed and able to launch.

### 1. Scaffold the crate

Create a new directory **outside** any Cargo workspace. cargo-component
wants its own profile and target setup that clashes with workspace
release configs:

```
my-plugin/
├── Cargo.toml
├── plugin.toml
└── src/
    └── lib.rs
```

`Cargo.toml`:

```toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2024"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.41"
wit-bindgen-rt = { version = "0.44", features = ["bitflags"] }

[package.metadata.component]
package = "savvagent:plugin"

[package.metadata.component.target]
path = "/absolute/path/to/savvagent/crates/savvagent-plugin-wit/wit"
world = "plugin-static"

[profile.release]
opt-level = "s"
lto = true
codegen-units = 1
strip = "symbols"

# Keep this crate's target/ out of any parent workspace's auto-discovery.
[workspace]
```

The `[package.metadata.component.target]` path should point at the
checked-in WIT files in your Savvagent clone. Once Savvagent publishes
its WIT contract as a versioned package you can drop the local path.

### 2. Write the plugin

`src/lib.rs`:

```rust
#[allow(warnings)]
mod bindings;

use bindings::Guest;
use bindings::savvagent::plugin::types as t;

struct Component;

impl Guest for Component {
    fn manifest() -> Result<t::PluginManifest, t::PluginError> {
        Ok(t::PluginManifest {
            id: "myorg.my-plugin".to_string(),
            name: "My Plugin".to_string(),
            version: "0.1.0".to_string(),
            description: "Says hello from WASM.".to_string(),
            kind: t::PluginKind::Optional,
            contributions: t::Contributions {
                slash_commands: vec!["hello".to_string()],
                hooks: vec![],
                screens: vec![],
                render_slots: vec![],
                keybindings: vec![],
                themes: false,
            },
        })
    }

    fn handle_slash(
        name: String,
        _args: Vec<String>,
    ) -> Result<Vec<t::Effect>, t::PluginError> {
        if name == "hello" {
            return Ok(vec![t::Effect::PushNote(t::Note {
                text: "Hello from WASM!".to_string(),
                level: t::NoteLevel::Info,
            })]);
        }
        Ok(Vec::new())
    }

    fn on_event(_event_json: String) -> Result<Vec<t::Effect>, t::PluginError> {
        Ok(Vec::new())
    }

    fn render_slot(_slot_id: String, _area: t::Region) -> Vec<t::StyledLine> {
        Vec::new()
    }

    fn themes() -> Vec<t::ThemeEntry> {
        Vec::new()
    }
}

bindings::export!(Component with_types_in bindings);
```

The four exported methods are required by the `plugin-static` world.
`handle_slash` is the only one doing anything; the others are
deliberately no-ops. cargo-component generates `src/bindings.rs` at
build time from the WIT files referenced in `Cargo.toml`.

### 3. Author the manifest

`plugin.toml`:

```toml
[plugin]
id = "myorg.my-plugin"
name = "My Plugin"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
description = "Says hello from WASM."

[exports]
slash-commands = ["hello"]
```

The `id` must equal the directory name and the manifest returned by
the plugin's `manifest()` export. The loader rejects mismatches.

### 4. Build

```bash
cargo component build --release --target wasm32-unknown-unknown
```

The resulting `.wasm` lands at
`target/wasm32-unknown-unknown/release/my_plugin.wasm`.

### 5. Install locally for testing

```bash
mkdir -p ~/.savvagent/plugins/myorg.my-plugin
cp target/wasm32-unknown-unknown/release/my_plugin.wasm \
   ~/.savvagent/plugins/myorg.my-plugin/plugin.wasm
cp plugin.toml ~/.savvagent/plugins/myorg.my-plugin/plugin.toml
```

Launch Savvagent. The plugin will show as `untrusted` in `/plugins`.
Run `/plugins trust myorg.my-plugin`, confirm the trust prompt, and
restart. `/hello` is now wired.

### 6. Ship it

For distribution, host both files on a stable URL and document the
`plugin.toml` URL as the install target. Users run:

```
/plugins install https://example.com/my-plugin/plugin.toml
```

The manifest must reference the absolute `wasm = "..."` URL inside its
`[plugin]` block. The install flow re-fetches both, hashes the staging
tree, and asks the user to trust the hash.

---

## WIT contract reference

The contract lives in
[`crates/savvagent-plugin-wit/wit/`](../../crates/savvagent-plugin-wit/wit/):

| File | Defines | Used by |
|---|---|---|
| [`shared.wit`](../../crates/savvagent-plugin-wit/wit/shared.wit) | `Effect`, `PluginError`, `HookKind`, `Region`, `ThemeColor`, `PluginManifest`, `Contributions`, `Note`, `StyledLine`, `ThemeEntry`, key-event types | all three worlds |
| [`spp.wit`](../../crates/savvagent-plugin-wit/wit/spp.wit) | SPP types mirrored from `savvagent-protocol` — `CompleteRequest`, `CompleteResponse`, `StreamEvent`, `ModelInfo`, `ProviderError`, etc. | `plugin-provider` only |
| [`plugin-static.wit`](../../crates/savvagent-plugin-wit/wit/plugin-static.wit) | `world plugin-static` — exports for slash / hook / theme / render-slot / keybinding | static plugins |
| [`plugin-interactive.wit`](../../crates/savvagent-plugin-wit/wit/plugin-interactive.wit) | `world plugin-interactive` — `create-screen` plus the `screen-instance` resource (`on-key`, `render`, `tips`) | interactive plugins |
| [`plugin-provider.wit`](../../crates/savvagent-plugin-wit/wit/plugin-provider.wit) | `world plugin-provider` — `complete` / `list-models` / `count-tokens` exports plus the host-imported `http-capability`, `keyring-capability`, `progress-capability` interfaces | provider plugins |

### Manifest schema (full)

```toml
[plugin]
id = "myorg.thing"               # required; <org>.<name>, lowercase kebab per segment;
                                 # must equal the parent directory name
name = "Human-readable name"
version = "0.2.0"                # semver
world = "plugin-static"          # required: plugin-static | plugin-interactive | plugin-provider
description = "..."
homepage = "https://..."
license = "MIT OR Apache-2.0"
authors = ["..."]
savvagent = "^0.18"              # required; WIT contract version range
wasm = "https://example.com/v0.2.0/plugin.wasm"
                                 # required when fetched via /plugins install <toml-url>;
                                 # ignored on already-installed plugins

[exports]                        # declarative — loader compares against actual WIT exports
slash-commands = ["thing"]       # static-world
hooks = ["HostStarting", "TurnComplete"]
themes = true
render-slots = ["home.tips"]
keybindings = []
# interactive-only:
screens = ["thing.main"]
# provider-only:
provider-id = "myorg"            # required; appears in PROVIDERS, /connect lists it

[security]                       # provider-world only; rejected on static/interactive
allowed-hosts = ["api.example.com"]    # exact-match only in v1; no wildcards
keyring-accounts = ["myorg"]            # accounts the plugin may read via keyring.get

[runtime]
call-timeout-ms = 5000           # optional; default 5000; cap 300000
```

The loader validates the manifest, compares declared `[exports]`
against the actual WIT exports of the `.wasm`, and rejects mismatches
at instantiation time. Declarative mismatches surface as
`[plugins] skipped: <id>: ...` log lines (the rest of the plugin batch
still loads).

---

## Capabilities by world

The wasmtime `Linker` for each world adds only the host imports that
world is entitled to. A wasm module that imports an unavailable host
function fails at instantiation — there is no way for a `plugin-static`
plugin to call out to the network at all.

| Capability | static | interactive | provider |
|---|---|---|---|
| `log(level, msg)` | yes | yes | yes |
| `current-theme()` | yes | yes | no |
| `http.fetch` / `http.fetch-stream` | no | no | yes (allow-host filtered) |
| `keyring.get` | no | no | yes (allow-account filtered) |
| `progress.emit-stream-event` | no | no | yes |

A few notes on the gating:

- `http.*` is exposed only to `plugin-provider`. Even there, the host
  parses the request URL, extracts the canonical host (case-insensitive
  match), and rejects any host that is not in the manifest's
  `[security] allowed-hosts` list with `http-error::denied-host(host)`.
  Wildcards (`*.example.com`) are deferred to a later release.
- `keyring.get` always reads service `savvagent` (the same store
  `/connect` writes to). The `account` argument must appear in the
  manifest's `[security] keyring-accounts` list. Denied reads surface
  as `keyring-error::denied(account)`.
- `progress.emit-stream-event` forwards SPP `StreamEvent`s into the
  active `StreamEmitter` for the in-flight `complete` call. It is
  fire-and-forget and matches the in-process `ChannelEmitter`
  semantics. Calling it outside a `complete` call is a no-op.
- All host imports are capped per call by wasmtime's
  `epoch_interruption` mechanism. Default 5 seconds; overridable via
  `[runtime] call-timeout-ms` up to 300 seconds. A long-running
  `complete` call legitimately runs for minutes because the cap is
  per host-import call, not per outer export call.
- The `screen-instance::render` import on `plugin-interactive`
  receives a `region` but in v0.18.0 the host buffer is not exposed
  through draw primitives — the world's `render` body returns `()`
  and the host uses cached `tips()` plus on-key effects to drive the
  screen. Buffer-write primitives are reserved in the WIT but not
  linked in v0.18.0 (see [Limitations](#limitations-in-v0180)).

The full table is in
[design spec §5](../superpowers/specs/2026-05-25-external-plugins-design.md).

---

## Trust and install flow

### Tree hash

Every loaded plugin is identified by a SHA-256 over its entire
directory tree:

1. The loader walks the plugin's directory (`plugin.toml`,
   `plugin.wasm`, any `assets/` subtree).
2. Filenames are sorted UTF-8 ascending.
3. For each file the hash absorbs the filename, a NUL byte, and the
   file contents.
4. The final digest is the `sha256-tree` value persisted to
   `plugin-trust.toml`.

On every plugin load the host re-hashes the tree and compares it to
the recorded `sha256-tree`. A mismatch flips `trusted = false`, logs a
warning, surfaces the plugin as `hash-mismatch` in `/plugins`, and
forces the user to re-trust before the plugin loads again. This catches
both accidental local edits and any post-install tampering.

### `/plugins install <toml-url>`

1. Fetch `plugin.toml` over TLS, 64 KB cap.
2. Parse and validate the manifest (id format, `savvagent` range,
   declared exports vs world).
3. Fetch the `wasm` URL the manifest references, 32 MB cap, TLS only.
4. Hash the staging tree (`plugin.toml` + `plugin.wasm`).
5. Open the trust-prompt modal showing manifest fields, source URL,
   and the computed tree hash.
6. On **confirm**: write the trust record to
   `~/.savvagent/plugin-trust.toml`, atomic-move the staging directory
   into `~/.savvagent/plugins/<id>/`, push a `plugin <id> installed`
   note. The plugin is picked up on the next startup (or after
   `/plugins enable <id>` if disabled).
7. On **reject**: delete the staging directory; no state changes.

### `plugin-trust.toml`

`~/.savvagent/plugin-trust.toml` is the trust ledger. It is separate
from sub-project A's `~/.savvagent/trusted-projects.json` because
plugins are executable code and the semantics differ:

```toml
[plugins."myorg.thing"]
trusted = true
sha256-tree = "a3f5e7…"            # whole-tree hash at trust time
trusted-at = 1768176000             # unix timestamp
source-url = "https://example.com/v0.2.0/plugin.toml"
disabled-reason = ""                # "manual" or "repeated-traps" when disabled
```

The file is user-scope only — there is no project-local equivalent.
File permissions are 0o600 on Unix; the parent directory is 0o700.
Missing file means no plugins are trusted; first-launch users see the
default empty state.

### Trust state machine

| State | Cause | What happens on load |
|---|---|---|
| trusted, hash matches | normal | Plugin loads |
| trusted, hash mismatch | tampered or edited | Auto-revoked; surfaced as `hash-mismatch`; user re-trusts |
| untrusted (never trusted) | new plugin on disk | Skipped; appears in `/plugins`; user runs `/plugins trust <id>` |
| trusted, `disabled-reason` non-empty | `/plugins disable` or auto-disable | Skipped; surfaced with reason; `/plugins enable <id>` clears it |

---

## Three-strikes recovery

WebAssembly traps are non-fatal to Savvagent — the trap is caught by
wasmtime, surfaced to the host as a `PluginError::Unsupported(trap-info)`,
and the long-lived store for that plugin is dropped. The next call into
the plugin lazily rebuilds the store from the same pre-instance. State
that was held in the plugin between calls is **lost across a trap**
(documented consequence).

If the same plugin traps **three times within a rolling 10-minute
window**, the host auto-disables it:

1. Plugin is marked `disabled` in the in-memory registry; no further
   calls are dispatched.
2. `disabled-reason = "repeated-traps"` is persisted to
   `plugin-trust.toml`.
3. A `PushNote("plugin <id> auto-disabled after repeated traps")` is
   surfaced.
4. The plugin is skipped on every subsequent load until the user runs
   `/plugins enable <id>`, which clears `disabled-reason`. The trust
   record is untouched.

The same code path catches timeouts (wasmtime's `epoch_interruption`
cancels a long-running call after `[runtime] call-timeout-ms`) and
capability denials that escape user code. The strike counter is
per-process; restarting the TUI resets it.

Authors targeting this mechanism: anywhere your plugin can panic
(`unwrap()`, OOB index, integer overflow) is a strike. Treat the
strike budget as a circuit breaker and prefer returning
`PluginError::Unsupported` over panicking.

---

## Limitations in v0.18.0

These mirror the non-goals from
[design spec §7](../superpowers/specs/2026-05-25-external-plugins-design.md).
They are not bugs; they are deliberate scope boundaries.

- **No `SAVVAGENT.md` exposure to plugins.** The project context is
  host-private. Plugins receive only `TurnCtx` / `ScreenOpenCtx` /
  `HookPayload`.
- **No wildcards in `allowed-hosts`.** v0.18.0 enforces exact-match
  only on the URL host (case-insensitive). `*.example.com`-style
  globs are deferred.
- **No `http` for `plugin-static` or `plugin-interactive`.** Only
  `plugin-provider` plugins get the `http-capability` import. Static
  and interactive plugins that need to talk to a network service
  must factor that work into a sibling provider plugin (or wait
  until a later release widens the capability set).
- **No streaming-delta hooks** (`on_token`, `on_chunk`). The v0.9.0
  carve-out still applies; streaming surfaces only through provider
  `progress.emit-stream-event`.
- **No WASM subagents.** `Effect::RegisterInProcessTool` stays
  in-process-only. Wasm plugins *can* react to the `SubagentStop`
  hook fired by parent agent runs.
- **No WASM stdio MCP tools.** Tools remain stdio child processes
  owned by `ToolRegistry`. A wasm tool surface is a separate
  sub-project.
- **No plugin → plugin direct calls.** All cross-plugin communication
  goes through host dispatchers — typically `Effect::RunSlash` for
  invocations and the hook bus for events.
- **No hot reload.** Editing the manifest or `.wasm` requires
  restarting the TUI. `/plugins reload <id>` is post-v0.18.0.
- **No registry or index.** `/plugins install <toml-url>` with a
  URL is the only install path. There is no `/plugins search`,
  no `/plugins update`, no auto-update.
- **No code signing.** Trust is `SHA-256 tree hash` + user consent.
  Sigstore, provenance, and code-signing are post-v0.18.0
  enhancements.
- **No CPU / memory limits.** `epoch_interruption` caps wall time
  per host-import call; wasmtime's `ResourceLimiter` is not wired
  in v0.18.0. A pathological wasm allocator can OOM the host.
- **Single-file distribution.** v0.18.0 ships exactly one
  `plugin.toml` + one `plugin.wasm` per plugin URL. The `assets/`
  subdirectory is reserved in the spec but the multi-file fetcher
  is post-v0.18.0.
- **Interactive draw primitives are not linked in v0.18.0.** The
  WIT reserves `draw-text` / `draw-block` / `draw-line` /
  `clear-area` host imports for the `plugin-interactive` world,
  but the buffer bridge is intentionally deferred. v0.18.0 supports
  interactive plugins whose `render` body is a no-op and whose
  visible content comes from cached `tips()` and on-key `Effect`s.
- **`Effect::RegisterMcpServer`** and `HookKind::PreCompact` appear
  in the WIT surface but never fire in v0.18.0. The loader rejects
  manifests that depend on them.

When in doubt: the
[design spec](../superpowers/specs/2026-05-25-external-plugins-design.md)
is canonical. File an issue if anything in this guide disagrees with
the spec.
