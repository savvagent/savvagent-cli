# External plugins (sub-project D) — design

Date: 2026-05-25
Status: drafted, awaiting user review before plan
Supersedes: nothing
Related:
- `docs/superpowers/specs/2026-05-12-v0.9.0-plugin-system-design.md` — the v0.9.0 in-process `Plugin` trait, the 17 built-ins, and the WIT-portability rules (§9) this design redeems
- `docs/superpowers/specs/2026-05-21-user-slash-commands-design.md` — sub-project A; four-path discovery, trust-file pattern
- `docs/superpowers/specs/2026-05-22-user-hooks-design.md` — sub-project B; hook stdin contract, hook kinds, `PreToolUseGate`
- `docs/superpowers/specs/2026-05-23-user-agents-design.md` — sub-project C; subagent dispatch, `SubagentStop`, the `subagent` field added to hook payloads

## Context: the multi-subsystem split

| Order | Sub-project | Status |
|-------|-------------|--------|
| A | User slash commands | shipped (PR #94, v0.17.0 rollup) |
| B | User-defined hooks | shipped (PR #96 + follow-ups in #98, v0.17.0 rollup) |
| C | Agents (subagents via built-in `task` MCP tool) | shipped (PR #99, v0.17.0 rollup) |
| **D** | **External plugins (WASM, honoring v0.9.0's deferred WIT promise)** | *this spec* |

Sub-projects A/B/C ship together as `v0.17.0` immediately before this work
begins; sub-project D ships as `v0.18.0`.

A established the four-path discovery convention
(`<project>/.savvagent/`, `<project>/.claude/`, `~/.savvagent/`,
`~/.claude/`) and the trust-file pattern. B added the hook stdin contract
and the `PreToolUseGate`. C lit up `SubagentStop` and extended hook
payloads with an optional `subagent` field. v0.9.0 designed the
in-process `Plugin` trait under WIT-portability rules so that *one day*
those same plugins could be loaded from WASM.

Sub-project D is that day.

## Problem

The v0.9.0 plugin system has 17 built-in plugins and zero third-party
extension story. Adding a fourth provider, a custom theme pack, or a
project-specific footer badge means forking savvagent. The whole point of
designing `savvagent-plugin`'s trait surface under WIT-portability rules
(closed `Effect` enum, owned types, no callbacks, no `serde_json::Value`
in sigs) was to make external loading mechanical — but v0.9.0 explicitly
shipped no `.wit` file and no loader.

Meanwhile, sub-projects A/B/C have exhausted the "drop-a-text-file"
extension model: slash commands, hooks, and agents are all configurable
without code, but anything that needs *logic* (a custom screen, a new
provider, a theme that reacts to project state) still requires forking.

Sub-project D closes that gap by adding a WASM plugin runtime that:

1. Mechanically translates the v0.9.0 trait surface to WIT.
2. Loads `.wasm` plugins from the same four-path discovery A/B/C use.
3. Trust-gates every plugin via SHA-256 + user consent.
4. Adapts wasm plugins to `Box<dyn Plugin>` so the rest of the host
   doesn't change shape.

## Approach

Two new crates plus one new built-in plugin:

- **`savvagent-plugin-wit`** — leaf crate holding `.wit` files and
  `wit-bindgen`-generated host bindings. Zero dependencies beyond
  `wit-bindgen`.
- **`savvagent-plugin-wasm`** — wasmtime-backed runtime: manifest
  parser, four-path discovery, trust-file management, three adapters
  (one per WIT world) that produce `Box<dyn Plugin>` instances.
- **`internal:plugins`** — new built-in plugin owning the `/plugins`
  slash command, the plugin manager screen, and the install/trust/
  revoke/remove flows.

Three WIT worlds, one per contribution-difficulty class:

| World | Kinds covered | Why this slice |
|---|---|---|
| `plugin-static` | slash, hooks, themes, keybindings, render-slots | Stateless; instantiate-at-startup; tiny host-import surface (`log`, `current-theme`). |
| `plugin-interactive` | screens | Per-open state; per-screen-open `Store`; draw-primitive host imports. |
| `plugin-provider` | providers (`complete`/`list_models`/`count_tokens`) | Network + keyring + streaming-progress capabilities; SPP types crossing the boundary. |

A single `.wasm` file declares exactly one world. Organizations that ship
both themes and screens publish two plugins.

The v0.9.0 in-process `Plugin` trait stays unchanged. Wasm plugins become
`Box<dyn Plugin>` via per-world adapters; `register_external()` appends
them to the registry alongside `register_builtins()`. The rest of the
host — slash dispatch, hook gate, screen stack, providers list — sees
wasm plugins as ordinary plugins.

## Section 1 — User-facing surface

### Discovery paths

Mirror sub-projects A/B/C. First-wins by plugin id, project beats user,
savvagent beats claude:

1. `<project>/.savvagent/plugins/<id>/plugin.toml`
2. `<project>/.claude/plugins/<id>/plugin.toml`
3. `~/.savvagent/plugins/<id>/plugin.toml`
4. `~/.claude/plugins/<id>/plugin.toml`

Each plugin lives in its own directory named after `<id>`. The directory
contains at least `plugin.toml` and (after install) `plugin.wasm`.
Optional `assets/` subtree is resolved at `$PLUGIN_DIR/assets/` from
inside wasm; assets are read-only.

### `plugin.toml` schema

```toml
[plugin]
id = "acme.zenburn-theme"      # required, must equal directory name
                               # format: <org>.<name>, lowercase kebab per segment
name = "Acme Zenburn"
version = "0.2.0"               # semver
world = "plugin-static"         # required: one of plugin-static | plugin-interactive | plugin-provider
description = "..."
homepage = "https://..."
license = "MIT OR Apache-2.0"
authors = ["..."]
savvagent = "^0.18"             # required: WIT contract version range
wasm = "https://github.com/acme/zenburn/releases/download/v0.2.0/plugin.wasm"
                                # required only when fetched via /plugins install <toml-url>;
                                # ignored on already-installed plugins

[exports]                       # declarative — loader compares against actual WIT exports
slash-commands = ["zenburn"]
hooks = ["HostStarting"]
themes = true
render-slots = []
keybindings = []
# interactive-only:
screens = []                    # list of screen-id strings the plugin handles
# provider-only:
provider-id = "acme"            # required for plugin-provider; appears in PROVIDERS

[security]                      # provider-world only; rejected on static/interactive
allowed-hosts = ["api.acme.example"]   # exact-match only in v1; no wildcards
keyring-accounts = ["acme"]             # whitelist of keyring accounts the plugin may read

[runtime]
call-timeout-ms = 5000          # optional; default 5000; cap 300000
```

Loader validates manifest, compares declared `[exports]` against actual
WIT exports of the wasm, rejects mismatches at instantiation time.

### Install flow — `/plugins install <toml-url>`

The argument is a URL pointing at a **plugin.toml** file. Steps:

1. Fetch `plugin.toml` (TLS, 64 KB cap).
2. Parse and validate manifest.
3. Fetch the `wasm` URL referenced by the manifest (32 MB cap, TLS).
4. Compute SHA-256 over the **whole staging directory tree** (`plugin.toml`,
   `plugin.wasm`, any `assets/` files). Order: filenames sorted UTF-8.
5. Open trust-prompt modal showing manifest fields, source URL, and hash.
6. On confirm: write trust record, atomic-move staging into
   `~/.savvagent/plugins/<id>/`, emit `Effect::PushNote("plugin <id> installed")`.
7. On reject: delete staging directory.

Plugins with `assets/` distribution: the manifest references a directory
URL (e.g. `https://example.com/myplugin/`); the loader fetches
`plugin.toml`, `plugin.wasm`, and walks any referenced `assets/`. v0.18.0
ships single-file-only; multi-file distribution is post-v0.18.0.

### Trust file

`~/.savvagent/plugin-trust.toml` — separate from sub-project A's
slash-command trust file (plugins are executable; semantics differ).
User scope only.

```toml
[plugins."acme.zenburn-theme"]
trusted = true
sha256-tree = "a3f5e7…"        # whole-tree hash at trust time
trusted-at = 1768176000         # unix timestamp
source-url = "https://github.com/acme/zenburn/releases/download/v0.2.0/plugin.toml"
disabled-reason = ""            # set to "repeated-traps" or "manual" when disabled
```

On every plugin load:

1. Re-hash the tree.
2. Compare to `sha256-tree`. Mismatch → set `trusted = false`, log warning,
   surface in `/plugins list`; user must re-trust.
3. If `disabled-reason` non-empty, skip load; surface in `/plugins list`.

### `/plugins` slash commands

| Command | Behavior |
|---|---|
| `/plugins` | Opens plugin manager screen (an `internal:plugins` built-in screen). Lists all discovered plugins with trust status, world, exports, source path. |
| `/plugins install <toml-url>` | Install flow above. |
| `/plugins trust <id>` | Opens trust-prompt modal for an untrusted plugin. |
| `/plugins revoke <id>` | Drop trust record. Plugin stays on disk, unloads next start. |
| `/plugins remove <id>` | Revoke + delete plugin directory. |
| `/plugins enable <id>` | Clear `disabled-reason`, re-enable for next start. |
| `/plugins disable <id>` | Set `disabled-reason = "manual"`, unload next start. |

The plugin manager screen is itself a built-in (`internal:plugins`), not a
WASM plugin — must work even when no wasm plugin is trusted.

## Section 2 — Architecture

### New crates

```
crates/
├── savvagent-plugin-wit/         ← .wit files + wit-bindgen host bindings
│   ├── Cargo.toml                ← deps: wit-bindgen only
│   └── wit/
│       ├── shared.wit            ← Effect, HookKind, KeyEvent, Region, ThemeColor, PluginError
│       ├── spp.wit               ← SPP types mirrored from savvagent-protocol
│       ├── plugin-static.wit     ← world plugin-static
│       ├── plugin-interactive.wit ← world plugin-interactive
│       └── plugin-provider.wit   ← world plugin-provider
└── savvagent-plugin-wasm/        ← runtime + adapters
    ├── Cargo.toml                ← deps: wasmtime ~24.0, reqwest, keyring, sha2, toml, walkdir
    └── src/
        ├── lib.rs
        ├── manifest.rs           ← PluginManifest parser + validator
        ├── discovery.rs          ← four-path discovery
        ├── trust.rs              ← plugin-trust.toml management, hashing
        ├── adapter/
        │   ├── mod.rs
        │   ├── static.rs         ← StaticAdapter: Plugin
        │   ├── interactive.rs    ← InteractiveAdapter: Plugin + WasmScreen: Screen
        │   └── provider.rs       ← ProviderAdapter: ProviderClient
        ├── host_imports/
        │   ├── mod.rs
        │   ├── log.rs            ← log() impl
        │   ├── theme.rs          ← current-theme() impl
        │   ├── draw.rs           ← draw-text/draw-block/draw-line/clear-area for interactive
        │   ├── http.rs           ← fetch/fetch-stream with allowed-hosts filter
        │   ├── keyring.rs        ← get() with allowed-accounts filter
        │   └── progress.rs       ← emit-stream-event() forwarding to active emitter
        ├── spp_convert.rs        ← From/Into SPP <-> WIT
        └── register.rs           ← register_external()
```

### `register_all()` change in the TUI

```rust
// crates/savvagent/src/plugin/registry.rs
pub fn register_all(cfg: &PluginConfig) -> Result<PluginRegistry, RegistryError> {
    let mut reg = PluginRegistry::new();
    register_builtins(&mut reg);                 // unchanged from v0.9.0
    register_external(&mut reg, cfg)?;           // new
    Ok(reg)
}
```

`register_external` walks the four discovery paths, validates each
manifest, checks trust, instantiates the appropriate adapter, and pushes
`Box<dyn Plugin>` into the registry. From here the rest of the host is
unaware of which plugins are built-in vs wasm.

### Wasmtime engine sharing

One `wasmtime::Engine` per host process, shared across all loaded
plugins. Engines are cheap to clone (Arc internally) but expensive to
create (~50ms); a single engine amortizes startup. `Store` lifecycle:

- **Static + provider plugins:** one long-lived `Store` per plugin
  instance, shared across all calls. Rebuilt lazily on trap recovery
  (state loss across trap is documented).
- **Interactive plugins:** new `Store` per screen open, dropped on screen
  close. Matches the cancel-revert semantic.

Pre-instantiation via `wasmtime::component::InstancePre` makes per-open
`Store` cheap (microseconds per call).

### Capability surface comparison

| Capability | static | interactive | provider |
|---|---|---|---|
| `log(level, msg)` | ✅ | ✅ | ✅ |
| `current-theme()` | ✅ | ✅ | ❌ |
| `draw-text` / `draw-block` / `draw-line` / `clear-area` | ❌ | ✅ | ❌ |
| `http.fetch` / `http.fetch-stream` | ❌ | ❌ | ✅ (allowed-hosts filtered) |
| `keyring.get` | ❌ | ❌ | ✅ (allowed-accounts filtered) |
| `progress.emit-stream-event` | ❌ | ❌ | ✅ |

Linker for each world adds only the columns marked ✅. Wasm modules that
import an unavailable host function fail at instantiation.

## Section 3 — The three WIT worlds

### `shared.wit`

Mechanically translated from `savvagent-plugin` v0.9.0 §9. All types are
owned, all errors are concrete enums, no callbacks, no
`serde_json::Value`, explicit-width numerics. Sample:

```wit
package savvagent:plugin@0.1.0;

interface types {
    record key-event-portable { code: key-code, modifiers: key-modifiers }
    record region { x: u16, y: u16, width: u16, height: u16 }

    variant theme-color {
        reset, black, red, green, yellow, blue, magenta, cyan, gray,
        dark-gray, light-red, light-green, light-yellow, light-blue,
        light-magenta, light-cyan, white,
        indexed(u8),
        rgb(rgb-color),
    }
    record rgb-color { r: u8, g: u8, b: u8 }
    record text-mods { bold: bool, italic: bool, underline: bool, reverse: bool, dim: bool }

    variant effect {
        push-note(note),
        open-screen(screen-target),
        set-theme(string),
        run-slash(slash-call),
        save-transcript,
        clear-log,
        register-provider(provider-spec),
        register-keybinding(keybinding),
    }

    variant plugin-error {
        invalid-input(string),
        io(string),
        capability-denied(string),
        unsupported(string),
    }

    variant hook-kind {
        host-starting, host-stopping, turn-start, turn-complete,
        pre-tool-use, post-tool-use, subagent-stop, transcript-saved,
    }
}
```

### `plugin-static.wit`

```wit
package savvagent:plugin@0.1.0;

world plugin-static {
    use types.{effect, plugin-error, hook-kind, theme-color, region,
               rendered-span, theme, keybinding, plugin-manifest, log-level};

    import log: func(level: log-level, msg: string);
    import current-theme: func() -> list<tuple<string, theme-color>>;

    export init: func() -> result<plugin-manifest, plugin-error>;
    export handle-slash: func(name: string, args: list<string>)
        -> result<list<effect>, plugin-error>;
    export handle-hook: func(kind: hook-kind, payload-json: string)
        -> result<list<effect>, plugin-error>;
    export render-slot: func(slot-id: string, area: region) -> list<rendered-span>;
    export themes: func() -> list<theme>;
    export keybindings: func() -> list<keybinding>;
}
```

`handle-hook` takes `payload-json: string` rather than a typed payload —
hook payload shapes evolve faster than the WIT contract; one JSON parse
per hook firing is cheap and avoids re-revving WIT every time we add a
hook field.

### `plugin-interactive.wit`

```wit
package savvagent:plugin@0.1.0;

world plugin-interactive {
    use types.{key-event-portable, region, effect, theme-color,
               plugin-error, text-mods, plugin-manifest, log-level};

    import log: func(level: log-level, msg: string);
    import current-theme: func() -> list<tuple<string, theme-color>>;

    // Host-imported draw primitives — wasm calls these during render();
    // host accumulates into the active ratatui Buffer.
    import draw-text: func(x: u16, y: u16, text: string,
                           fg: theme-color, bg: theme-color, mods: text-mods);
    import draw-block: func(area: region, border-style: border-style,
                            title: option<string>);
    import draw-line: func(x1: u16, y1: u16, x2: u16, y2: u16, style: line-style);
    import clear-area: func(area: region, bg: theme-color);

    // Component Model resource: host holds opaque handle; wasm owns state.
    // Drop of the handle = screen closed.
    resource screen-instance {
        on-key: func(key: key-event-portable) -> list<effect>;
        render: func(area: region);             // () return; side effects via draw imports
        tips: func() -> string;
    }

    record screen-open-ctx {
        active-theme: list<tuple<string, theme-color>>,
        terminal-width: u16,
        terminal-height: u16,
    }

    export init: func() -> result<plugin-manifest, plugin-error>;
    export create-screen: func(screen-id: string, ctx: screen-open-ctx)
        -> result<screen-instance, plugin-error>;
}
```

`render` returns `()`; the host sets `active_buffer: Option<&mut Buffer>`
in `Store` state before calling, the wasm side calls `draw-text` etc.,
the host import implementations write into the buffer.

### `plugin-provider.wit`

```wit
package savvagent:plugin@0.1.0;

world plugin-provider {
    use types.{plugin-error, plugin-manifest, log-level};
    use spp.{complete-request, complete-response, stream-event,
             model-info, count-tokens-request, count-tokens-response,
             provider-error, provider-manifest};

    import log: func(level: log-level, msg: string);
    import http: http-capability;
    import keyring: keyring-capability;
    import progress: progress-capability;

    export init: func() -> result<provider-manifest, plugin-error>;
    export complete: func(req: complete-request)
        -> result<complete-response, provider-error>;
    export list-models: func() -> result<list<model-info>, provider-error>;
    export count-tokens: func(req: count-tokens-request)
        -> result<count-tokens-response, provider-error>;
}

interface http-capability {
    record http-request {
        method: string,
        url: string,
        headers: list<tuple<string, string>>,
        body: option<list<u8>>,
        timeout-ms: option<u32>,            // capped at 300_000 by host
    }
    record http-response {
        status: u16,
        headers: list<tuple<string, string>>,
        body: list<u8>,
    }
    variant http-error {
        transport(string), tls(string),
        denied-host(string), denied-method(string),
        oversize, timeout, body-too-large(u64),
    }

    resource http-stream {
        status: func() -> u16;
        headers: func() -> list<tuple<string, string>>;
        next-chunk: func() -> result<option<list<u8>>, http-error>;
    }

    fetch: func(req: http-request) -> result<http-response, http-error>;
    fetch-stream: func(req: http-request) -> result<http-stream, http-error>;
}

interface keyring-capability {
    variant keyring-error { not-found, denied(string), backend(string) }
    // service is always "savvagent" — fixed; only account is parameter.
    // account must be in manifest's keyring-accounts list.
    get: func(account: string) -> result<string, keyring-error>;
}

interface progress-capability {
    // Fire-and-forget; matches ChannelEmitter semantics.
    emit-stream-event: func(event: stream-event);
}
```

## Section 4 — Provider WIT port

This is the section that justifies the v1.0-level scope flag. v0.9.0 §9
said `dyn ProviderClient` was not WIT-portable. We don't *port* it —
we add a parallel WIT surface that mechanically converts to/from SPP,
wrap as `Box<dyn ProviderClient>` on the host side, and leave in-process
providers untouched.

### SPP-in-WIT mechanical translation

`spp.wit` mirrors `savvagent-protocol/src/lib.rs` field-for-field
(estimated ~250 lines). Every SPP type has `From<Rust> for Wit` and
`From<Wit> for Rust` impls in `savvagent-plugin-wasm/src/spp_convert.rs`.

**Round-trip pin:** every fixture under
`savvagent-protocol/tests/fixtures/` round-trips through WIT and back,
byte-equal. One unit test per variant. Property tests on
`CompleteRequest` and `StreamEvent` (the high-fanout types).

### `WasmProviderClient` shim

```rust
// crates/savvagent-plugin-wasm/src/adapter/provider.rs
pub struct WasmProviderClient {
    engine: Engine,
    manifest: Arc<PluginManifest>,
    pre: ProviderPreInstance,
}

#[async_trait]
impl ProviderClient for WasmProviderClient {
    async fn complete(
        &self,
        req: CompleteRequest,
        emitter: Box<dyn StreamEmitter>,
    ) -> Result<CompleteResponse, ProviderError> {
        let mut store = Store::new(&self.engine, ProviderHostState {
            manifest: self.manifest.clone(),
            active_emitter: Some(emitter),
            http_client: shared_reqwest_client(),
        });
        let inst = self.pre.instantiate_async(&mut store).await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let wit_req: wit::CompleteRequest = req.into();
        let result = inst.call_complete(&mut store, &wit_req).await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        result.map(Into::into).map_err(Into::into)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> { /* … */ }
    async fn count_tokens(&self, req: CountTokensRequest)
        -> Result<CountTokensResponse, ProviderError> { /* … */ }
}
```

Per-`complete`-call `Store` — instantiation is cheap via
pre-instantiation; per-call stores guarantee no state leak between
concurrent provider calls. Concurrency is bounded by how many
`/connect`-active wasm providers exist; each one serializes its own calls
via the `WasmProviderClient`'s internal queue.

### Registration into `PROVIDERS`

`crates/savvagent/src/providers.rs::PROVIDERS` is a static slice today.
Wasm providers don't live there. The plugin runtime scans, instantiates
each `plugin-provider`-world plugin's `init` function to read the
`ProviderManifest`, and exposes a runtime-extended `effective_providers()`
via a one-shot `OnceCell<Vec<ProviderSpec>>` that *appends* discovered
wasm providers to the built-in slice. From `/connect`'s point of view, a
wasm provider id is indistinguishable from a built-in id.

`ProviderSpec::build_*` for a wasm provider clones the
`Arc<WasmProviderClient>`.

### Streaming progress — no rmcp gotcha

Built-in providers had the `ProgressDispatcher` subscriber-doesn't-auto-
close problem because they round-tripped through MCP. Wasm providers
don't: they hold the `Box<dyn StreamEmitter>` directly in store state and
call into it synchronously from the `progress::emit-stream-event` host
import. No forwarder task, no `JoinHandle::abort()`.

### Host-import capability gating

```rust
fn http_fetch(state: &mut ProviderHostState, req: HttpRequest)
    -> Result<HttpResponse, HttpError>
{
    let host = parse_url_host(&req.url).map_err(HttpError::Transport)?;
    if !state.manifest.allowed_hosts.contains(&host) {
        return Err(HttpError::DeniedHost(host));
    }
    // reqwest call with TLS via rustls
}

fn keyring_get(state: &mut ProviderHostState, account: String)
    -> Result<String, KeyringError>
{
    if !state.manifest.keyring_accounts.contains(&account) {
        return Err(KeyringError::Denied(account));
    }
    keyring::Entry::new("savvagent", &account)
        .map_err(|e| KeyringError::Backend(e.to_string()))?
        .get_password()
        .map_err(|e| match e {
            keyring::Error::NoEntry => KeyringError::NotFound,
            other => KeyringError::Backend(other.to_string()),
        })
}
```

Service name is hard-coded `"savvagent"`; account must be in
manifest's `keyring-accounts`. Mitigates a malicious provider reading
arbitrary OS keyring entries.

## Section 5 — Built-in plugin migration & coexistence

### Adapters produce `Box<dyn Plugin>`

One adapter per world. `StaticAdapter` and `ProviderAdapter` (the
`ProviderClient` shim from §4) hold long-lived stores; `InteractiveAdapter`
creates per-screen-open stores.

```rust
// adapter/static.rs
pub(crate) struct StaticAdapter {
    engine: Engine,
    manifest: Arc<PluginManifest>,
    pre: StaticPreInstance,
    store: tokio::sync::Mutex<Store<StaticHostState>>,
    instance: StaticInstance,
}

#[async_trait]
impl Plugin for StaticAdapter {
    fn id(&self) -> &str { &self.manifest.id }

    async fn handle_slash(&self, name: &str, args: Vec<String>, _ctx: TurnCtx)
        -> Result<Vec<Effect>, PluginError>
    {
        let mut store = self.store.lock().await;
        let wit_effects = self.instance
            .call_handle_slash(&mut *store, name, &args).await?;
        Ok(wit_effects.into_iter().map(Into::into).collect())
    }

    async fn handle_hook(&self, kind: HookKind, payload: HookPayload)
        -> Result<Vec<Effect>, PluginError>
    {
        let payload_json = serde_json::to_string(&payload)
            .expect("HookPayload always serializes");
        let mut store = self.store.lock().await;
        let wit_effects = self.instance
            .call_handle_hook(&mut *store, kind.into(), &payload_json).await?;
        Ok(wit_effects.into_iter().map(Into::into).collect())
    }
    // themes(), keybindings(), render_slot() — same trampoline pattern
}
```

### `WasmScreen` and the buffer bridge

```rust
struct WasmScreen {
    store: tokio::sync::Mutex<Store<InteractiveHostState>>,
    instance: InteractiveInstance,
    handle: ScreenInstanceResource,
}

#[async_trait]
impl Screen for WasmScreen {
    async fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let mut store = self.store.lock().await;
        let wit_effects = self.handle.call_on_key(&mut *store, key.into()).await
            .unwrap_or_default();
        // Cache tips refresh for next sync tips() call.
        store.data_mut().cached_tips = self.handle.call_tips(&mut *store).await
            .unwrap_or_default();
        wit_effects.into_iter().map(Into::into).collect()
    }

    async fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let mut store = self.store.lock().await;
        store.data_mut().active_buffer = Some(buf as *mut Buffer);  // see invariant
        let _ = self.handle.call_render(&mut *store, area.into()).await;
        store.data_mut().active_buffer = None;
    }

    async fn tips(&self) -> String {
        self.store.lock().await.data().cached_tips.clone()
    }
}
```

`active_buffer` invariant (doc-comment on the field): the pointer is
only valid for the duration of a single `render()` call, set before the
wasm call and cleared after; host-imported draw primitives dereference
it under the lock held by the calling task. Lifetime-wise this is sound
because the borrow is contained inside a synchronous-from-the-host's-
view call, but the unsafe-adjacent shape warrants explicit documentation.

### Coexistence with sub-projects A/B/C

- **Sub-project A (slash commands).** Markdown commands and wasm slash
  handlers live in different plugins. The slash dispatcher iterates the
  registry; first-wins follows registry order (built-ins → wasm →
  markdown). Wasm slash plugins see the same `TurnCtx` as built-ins.

- **Sub-project B (hooks).** The hook gate iterates all registered
  plugins for the kind. Wasm plugins receive `payload-json: string`;
  the optional `subagent` field added in C flows through unchanged
  (JSON layer is backward-compatible). Old plugins ignore unknown keys.

- **Sub-project C (agents).** `RegisterInProcessTool` stays
  savvagent-internal — wasm plugins cannot register subagents. Stated as
  explicit non-goal. Wasm plugins *can* react to `SubagentStop` hooks
  emitted by agent runs.

### Trap recovery & three-strikes-disable

A wasm plugin that traps once does not unload — the trap is surfaced as
`PluginError::Unsupported(trap-info)`, the host emits a `PushNote`, the
plugin's long-lived store is dropped, and the next call lazily rebuilds
the store. State across the trap is lost; documented as a recovery
consequence.

After **3 traps within a rolling 10-minute window**, the plugin is
marked `disabled` in the registry, `disabled-reason = "repeated-traps"`
is written to `plugin-trust.toml`, and the plugin is skipped on next
load. User re-enables via `/plugins enable <id>`.

## Section 6 — Testing, errors, PR slicing

### Four-layer testing strategy

**Layer 1 · WIT type round-trips (pure Rust).**
Every record/variant in `shared.wit` and `spp.wit` gets `From<Rust> for
Wit` and `From<Wit> for Rust` impls with one unit test per direction per
variant. Property tests on `CompleteRequest` and `StreamEvent`. The full
SPP fixture set from `savvagent-protocol/tests/fixtures/` round-trips
byte-equal.

**Layer 2 · Adapter integration tests (wasmtime + committed fixtures).**
Hand-crafted wasm fixtures committed to
`crates/savvagent-plugin-wasm/tests/fixtures/<world>/` (~50 KB each,
≈ 400 KB total binary growth accepted). Reproducible build via
`Justfile`-driven `cargo component build`, but the `.wasm` artifacts
are committed so main CI doesn't require `cargo-component`.

Each fixture exercises every export of its world:
- `static.wasm` — themes, slash echo, hook counter, render-slot stub
- `interactive.wasm` — draws known pattern of text given known key seq
- `provider.wasm` — fixed `CompleteResponse` + scripted stream events

**Layer 3 · Fault-injection fixtures.**
- `trap.wasm` — panics in `handle-slash` → host surfaces
  `PluginError::Unsupported(trap-info)`
- `timeout.wasm` — infinite loop → epoch-interruption cancellation
- `denied-host.wasm` — provider fetches a host not in `allowed-hosts`
- `denied-account.wasm` — provider reads keyring account not whitelisted
- `bad-export.wasm` — manifest declares `themes = true` but no `themes`
  export → loader rejects at validation

**Layer 4 · End-to-end smoke.**
One real example plugin per world under
`examples/plugin-hello-{static,interactive,provider}/`. Built via
`cargo component build` in a **separate CI job** that doesn't gate main
merge (cargo-component has historically had rough edges). Smoke test
spawns savvagent with `SAVVAGENT_HOME=$tmp`, drops example into discovery
path, hits via `cargo run -p savvagent-host --example headless`.

### Error matrix

| Layer | Failure | User sees |
|---|---|---|
| Discovery | Invalid manifest | `[plugins] skipped: <path>: <reason>` log |
| Discovery | Manifest `savvagent` range mismatch | Same, with version reason |
| Trust | Tree-hash mismatch | Trust auto-revoked; warning in `/plugins list`; re-prompt next start |
| Trust | Untrusted on first load | Appears `untrusted` in `/plugins list`; user must `/plugins trust <id>` |
| Instantiation | Missing/wrong WIT exports | `[plugins] skipped: <id>: missing export <name>` |
| Instantiation | Malformed binary, OOM | Same |
| Runtime | Trap | `PluginError::Unsupported(trap)`, `PushNote("plugin <id> crashed: <trap-info>")`, store rebuilt on next call |
| Runtime | Call timeout | Same channel, "timed out after Xms" |
| Runtime | Capability denial | Same channel, "denied capability: <name>" |
| Runtime | 3+ traps in 10min | Auto-disable, `disabled-reason = "repeated-traps"` |

### Per-import-call timeouts

Wasmtime `epoch_interruption` caps wall time per host-import call.
Default 5s for all worlds; overridable per-plugin via
`[runtime] call-timeout-ms = 30000` (cap 300s). Lets a long-running
`complete()` legitimately stream for minutes as long as host imports
keep making progress.

### Single PR — sliced commits

Reviewability comes from commit ordering. Each commit is
`cargo check && cargo test` green on its own:

```
 1. Add savvagent-plugin-wit crate scaffold + shared.wit + spp.wit + CI dep-guard
 2. Add WIT bindings + SPP <-> WIT From/Into + round-trip tests
 3. Add savvagent-plugin-wasm crate scaffold (wasmtime ~24.0, empty modules)
 4. Manifest + four-path discovery + plugin-trust.toml + tree-hash + unit tests
 5. Static-world adapter + log/current-theme imports + static fixture + tests
 6. Interactive-world adapter + draw imports + interactive fixture + tests
 7. Provider-world adapter + http/keyring/progress imports + provider fixture + tests
 8. Fault-injection fixtures + tests (trap, timeout, denied caps, bad-export)
 9. Three-strikes-disable + trap recovery + tests
10. Wire register_external into register_all in TUI
11. internal:plugins built-in + plugin manager screen
12. /plugins slash command (install/trust/revoke/remove/enable/disable/list)
13. Three example plugins under examples/ + separate wasm CI job
14. README + plugin-authoring docs + CHANGELOG entry
15. Version bump in workspace Cargo.toml + dependency literals
```

### Risk register

- **Commit 4 (trust/discovery)** lands the trust state machine. Hand-audit
  before merge; unit tests on every transition.
- **Commit 6 (interactive)** holds `&mut Buffer` in store state. Inline
  doc-comment on the invariant; consider `unsafe`-marking the raw-pointer
  field if simpler than maintaining the `*mut Buffer` discipline.
- **Commit 7 (provider)** is the biggest single commit (~2k LoC including
  SPP-in-WIT trampolines). Worth its own pre-merge review pass; consider
  splitting into 7a (capability imports + adapter skeleton) and 7b
  (provider registration into PROVIDERS) if the diff is unwieldy.
- **CI wasm-toolchain isolation.** Main `cargo test --workspace` does not
  depend on `cargo-component`. Example-plugins job is supplemental;
  its failure does not gate merge.

## Section 7 — Non-goals, deferred, open questions

### Non-goals for v0.18.0

- **WASM subagents.** `RegisterInProcessTool` stays in-process-only.
- **WASM tools (stdio MCP tool servers in wasm).** Separate sub-project.
- **Plugin → plugin direct calls.** Communication is through host
  dispatchers (`Effect::RunSlash`).
- **`http` wildcards in `allowed-hosts`.** Exact match only in v1;
  `*.example.com` syntax deferred.
- **Streaming-delta hooks** (`on_token`, `on_chunk`). v0.9.0 carve-out
  still applies.
- **Hot reload.** Editing manifest or wasm requires restart;
  `/plugins reload <id>` is post-v0.18.0.
- **Registry / index.** `/plugins install <toml-url>` only.
- **Auto-update.** No version-check, no background fetch.
- **Sandbox CPU/memory limits.** `epoch_interruption` caps wall time
  only; wasmtime `ResourceLimiter` is a future enhancement.
- **Signed plugins.** Trust is SHA-256 + user consent. Code-signing,
  Sigstore, provenance are post-v0.18.0.
- **GUI installer.** `/plugins install` is the only path.
- **Multi-file plugin distribution.** Single-file `plugin.toml` + single
  `wasm` URL in v1; `assets/` and directory-URL distribution post-v0.18.0.
- **`SAVVAGENT.md` exposure to plugins.** Host-private; plugins receive
  only `TurnCtx`/`ScreenOpenCtx`/`HookPayload`.

### Reserved-but-not-implemented

These appear in the WIT surface or manifest schema but never fire:

- `Effect::RegisterMcpServer` — manifest field reserved; loader rejects
  with "unsupported" if set.
- `HookKind::PreCompact` — same status as v0.9.0/B; still reserved.

### Versioning

- WIT package version: `savvagent:plugin@0.1.0` — bumped on any
  backward-incompatible WIT change. v0.18.0 ships `0.1.0`.
- Plugin manifest `savvagent = "^0.18"` — covers the v0.18.x line.
- wasmtime version pinned to `24.0` (minor). Security-advisory bumps
  reviewed manually.
- Workspace version bumps to `0.18.0` in commit 15.
- Per memory: ship A/B/C as `v0.17.0` immediately before this PR opens;
  this PR's tag is `v0.18.0`.

### Open questions

1. **`/plugins` keybinding.** Default is slash-only. Skipped unless usage
   shows demand.
2. **Plugin sort order in registry.** Currently built-ins → wasm
   (discovery order) → markdown. Stable across runs is enough; explicit
   sort is post-v0.18.0.
3. **`ProviderAdapter` concurrency.** `WasmProviderClient` is `Clone`able
   via `Arc`; multiple concurrent `complete()` calls each get their own
   `Store`. If wasmtime instantiation ever becomes a hotspot, we can pool
   pre-instances; defer.

---

## Appendix: file deltas summary

| Crate / file | Change |
|---|---|
| `crates/savvagent-plugin-wit/` | New: WIT files + bindgen output |
| `crates/savvagent-plugin-wasm/` | New: runtime + adapters + host imports |
| `crates/savvagent/src/plugin/registry.rs` | `register_all` now calls `register_external` |
| `crates/savvagent/src/plugin/builtin/plugins.rs` | New: `internal:plugins` built-in |
| `crates/savvagent/src/main.rs` | Wires plugin-config from env / CLI |
| `Cargo.toml` (workspace) | Adds two new crates; pins `wasmtime = "24.0"` |
| `README.md` | New "Authoring plugins" section |
| `CHANGELOG.md` | v0.18.0 entry |
| `docs/superpowers/specs/2026-05-25-external-plugins-design.md` | This file |

Expected total: ~8–12k LoC across all 15 commits, including
wit-bindgen-generated code (~3k LoC) and committed wasm fixtures
(~400 KB binary growth).
