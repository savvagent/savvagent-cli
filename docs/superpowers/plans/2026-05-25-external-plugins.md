# External Plugins (sub-project D) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add WASM external-plugin support to savvagent. Wasm modules implementing one of three WIT worlds (static / interactive / provider) are discovered from four well-known directories, hash-trusted, and adapted to `Box<dyn Plugin>` so the rest of the host treats them identically to built-ins.

**Architecture:** Two new crates — `savvagent-plugin-wit` (pure WIT + bindgen, leaf) and `savvagent-plugin-wasm` (wasmtime runtime + adapters + host-imports). A new `register_external()` call in `crates/savvagent/src/plugin/registry.rs` appends discovered wasm plugins. A new `internal:plugins` built-in owns `/plugins` (install/trust/revoke/remove/list/enable/disable).

**Tech Stack:** wasmtime 24.0 (Component Model), wit-bindgen, reqwest (TLS via rustls — already in workspace), keyring (for provider creds), sha2 (tree hashing), toml, walkdir.

**Spec:** `docs/superpowers/specs/2026-05-25-external-plugins-design.md`. Read it before starting.

**Reality reconciliations against the spec:**

The spec sketched a WIT surface that didn't exactly match the actual `Plugin` trait in `crates/savvagent-plugin/src/plugin.rs`. The plan reconciles as follows:

| Spec named | Actual trait uses | Plan's WIT export name |
|---|---|---|
| `handle-hook(kind, payload-json)` | `on_event(HostEvent)` | `on-event(event-json: string)` |
| `init() -> plugin-manifest` | `manifest() -> Manifest` | `manifest() -> plugin-manifest` |
| Screen with `ScreenOpenCtx` | `create_screen(id, ScreenArgs)` | `create-screen(id, screen-args)` |
| Slash with `TurnCtx` | `handle_slash(name, args)` no ctx | `handle-slash(name, args)` |

The `event-json` boundary matches the existing `HostEvent` enum's serde representation. We never change `HostEvent`'s on-wire shape during this work — additions are backward-compatible by serde-default rules.

---

## Pre-flight

Before Task 1, the implementer should:

- Read the spec end-to-end.
- Read `crates/savvagent-plugin/src/plugin.rs` (the trait we adapt to).
- Read `crates/savvagent-plugin/src/event.rs` (HostEvent + HookKind).
- Read `crates/savvagent/src/plugin/builtin/user_hooks/discovery.rs` (prior-art four-path discovery).
- Read `crates/savvagent/src/plugin/registry.rs` (insertion site for `register_external`).
- Confirm rust toolchain ≥ 1.85 (workspace `rust-version`).

---

## Task 1: `savvagent-plugin-wit` crate scaffold + shared.wit + spp.wit + CI dep-guard

**Files:**
- Create: `crates/savvagent-plugin-wit/Cargo.toml`
- Create: `crates/savvagent-plugin-wit/src/lib.rs`
- Create: `crates/savvagent-plugin-wit/wit/shared.wit`
- Create: `crates/savvagent-plugin-wit/wit/spp.wit`
- Create: `crates/savvagent-plugin-wit/build.rs`
- Create: `crates/savvagent-plugin-wit/tests/wit_parses.rs`
- Modify: `Cargo.toml` (workspace) — add the crate to `[workspace] members` and `[workspace.dependencies]`
- Create: `.github/workflows/wit-dep-guard.yml` (CI grep ensuring no ratatui/crossterm/tokio/anyhow in `savvagent-plugin-wit`)

- [ ] **Step 1.1: Add the crate to the workspace.**

Edit `Cargo.toml` to add `"crates/savvagent-plugin-wit"` to `members` (alphabetically near `savvagent-plugin`) and add this to `[workspace.dependencies]`:

```toml
savvagent-plugin-wit = { path = "crates/savvagent-plugin-wit", version = "0.17.0" }
wit-bindgen = "0.34"
```

The version literal here is `0.17.0` because the workspace is still at `0.17.0`; Task 15 bumps to `0.18.0`. Keeping them in sync per `feedback_semver.md`.

- [ ] **Step 1.2: Write `crates/savvagent-plugin-wit/Cargo.toml`.**

```toml
[package]
name = "savvagent-plugin-wit"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true
description = "WIT contract for savvagent external plugins (sub-project D)."

[dependencies]
wit-bindgen = { workspace = true }

[build-dependencies]
# none — wit-bindgen runs at compile time via its proc-macro entry,
# build.rs only re-runs cargo on wit/* changes.

[lints.rust]
# Dep-guard enforcement at the source level (in addition to CI grep).
# This crate must never depend on ratatui/crossterm/tokio/anyhow.
unsafe_code = "forbid"
```

- [ ] **Step 1.3: Write `crates/savvagent-plugin-wit/build.rs`.**

```rust
fn main() {
    // Re-run if any WIT file changes.
    println!("cargo::rerun-if-changed=wit");
}
```

- [ ] **Step 1.4: Write `crates/savvagent-plugin-wit/wit/shared.wit`.**

```wit
package savvagent:plugin@0.1.0;

interface types {
    // ---- Key events ---------------------------------------------------
    record key-event-portable {
        code: key-code,
        modifiers: key-modifiers,
    }

    variant key-code {
        char(string),         // single-char text input; UTF-8 grapheme is fine
        enter,
        escape,
        backspace,
        tab,
        backtab,
        delete,
        insert,
        home,
        %end,
        page-up,
        page-down,
        up,
        down,
        left,
        right,
        function(u8),         // F1..F12 -> 1..12
        null,
    }

    record key-modifiers {
        ctrl: bool,
        shift: bool,
        alt: bool,
        meta: bool,
    }

    // ---- Geometry & color --------------------------------------------
    record region {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    }

    variant theme-color {
        reset,
        black,
        red,
        green,
        yellow,
        blue,
        magenta,
        cyan,
        gray,
        dark-gray,
        light-red,
        light-green,
        light-yellow,
        light-blue,
        light-magenta,
        light-cyan,
        white,
        indexed(u8),
        rgb(rgb-color),
    }

    record rgb-color { r: u8, g: u8, b: u8 }

    record text-mods {
        bold: bool,
        italic: bool,
        underline: bool,
        reverse: bool,
        dim: bool,
    }

    // ---- Effects -----------------------------------------------------
    // Mirror of savvagent_plugin::Effect (closed set; v0.9.0 §9 rule).
    variant effect {
        push-note(note),
        open-screen(screen-target),
        set-theme(string),
        run-slash(slash-call),
        save-transcript,
        clear-log,
        register-keybinding(keybinding),
    }

    record note {
        text: string,
        level: note-level,
    }

    variant note-level { info, warning, error }

    record screen-target {
        plugin-id: string,
        screen-id: string,
        args-json: string,           // serialized ScreenArgs
    }

    record slash-call {
        name: string,
        args: list<string>,
    }

    record keybinding {
        key: key-event-portable,
        action: keybinding-action,
    }

    variant keybinding-action {
        slash(slash-call),
        emit-effect(effect-name),
    }

    variant effect-name {
        save-transcript-action,
        clear-log-action,
    }

    // ---- Manifest ----------------------------------------------------
    record plugin-manifest {
        id: string,
        name: string,
        version: string,
        description: string,
        kind: plugin-kind,
        contributions: contributions,
    }

    variant plugin-kind { core, optional }

    record contributions {
        slash-commands: list<string>,
        hooks: list<hook-kind>,
        screens: list<string>,
        render-slots: list<string>,
        keybindings: list<keybinding>,
        themes: bool,
    }

    // ---- Hook discriminants (mirror savvagent_plugin::HookKind) -----
    variant hook-kind {
        host-starting,
        connect,
        disconnect,
        turn-start,
        turn-end,
        tool-call-start,
        tool-call-end,
        prompt-submitted,
        transcript-saved,
        provider-registered,
        context-size-changed,
        active-provider-changed,
        subagent-stop,
    }

    // ---- Errors ------------------------------------------------------
    variant plugin-error {
        invalid-input(string),
        io(string),
        capability-denied(string),
        unsupported(string),
        screen-not-found(string),
    }

    // ---- Render output ----------------------------------------------
    record styled-span {
        text: string,
        fg: theme-color,
        bg: theme-color,
        mods: text-mods,
    }

    record styled-line {
        spans: list<styled-span>,
    }

    // ---- Logging level -----------------------------------------------
    variant log-level { trace, debug, info, warn, error }

    // ---- Theme catalog entry ----------------------------------------
    record theme-entry {
        slug: string,
        name: string,
        colors: list<tuple<string, theme-color>>,
    }
}
```

- [ ] **Step 1.5: Write `crates/savvagent-plugin-wit/wit/spp.wit`.**

```wit
package savvagent:spp@0.1.0;

interface types {
    // SPP types mirrored from savvagent-protocol field-for-field.
    // From/Into impls live in savvagent-plugin-wasm/src/spp_convert.rs.

    record complete-request {
        model: string,
        system: option<string>,
        messages: list<message>,
        tools: list<tool-def>,
        max-tokens: option<u32>,
        streaming: option<streaming-config>,
        thinking: option<thinking-config>,
    }

    record message {
        role: role,
        content: list<content-block>,
    }

    variant role { user, assistant, system }

    variant content-block {
        text(string),
        tool-use(tool-use-block),
        tool-result(tool-result-block),
        image(image-block),
        thinking(thinking-block),
    }

    record tool-use-block {
        id: string,
        name: string,
        input-json: string,
    }

    record tool-result-block {
        tool-use-id: string,
        content: string,
        is-error: bool,
    }

    record image-block {
        media-type: string,
        data-base64: string,
    }

    record thinking-block {
        text: string,
        signature: option<string>,
    }

    record tool-def {
        name: string,
        description: string,
        input-schema-json: string,
    }

    record streaming-config { enabled: bool }
    record thinking-config { budget-tokens: u32 }

    record complete-response {
        id: string,
        model: string,
        content: list<content-block>,
        stop-reason: stop-reason,
        usage: usage,
    }

    variant stop-reason {
        end-turn,
        max-tokens,
        stop-sequence,
        tool-use,
        unknown(string),
    }

    record usage {
        input-tokens: u32,
        output-tokens: u32,
    }

    // ---- Streaming events ------------------------------------------
    variant stream-event {
        message-start(message-meta),
        content-block-start(content-block-start-evt),
        content-block-delta(content-block-delta-evt),
        content-block-stop(u32),
        message-delta(message-delta-evt),
        message-stop(stop-info),
        ping,
    }

    record message-meta {
        id: string,
        model: string,
    }

    record content-block-start-evt {
        index: u32,
        block: content-block,
    }

    record content-block-delta-evt {
        index: u32,
        delta: content-block-delta,
    }

    variant content-block-delta {
        text-delta(string),
        input-json-delta(string),
        thinking-delta(string),
        signature-delta(string),
    }

    record message-delta-evt {
        stop-reason: option<stop-reason>,
        usage: option<usage>,
    }

    record stop-info {
        usage: usage,
    }

    // ---- Models & tokens -------------------------------------------
    record model-info {
        id: string,
        display-name: string,
        context-window: u32,
    }

    record count-tokens-request {
        model: string,
        messages: list<message>,
    }

    record count-tokens-response {
        input-tokens: u32,
    }

    record provider-manifest {
        provider-id: string,
        models: list<model-info>,
        default-model: option<string>,
    }

    // ---- Provider error variants -----------------------------------
    variant provider-error {
        auth-failed(string),
        rate-limited(rate-limit-info),
        bad-request(string),
        upstream(upstream-error),
        transport(string),
        cancelled,
    }

    record rate-limit-info {
        retry-after-secs: option<u32>,
        reason: string,
    }

    record upstream-error {
        status: u16,
        body: string,
    }
}
```

- [ ] **Step 1.6: Write `crates/savvagent-plugin-wit/src/lib.rs`.**

```rust
//! WIT contract crate for savvagent external plugins.
//!
//! This crate holds the `.wit` files and (via `wit-bindgen`) the generated
//! host-side bindings used by `savvagent-plugin-wasm`. It must remain a
//! leaf crate with zero runtime dependencies — see `wit-dep-guard.yml` for
//! the CI enforcement.

#![forbid(unsafe_code)]
#![doc = include_str!("../wit/shared.wit")]

// wit-bindgen generates host-side bindings for one component-model world
// per call. We generate three: one per world introduced in v0.18.0.
//
// The generation is gated by an explicit module so the produced types
// are namespaced and the public surface is curated.

pub mod static_world {
    wit_bindgen::generate!({
        path: "../savvagent-plugin-wit/wit",
        world: "plugin-static",
        with: {},
    });
}

pub mod interactive_world {
    wit_bindgen::generate!({
        path: "../savvagent-plugin-wit/wit",
        world: "plugin-interactive",
        with: {},
    });
}

pub mod provider_world {
    wit_bindgen::generate!({
        path: "../savvagent-plugin-wit/wit",
        world: "plugin-provider",
        with: {},
    });
}
```

> **Note:** The actual `wit_bindgen::generate!` syntax for `0.34` may require slight adjustment — verify against `https://docs.rs/wit-bindgen/0.34/` when this lands. If the macro can't find the `wit/` dir from `src/lib.rs`, switch to an `include_dir!`/`build.rs` codegen path. Don't proceed if generate! refuses to expand — see the verification step.

- [ ] **Step 1.7: Add stub WIT world files** (will be filled in Task 2).

Create `crates/savvagent-plugin-wit/wit/plugin-static.wit`, `plugin-interactive.wit`, `plugin-provider.wit` with skeleton content so `cargo build` at the end of Task 1 succeeds:

```wit
// plugin-static.wit
package savvagent:plugin@0.1.0;

world plugin-static {
    use types.{plugin-manifest, plugin-error};
    export manifest: func() -> result<plugin-manifest, plugin-error>;
}
```

```wit
// plugin-interactive.wit
package savvagent:plugin@0.1.0;

world plugin-interactive {
    use types.{plugin-manifest, plugin-error};
    export manifest: func() -> result<plugin-manifest, plugin-error>;
}
```

```wit
// plugin-provider.wit
package savvagent:plugin@0.1.0;

world plugin-provider {
    use types.{plugin-manifest, plugin-error};
    export manifest: func() -> result<plugin-manifest, plugin-error>;
}
```

- [ ] **Step 1.8: Write the parse-only sanity test `crates/savvagent-plugin-wit/tests/wit_parses.rs`.**

```rust
//! Sanity test: every WIT file in the crate parses with `wit-parser`.

use std::fs;

#[test]
fn every_wit_file_parses() {
    let mut resolver = wit_parser::Resolve::default();
    let pkg = resolver
        .push_dir(std::path::Path::new("wit"))
        .expect("wit/ must contain parseable .wit files");
    assert!(pkg.0.iter().any(|_| true), "at least one package");
}
```

Add to `Cargo.toml`:

```toml
[dev-dependencies]
wit-parser = "0.220"
```

- [ ] **Step 1.9: Run sanity build.**

```bash
cargo build -p savvagent-plugin-wit
cargo test -p savvagent-plugin-wit
```

Expected: clean build, `every_wit_file_parses` passes. If `wit_bindgen::generate!` fails, fall back to the build.rs codegen path — the world files are skeletal at this point so the macro should expand to nearly nothing.

- [ ] **Step 1.10: Write the dep-guard CI workflow `.github/workflows/wit-dep-guard.yml`.**

```yaml
name: WIT dep-guard

on:
  push: { paths: [ "crates/savvagent-plugin-wit/**", ".github/workflows/wit-dep-guard.yml" ] }
  pull_request: { paths: [ "crates/savvagent-plugin-wit/**" ] }

jobs:
  guard:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Forbidden deps grep
        run: |
          set -eu
          deps=$(awk '/^\[dependencies\]/{f=1;next} /^\[/{f=0} f' \
                 crates/savvagent-plugin-wit/Cargo.toml)
          for forbidden in ratatui crossterm tokio anyhow reqwest wasmtime; do
            if echo "$deps" | grep -q "^$forbidden\b"; then
              echo "::error::savvagent-plugin-wit must not depend on '$forbidden'"
              exit 1
            fi
          done
          echo "ok"
```

- [ ] **Step 1.11: Commit.**

```bash
git add Cargo.toml crates/savvagent-plugin-wit/ .github/workflows/wit-dep-guard.yml
git commit -m "feat(plugin-wit): crate scaffold + shared.wit + spp.wit + dep-guard CI"
```

---

## Task 2: WIT world definitions + bindings + SPP↔WIT round-trips

**Files:**
- Modify: `crates/savvagent-plugin-wit/wit/plugin-static.wit`
- Modify: `crates/savvagent-plugin-wit/wit/plugin-interactive.wit`
- Modify: `crates/savvagent-plugin-wit/wit/plugin-provider.wit`
- Create: `crates/savvagent-plugin-wit/tests/world_validates.rs`
- Create: `crates/savvagent-plugin-wasm/Cargo.toml` (scaffold only — empty lib)
- Create: `crates/savvagent-plugin-wasm/src/lib.rs`
- Create: `crates/savvagent-plugin-wasm/src/spp_convert.rs` (the From/Into impls + tests)
- Modify: `Cargo.toml` (workspace) — add savvagent-plugin-wasm + wasmtime dependency stanzas

- [ ] **Step 2.1: Fill `plugin-static.wit`.**

```wit
package savvagent:plugin@0.1.0;

world plugin-static {
    use types.{plugin-manifest, plugin-error, effect, hook-kind, theme-color,
               region, styled-line, theme-entry, log-level};

    // ---- Host-imported capabilities -----------------------------------
    import log: func(level: log-level, msg: string);
    import current-theme: func() -> list<tuple<string, theme-color>>;

    // ---- Plugin exports ----------------------------------------------
    export manifest: func() -> result<plugin-manifest, plugin-error>;
    export handle-slash: func(name: string, args: list<string>)
        -> result<list<effect>, plugin-error>;
    export on-event: func(event-json: string)
        -> result<list<effect>, plugin-error>;
    export render-slot: func(slot-id: string, area: region) -> list<styled-line>;
    export themes: func() -> list<theme-entry>;
}
```

- [ ] **Step 2.2: Fill `plugin-interactive.wit`.**

```wit
package savvagent:plugin@0.1.0;

world plugin-interactive {
    use types.{plugin-manifest, plugin-error, effect, key-event-portable,
               region, theme-color, text-mods, log-level};

    // ---- Imports -----------------------------------------------------
    import log: func(level: log-level, msg: string);
    import current-theme: func() -> list<tuple<string, theme-color>>;

    // Draw primitives: host accumulates into a ratatui Buffer set in
    // store state before `render` is called.
    import draw-text: func(x: u16, y: u16, text: string,
                           fg: theme-color, bg: theme-color, mods: text-mods);
    import draw-block: func(area: region, border-style: border-style,
                            title: option<string>);
    import draw-line: func(x1: u16, y1: u16, x2: u16, y2: u16, style: line-style);
    import clear-area: func(area: region, bg: theme-color);

    variant border-style { none, plain, rounded, double-line, thick }
    variant line-style { solid, dashed, dotted }

    record screen-args {
        invocation-json: string,   // serialized savvagent_plugin::ScreenArgs
        terminal-width: u16,
        terminal-height: u16,
    }

    // Component-model resource: host holds opaque handle.
    resource screen-instance {
        on-key: func(key: key-event-portable) -> list<effect>;
        render: func(area: region);                    // side effects via draw imports
        tips: func() -> string;
    }

    // ---- Exports -----------------------------------------------------
    export manifest: func() -> result<plugin-manifest, plugin-error>;
    export create-screen: func(screen-id: string, args: screen-args)
        -> result<screen-instance, plugin-error>;
}
```

- [ ] **Step 2.3: Fill `plugin-provider.wit`.**

```wit
package savvagent:plugin@0.1.0;

world plugin-provider {
    use types.{plugin-manifest, plugin-error, log-level};
    use savvagent:spp/types.{complete-request, complete-response, stream-event,
                              model-info, count-tokens-request, count-tokens-response,
                              provider-error, provider-manifest};

    // ---- Imports -----------------------------------------------------
    import log: func(level: log-level, msg: string);
    import http: http-capability;
    import keyring: keyring-capability;
    import progress: progress-capability;

    // ---- Exports -----------------------------------------------------
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
        timeout-ms: option<u32>,
    }

    record http-response {
        status: u16,
        headers: list<tuple<string, string>>,
        body: list<u8>,
    }

    variant http-error {
        transport(string),
        tls(string),
        denied-host(string),
        denied-method(string),
        oversize,
        timeout,
        body-too-large(u64),
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
    variant keyring-error {
        not-found,
        denied(string),
        backend(string),
    }

    // service is always "savvagent" — fixed; only account is a parameter.
    get: func(account: string) -> result<string, keyring-error>;
}

interface progress-capability {
    use savvagent:spp/types.{stream-event};
    emit-stream-event: func(event: stream-event);
}
```

- [ ] **Step 2.4: Write `crates/savvagent-plugin-wit/tests/world_validates.rs`.**

```rust
//! Every world file resolves to a fully-typed component-model world.

use std::path::Path;

#[test]
fn three_worlds_resolve() {
    let mut r = wit_parser::Resolve::default();
    r.push_dir(Path::new("wit")).expect("parse");
    let worlds: Vec<String> = r
        .worlds
        .iter()
        .map(|(_, w)| w.name.clone())
        .collect();
    assert!(worlds.contains(&"plugin-static".into()),
            "missing plugin-static; got {worlds:?}");
    assert!(worlds.contains(&"plugin-interactive".into()),
            "missing plugin-interactive; got {worlds:?}");
    assert!(worlds.contains(&"plugin-provider".into()),
            "missing plugin-provider; got {worlds:?}");
}
```

- [ ] **Step 2.5: Add `savvagent-plugin-wasm` to the workspace and create the scaffold crate.**

Edit `Cargo.toml`:

```toml
# in [workspace] members:
"crates/savvagent-plugin-wasm",

# in [workspace.dependencies]:
savvagent-plugin-wasm = { path = "crates/savvagent-plugin-wasm", version = "0.17.0" }
wasmtime = "24.0"
wasmtime-wasi = "24.0"
sha2 = "0.10"
walkdir = "2.5"
```

Create `crates/savvagent-plugin-wasm/Cargo.toml`:

```toml
[package]
name = "savvagent-plugin-wasm"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true
description = "Wasmtime-backed runtime for savvagent external plugins."

[dependencies]
savvagent-plugin = { workspace = true }
savvagent-plugin-wit = { workspace = true }
savvagent-protocol = { workspace = true }
savvagent-mcp = { workspace = true }
async-trait = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
toml = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true }
wasmtime = { workspace = true }
wasmtime-wasi = { workspace = true }
sha2 = { workspace = true }
walkdir = { workspace = true }
reqwest = { workspace = true }
keyring = "2"
tracing = "0.1"
url = "2"

[dev-dependencies]
tempfile = "3"
proptest = "1"
```

- [ ] **Step 2.6: Write the empty lib `crates/savvagent-plugin-wasm/src/lib.rs`.**

```rust
//! Wasmtime-backed runtime for savvagent external plugins.
//!
//! This crate adapts WASM components implementing one of three WIT worlds
//! (plugin-static / plugin-interactive / plugin-provider) to
//! `Box<dyn savvagent_plugin::Plugin>` — making them indistinguishable from
//! built-ins to the rest of the host.

#![deny(missing_docs)]

pub mod spp_convert;

/// Re-export of the WIT bindings for downstream convenience.
pub use savvagent_plugin_wit as wit;
```

- [ ] **Step 2.7: Write `crates/savvagent-plugin-wasm/src/spp_convert.rs` — the From/Into impls.**

This file is mechanical and long. The contract: every type in `savvagent_protocol` has both directions; every variant has one test; high-fanout types (`CompleteRequest`, `StreamEvent`) get proptests.

```rust
//! Mechanical From/Into between `savvagent_protocol` types and the WIT
//! mirror in `savvagent_plugin_wit::provider_world::savvagent::spp::types`.
//!
//! Convention: the WIT alias is `wit::*`. The Rust types are imported
//! from `savvagent_protocol::*` with their canonical names.

use savvagent_plugin_wit::provider_world::savvagent::spp::types as wit;
use savvagent_protocol as spp;

// ---- CompleteRequest -------------------------------------------------
impl From<spp::CompleteRequest> for wit::CompleteRequest {
    fn from(r: spp::CompleteRequest) -> Self {
        Self {
            model: r.model,
            system: r.system,
            messages: r.messages.into_iter().map(Into::into).collect(),
            tools: r.tools.into_iter().map(Into::into).collect(),
            max_tokens: r.max_tokens,
            streaming: r.streaming.map(Into::into),
            thinking: r.thinking.map(Into::into),
        }
    }
}

impl From<wit::CompleteRequest> for spp::CompleteRequest {
    fn from(r: wit::CompleteRequest) -> Self {
        Self {
            model: r.model,
            system: r.system,
            messages: r.messages.into_iter().map(Into::into).collect(),
            tools: r.tools.into_iter().map(Into::into).collect(),
            max_tokens: r.max_tokens,
            streaming: r.streaming.map(Into::into),
            thinking: r.thinking.map(Into::into),
        }
    }
}

// ---- Message + content blocks ---------------------------------------
impl From<spp::Message> for wit::Message {
    fn from(m: spp::Message) -> Self {
        Self {
            role: m.role.into(),
            content: m.content.into_iter().map(Into::into).collect(),
        }
    }
}
impl From<wit::Message> for spp::Message {
    fn from(m: wit::Message) -> Self {
        Self {
            role: m.role.into(),
            content: m.content.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<spp::Role> for wit::Role {
    fn from(r: spp::Role) -> Self {
        match r {
            spp::Role::User => Self::User,
            spp::Role::Assistant => Self::Assistant,
            spp::Role::System => Self::System,
        }
    }
}
impl From<wit::Role> for spp::Role {
    fn from(r: wit::Role) -> Self {
        match r {
            wit::Role::User => Self::User,
            wit::Role::Assistant => Self::Assistant,
            wit::Role::System => Self::System,
        }
    }
}

impl From<spp::ContentBlock> for wit::ContentBlock {
    fn from(b: spp::ContentBlock) -> Self {
        match b {
            spp::ContentBlock::Text(s) => Self::Text(s),
            spp::ContentBlock::ToolUse(t) => Self::ToolUse(t.into()),
            spp::ContentBlock::ToolResult(t) => Self::ToolResult(t.into()),
            spp::ContentBlock::Image(i) => Self::Image(i.into()),
            spp::ContentBlock::Thinking(t) => Self::Thinking(t.into()),
        }
    }
}
impl From<wit::ContentBlock> for spp::ContentBlock {
    fn from(b: wit::ContentBlock) -> Self {
        match b {
            wit::ContentBlock::Text(s) => Self::Text(s),
            wit::ContentBlock::ToolUse(t) => Self::ToolUse(t.into()),
            wit::ContentBlock::ToolResult(t) => Self::ToolResult(t.into()),
            wit::ContentBlock::Image(i) => Self::Image(i.into()),
            wit::ContentBlock::Thinking(t) => Self::Thinking(t.into()),
        }
    }
}

// ---- Tool blocks ----------------------------------------------------
impl From<spp::ToolUseBlock> for wit::ToolUseBlock {
    fn from(t: spp::ToolUseBlock) -> Self {
        Self {
            id: t.id,
            name: t.name,
            input_json: serde_json::to_string(&t.input)
                .expect("ToolUseBlock.input is always serializable"),
        }
    }
}
impl From<wit::ToolUseBlock> for spp::ToolUseBlock {
    fn from(t: wit::ToolUseBlock) -> Self {
        Self {
            id: t.id,
            name: t.name,
            input: serde_json::from_str(&t.input_json)
                .unwrap_or(serde_json::Value::Null),
        }
    }
}

// ... (similar mechanical impls for ToolResultBlock, ImageBlock,
//      ThinkingBlock, ToolDef, StreamingConfig, ThinkingConfig,
//      CompleteResponse, StopReason, Usage, StreamEvent + variants,
//      ModelInfo, CountTokensRequest, CountTokensResponse, ProviderError,
//      RateLimitInfo, UpstreamError, ProviderManifest)

#[cfg(test)]
mod roundtrip_tests {
    use super::*;

    // One round-trip test per variant. Pattern:
    fn assert_complete_request_roundtrips(r: spp::CompleteRequest) {
        let w: wit::CompleteRequest = r.clone().into();
        let back: spp::CompleteRequest = w.into();
        assert_eq!(r, back);
    }

    #[test]
    fn empty_complete_request() {
        assert_complete_request_roundtrips(spp::CompleteRequest {
            model: "claude-sonnet-4-6".into(),
            system: None,
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            streaming: None,
            thinking: None,
        });
    }

    #[test]
    fn text_content_block_roundtrip() {
        let b = spp::ContentBlock::Text("hello".into());
        let w: wit::ContentBlock = b.clone().into();
        let back: spp::ContentBlock = w.into();
        assert_eq!(b, back);
    }

    #[test]
    fn tool_use_block_roundtrip() {
        let b = spp::ContentBlock::ToolUse(spp::ToolUseBlock {
            id: "abc".into(),
            name: "tool-fs:read_file".into(),
            input: serde_json::json!({"path": "/etc/hosts"}),
        });
        let w: wit::ContentBlock = b.clone().into();
        let back: spp::ContentBlock = w.into();
        assert_eq!(b, back);
    }

    // One test per StreamEvent variant.
    #[test]
    fn stream_event_ping_roundtrip() {
        let e = spp::StreamEvent::Ping;
        let w: wit::StreamEvent = e.clone().into();
        let back: spp::StreamEvent = w.into();
        assert_eq!(e, back);
    }

    // ... one test per remaining variant (MessageStart, ContentBlockStart,
    //     ContentBlockDelta with each delta sub-variant, ContentBlockStop,
    //     MessageDelta, MessageStop).

    // Property tests for the high-fanout types.
    proptest::proptest! {
        #[test]
        fn complete_request_roundtrip_property(r in arb_complete_request()) {
            let w: wit::CompleteRequest = r.clone().into();
            let back: spp::CompleteRequest = w.into();
            proptest::prop_assert_eq!(r, back);
        }
    }

    fn arb_complete_request() -> impl proptest::strategy::Strategy<Value = spp::CompleteRequest> {
        use proptest::prelude::*;
        (
            any::<String>(),
            proptest::option::of(any::<String>()),
            prop::collection::vec(arb_message(), 0..4),
        )
            .prop_map(|(model, system, messages)| spp::CompleteRequest {
                model, system, messages, tools: vec![],
                max_tokens: None, streaming: None, thinking: None,
            })
    }

    fn arb_message() -> impl proptest::strategy::Strategy<Value = spp::Message> {
        use proptest::prelude::*;
        (
            prop_oneof![Just(spp::Role::User), Just(spp::Role::Assistant)],
            prop::collection::vec(Just(spp::ContentBlock::Text("x".into())), 0..3),
        )
            .prop_map(|(role, content)| spp::Message { role, content })
    }
}
```

The complete implementation is mechanical; the executor fills in the remaining variants following the exact same `From<spp::X> for wit::X` / `From<wit::X> for spp::X` pattern. Reject any deviation that doesn't preserve byte-equality on round-trip.

- [ ] **Step 2.8: Run tests.**

```bash
cargo build -p savvagent-plugin-wit -p savvagent-plugin-wasm
cargo test -p savvagent-plugin-wit -p savvagent-plugin-wasm
```

Expected: all tests pass. SPP fixtures round-trip cleanly.

- [ ] **Step 2.9: Commit.**

```bash
git add Cargo.toml crates/savvagent-plugin-wit/ crates/savvagent-plugin-wasm/
git commit -m "feat(plugin-wit,plugin-wasm): three WIT worlds + SPP<->WIT round-trips"
```

---

## Task 3: Manifest, discovery, trust file, tree-hash

**Files:**
- Create: `crates/savvagent-plugin-wasm/src/manifest.rs`
- Create: `crates/savvagent-plugin-wasm/src/discovery.rs`
- Create: `crates/savvagent-plugin-wasm/src/trust.rs`
- Create: `crates/savvagent-plugin-wasm/src/error.rs`
- Modify: `crates/savvagent-plugin-wasm/src/lib.rs` (export modules)
- Create: `crates/savvagent-plugin-wasm/tests/discovery.rs`
- Create: `crates/savvagent-plugin-wasm/tests/trust.rs`

Reference the existing four-path pattern in `crates/savvagent/src/plugin/builtin/user_hooks/discovery.rs`. Mirror its precedence order and project-root walk.

- [ ] **Step 3.1: Define the error type — `crates/savvagent-plugin-wasm/src/error.rs`.**

```rust
//! Error types for the wasm runtime — covers discovery, manifest parsing,
//! trust validation, and runtime adapter errors.

use std::path::PathBuf;
use thiserror::Error;

/// All failures the wasm-plugin runtime can produce. Stored as enum
/// variants per error class so callers can pattern-match.
#[derive(Debug, Error)]
pub enum WasmPluginError {
    #[error("manifest at {0:?}: {1}")]
    Manifest(PathBuf, String),

    #[error("io error at {0:?}: {1}")]
    Io(PathBuf, std::io::Error),

    #[error("plugin id '{0}' is invalid: {1}")]
    InvalidId(String, String),

    #[error("plugin world '{0}' is not one of plugin-static|plugin-interactive|plugin-provider")]
    InvalidWorld(String),

    #[error("plugin {0} declares exports {declared:?} but wasm exports {actual:?}", declared = .1, actual = .2)]
    ExportMismatch(String, Vec<String>, Vec<String>),

    #[error("plugin {0} requires savvagent {1} but this build provides {2}")]
    VersionMismatch(String, String, String),

    #[error("plugin {0} hash mismatch: stored={1} actual={2}")]
    HashMismatch(String, String, String),

    #[error("plugin {0} is not trusted; run /plugins trust {0}")]
    Untrusted(String),

    #[error("plugin {0} is disabled: {1}")]
    Disabled(String, String),

    #[error("wasmtime: {0}")]
    Wasmtime(#[from] anyhow::Error),

    #[error("capability denied: {0}")]
    CapabilityDenied(String),
}
```

- [ ] **Step 3.2: Manifest type + TOML parsing — `crates/savvagent-plugin-wasm/src/manifest.rs`.**

```rust
//! Parser/validator for `plugin.toml` files.
//!
//! Validation steps (in order):
//! 1. TOML parses.
//! 2. Required fields present.
//! 3. id format is `<lowercase-kebab>.<lowercase-kebab>`.
//! 4. `world` is one of the three known values.
//! 5. `savvagent` version range is satisfiable by current build.
//! 6. `[security]` is rejected on non-provider worlds.

use std::path::Path;
use serde::Deserialize;

use crate::error::WasmPluginError;

const CURRENT_WIT_VERSION: &str = "0.18";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PluginManifest {
    pub plugin: PluginSection,
    #[serde(default)]
    pub exports: ExportsSection,
    #[serde(default)]
    pub security: Option<SecuritySection>,
    #[serde(default)]
    pub runtime: RuntimeSection,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PluginSection {
    pub id: String,
    pub name: String,
    pub version: String,
    pub world: PluginWorld,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    pub savvagent: String,
    #[serde(default)]
    pub wasm: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginWorld {
    PluginStatic,
    PluginInteractive,
    PluginProvider,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct ExportsSection {
    #[serde(default)]
    pub slash_commands: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub screens: Vec<String>,
    #[serde(default)]
    pub render_slots: Vec<String>,
    #[serde(default)]
    pub keybindings: Vec<String>,
    #[serde(default)]
    pub themes: bool,
    #[serde(default)]
    pub provider_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SecuritySection {
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    pub keyring_accounts: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RuntimeSection {
    #[serde(default = "default_call_timeout")]
    pub call_timeout_ms: u32,
}

impl Default for RuntimeSection {
    fn default() -> Self { Self { call_timeout_ms: 5_000 } }
}

fn default_call_timeout() -> u32 { 5_000 }

impl PluginManifest {
    /// Parse + validate a plugin.toml at `path`. The directory name is
    /// passed separately so we can validate the id matches.
    pub fn load(path: &Path, expected_id: &str) -> Result<Self, WasmPluginError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| WasmPluginError::Io(path.to_path_buf(), e))?;
        let m: PluginManifest = toml::from_str(&text)
            .map_err(|e| WasmPluginError::Manifest(path.to_path_buf(), e.to_string()))?;

        validate_id(&m.plugin.id)
            .map_err(|reason| WasmPluginError::InvalidId(m.plugin.id.clone(), reason))?;

        if m.plugin.id != expected_id {
            return Err(WasmPluginError::Manifest(
                path.to_path_buf(),
                format!("id '{}' does not match directory '{expected_id}'", m.plugin.id),
            ));
        }

        validate_version_range(&m.plugin.savvagent)
            .map_err(|reason| WasmPluginError::VersionMismatch(
                m.plugin.id.clone(),
                m.plugin.savvagent.clone(),
                reason,
            ))?;

        // [security] is provider-world only.
        if m.security.is_some() && !matches!(m.plugin.world, PluginWorld::PluginProvider) {
            return Err(WasmPluginError::Manifest(
                path.to_path_buf(),
                "[security] is only valid for plugin-provider world".into(),
            ));
        }

        // Provider plugins must declare provider_id.
        if matches!(m.plugin.world, PluginWorld::PluginProvider)
            && m.exports.provider_id.is_none()
        {
            return Err(WasmPluginError::Manifest(
                path.to_path_buf(),
                "plugin-provider must set [exports] provider-id".into(),
            ));
        }

        // Cap call timeout at 300s.
        let runtime = RuntimeSection {
            call_timeout_ms: m.runtime.call_timeout_ms.min(300_000),
        };

        Ok(PluginManifest { runtime, ..m })
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    let parts: Vec<&str> = id.split('.').collect();
    if parts.len() != 2 {
        return Err("must be <org>.<name> (exactly one dot)".into());
    }
    for part in &parts {
        if part.is_empty() {
            return Err("segments must be non-empty".into());
        }
        if !part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(format!("segment '{part}' must be lowercase kebab-case"));
        }
        if part.starts_with('-') || part.ends_with('-') {
            return Err(format!("segment '{part}' must not start/end with '-'"));
        }
    }
    Ok(())
}

fn validate_version_range(range: &str) -> Result<(), String> {
    // Accept caret ranges only in v1: "^0.18", "^0.18.0", "^0.18.1".
    let stripped = range.strip_prefix('^').ok_or("must be caret-prefixed (e.g. ^0.18)")?;
    if !stripped.starts_with(CURRENT_WIT_VERSION) {
        return Err(format!("requires {stripped} but build provides {CURRENT_WIT_VERSION}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_manifest(s: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{s}").unwrap();
        f
    }

    #[test]
    fn valid_static_manifest_parses() {
        let f = write_manifest(r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"

[exports]
slash-commands = ["demo"]
themes = false
"#);
        let m = PluginManifest::load(f.path(), "acme.demo").unwrap();
        assert_eq!(m.plugin.world, PluginWorld::PluginStatic);
        assert_eq!(m.exports.slash_commands, vec!["demo".to_string()]);
    }

    #[test]
    fn id_mismatch_rejected() {
        let f = write_manifest(r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#);
        let e = PluginManifest::load(f.path(), "acme.other").unwrap_err();
        assert!(matches!(e, WasmPluginError::Manifest(_, _)));
    }

    #[test]
    fn security_on_static_rejected() {
        let f = write_manifest(r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"

[security]
allowed-hosts = ["example.com"]
"#);
        let e = PluginManifest::load(f.path(), "acme.demo").unwrap_err();
        assert!(matches!(e, WasmPluginError::Manifest(_, _)));
    }

    #[test]
    fn provider_without_provider_id_rejected() {
        let f = write_manifest(r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-provider"
savvagent = "^0.18"
"#);
        let e = PluginManifest::load(f.path(), "acme.demo").unwrap_err();
        assert!(matches!(e, WasmPluginError::Manifest(_, _)));
    }

    #[test]
    fn version_range_mismatch_rejected() {
        let f = write_manifest(r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.17"
"#);
        let e = PluginManifest::load(f.path(), "acme.demo").unwrap_err();
        assert!(matches!(e, WasmPluginError::VersionMismatch(..)));
    }

    #[test]
    fn id_format_rejected() {
        for bad in ["acme", "Acme.demo", "acme.", ".demo", "a.b.c", "acme.de_mo"] {
            let toml = format!(
                r#"
[plugin]
id = "{bad}"
name = "x"
version = "0"
world = "plugin-static"
savvagent = "^0.18"
"#);
            let f = write_manifest(&toml);
            let e = PluginManifest::load(f.path(), bad).unwrap_err();
            assert!(
                matches!(e, WasmPluginError::InvalidId(..) | WasmPluginError::Manifest(..)),
                "expected invalid-id error for '{bad}', got {e:?}"
            );
        }
    }

    #[test]
    fn call_timeout_capped() {
        let f = write_manifest(r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"

[runtime]
call-timeout-ms = 9999999
"#);
        let m = PluginManifest::load(f.path(), "acme.demo").unwrap();
        assert_eq!(m.runtime.call_timeout_ms, 300_000);
    }
}
```

- [ ] **Step 3.3: Discovery — `crates/savvagent-plugin-wasm/src/discovery.rs`.**

```rust
//! Walk the four well-known directories and produce a list of valid,
//! manifest-parsed plugin candidates. First-wins by plugin id.
//!
//! Path precedence (matches sub-projects A/B/C):
//! 1. <project>/.savvagent/plugins/<id>/plugin.toml
//! 2. <project>/.claude/plugins/<id>/plugin.toml
//! 3. ~/.savvagent/plugins/<id>/plugin.toml
//! 4. ~/.claude/plugins/<id>/plugin.toml

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::WasmPluginError;
use crate::manifest::PluginManifest;

#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub dir: PathBuf,
    pub source_scope: SourceScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceScope {
    ProjectSavvagent,
    ProjectClaude,
    UserSavvagent,
    UserClaude,
}

/// Result of one full discovery pass.
pub struct Discovery {
    pub plugins: Vec<DiscoveredPlugin>,
    pub warnings: Vec<String>,
}

/// Discover plugins from the four standard paths.
///
/// `project_root` is the directory returned by the same project-root
/// resolver `SAVVAGENT.md` uses (walk up for `.git/` or `.savvagent/`).
/// `home_dir` is `dirs::home_dir()` in production; injectable for tests.
pub fn discover(project_root: Option<&Path>, home_dir: Option<&Path>) -> Discovery {
    let mut by_id: HashMap<String, DiscoveredPlugin> = HashMap::new();
    let mut warnings = Vec::new();

    let paths: Vec<(Option<PathBuf>, SourceScope)> = vec![
        (project_root.map(|p| p.join(".savvagent/plugins")), SourceScope::ProjectSavvagent),
        (project_root.map(|p| p.join(".claude/plugins")), SourceScope::ProjectClaude),
        (home_dir.map(|h| h.join(".savvagent/plugins")), SourceScope::UserSavvagent),
        (home_dir.map(|h| h.join(".claude/plugins")), SourceScope::UserClaude),
    ];

    for (maybe_dir, scope) in paths {
        let Some(dir) = maybe_dir else { continue };
        if !dir.is_dir() {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                warnings.push(format!("[plugins] read_dir {dir:?}: {e}"));
                continue;
            }
        };
        for entry in entries.flatten() {
            let plugin_dir = entry.path();
            if !plugin_dir.is_dir() { continue }
            let manifest_path = plugin_dir.join("plugin.toml");
            if !manifest_path.is_file() { continue }
            let id_from_dir = match plugin_dir.file_name().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            match PluginManifest::load(&manifest_path, &id_from_dir) {
                Ok(m) => {
                    let entry = DiscoveredPlugin {
                        manifest: m,
                        dir: plugin_dir,
                        source_scope: scope,
                    };
                    by_id.entry(id_from_dir).or_insert(entry);  // first-wins
                }
                Err(WasmPluginError::Manifest(p, why)) => {
                    warnings.push(format!("[plugins] skipped {p:?}: {why}"));
                }
                Err(e) => {
                    warnings.push(format!("[plugins] skipped {plugin_dir:?}: {e}"));
                }
            }
        }
    }

    let plugins = by_id.into_values().collect();
    Discovery { plugins, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_plugin(dir: &Path, id: &str, world: &str) {
        let plugin_dir = dir.join(id);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let mut f = std::fs::File::create(plugin_dir.join("plugin.toml")).unwrap();
        write!(
            f,
            r#"
[plugin]
id = "{id}"
name = "{id}"
version = "0.1.0"
world = "{world}"
savvagent = "^0.18"
"#
        )
        .unwrap();
    }

    #[test]
    fn project_beats_user() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(project.join(".savvagent/plugins")).unwrap();
        std::fs::create_dir_all(home.join(".savvagent/plugins")).unwrap();

        write_plugin(&project.join(".savvagent/plugins"), "acme.demo", "plugin-static");
        write_plugin(&home.join(".savvagent/plugins"), "acme.demo", "plugin-static");

        let d = discover(Some(&project), Some(&home));
        assert_eq!(d.plugins.len(), 1);
        assert_eq!(d.plugins[0].source_scope, SourceScope::ProjectSavvagent);
    }

    #[test]
    fn savvagent_beats_claude_within_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(project.join(".savvagent/plugins")).unwrap();
        std::fs::create_dir_all(project.join(".claude/plugins")).unwrap();

        write_plugin(&project.join(".savvagent/plugins"), "acme.demo", "plugin-static");
        write_plugin(&project.join(".claude/plugins"), "acme.demo", "plugin-static");

        let d = discover(Some(&project), None);
        assert_eq!(d.plugins.len(), 1);
        assert_eq!(d.plugins[0].source_scope, SourceScope::ProjectSavvagent);
    }

    #[test]
    fn invalid_manifest_warns_but_does_not_block_others() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(project.join(".savvagent/plugins")).unwrap();
        write_plugin(&project.join(".savvagent/plugins"), "good.demo", "plugin-static");

        let bad_dir = project.join(".savvagent/plugins/bad.demo");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(bad_dir.join("plugin.toml"), "not toml at all = ::").unwrap();

        let d = discover(Some(&project), None);
        assert_eq!(d.plugins.len(), 1);
        assert_eq!(d.plugins[0].manifest.plugin.id, "good.demo");
        assert!(!d.warnings.is_empty());
    }
}
```

- [ ] **Step 3.4: Trust file — `crates/savvagent-plugin-wasm/src/trust.rs`.**

```rust
//! Manages `~/.savvagent/plugin-trust.toml` — the per-user trust ledger
//! for external plugins.
//!
//! Trust unit: SHA-256 over the plugin's full directory tree
//! (plugin.toml, plugin.wasm, any assets/* file), with filenames
//! sorted UTF-8 ascending.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::WasmPluginError;

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TrustFile {
    #[serde(default)]
    pub plugins: BTreeMap<String, TrustRecord>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TrustRecord {
    pub trusted: bool,
    pub sha256_tree: String,
    pub trusted_at: u64,
    pub source_url: Option<String>,
    #[serde(default)]
    pub disabled_reason: String,
}

impl TrustFile {
    pub fn load(home_dir: &Path) -> Result<Self, WasmPluginError> {
        let path = home_dir.join(".savvagent/plugin-trust.toml");
        if !path.exists() {
            return Ok(TrustFile::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| WasmPluginError::Io(path.clone(), e))?;
        toml::from_str(&text)
            .map_err(|e| WasmPluginError::Manifest(path, e.to_string()))
    }

    pub fn save(&self, home_dir: &Path) -> Result<(), WasmPluginError> {
        let path = home_dir.join(".savvagent/plugin-trust.toml");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| WasmPluginError::Io(parent.to_path_buf(), e))?;
        }
        let text = toml::to_string(self)
            .map_err(|e| WasmPluginError::Manifest(path.clone(), e.to_string()))?;
        std::fs::write(&path, text)
            .map_err(|e| WasmPluginError::Io(path, e))?;
        Ok(())
    }

    pub fn check(&self, id: &str, current_tree_hash: &str)
        -> TrustCheck
    {
        let Some(rec) = self.plugins.get(id) else {
            return TrustCheck::Untrusted;
        };
        if !rec.disabled_reason.is_empty() {
            return TrustCheck::Disabled(rec.disabled_reason.clone());
        }
        if !rec.trusted {
            return TrustCheck::Untrusted;
        }
        if rec.sha256_tree != current_tree_hash {
            return TrustCheck::HashMismatch {
                stored: rec.sha256_tree.clone(),
                actual: current_tree_hash.to_string(),
            };
        }
        TrustCheck::Ok
    }

    pub fn trust(&mut self, id: &str, tree_hash: String, source_url: Option<String>) {
        self.plugins.insert(
            id.to_string(),
            TrustRecord {
                trusted: true,
                sha256_tree: tree_hash,
                trusted_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs()).unwrap_or(0),
                source_url,
                disabled_reason: String::new(),
            },
        );
    }

    pub fn revoke(&mut self, id: &str) {
        self.plugins.remove(id);
    }

    pub fn set_disabled(&mut self, id: &str, reason: &str) {
        if let Some(rec) = self.plugins.get_mut(id) {
            rec.disabled_reason = reason.to_string();
        }
    }

    pub fn clear_disabled(&mut self, id: &str) {
        if let Some(rec) = self.plugins.get_mut(id) {
            rec.disabled_reason.clear();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustCheck {
    Ok,
    Untrusted,
    HashMismatch { stored: String, actual: String },
    Disabled(String),
}

/// Compute the SHA-256 over an entire plugin directory tree.
/// Filenames are sorted UTF-8 ascending before hashing.
pub fn tree_hash(plugin_dir: &Path) -> Result<String, WasmPluginError> {
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(plugin_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();
    files.sort();

    let mut hasher = Sha256::new();
    for file in files {
        let rel = file.strip_prefix(plugin_dir).unwrap_or(&file);
        let rel_str = rel.to_string_lossy();
        hasher.update(b"path:");
        hasher.update(rel_str.as_bytes());
        hasher.update(b"\n");
        let bytes = std::fs::read(&file)
            .map_err(|e| WasmPluginError::Io(file.clone(), e))?;
        hasher.update(b"size:");
        hasher.update(bytes.len().to_le_bytes());
        hasher.update(b"\n");
        hasher.update(&bytes);
        hasher.update(b"\n");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_hash_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("plugin.toml"), b"a").unwrap();
        std::fs::write(dir.join("plugin.wasm"), b"b").unwrap();
        let h1 = tree_hash(dir).unwrap();
        let h2 = tree_hash(dir).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn tree_hash_detects_change() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("plugin.toml"), b"a").unwrap();
        std::fs::write(dir.join("plugin.wasm"), b"b").unwrap();
        let h1 = tree_hash(dir).unwrap();
        std::fs::write(dir.join("plugin.wasm"), b"c").unwrap();
        let h2 = tree_hash(dir).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn trust_lifecycle() {
        let tmp = tempfile::tempdir().unwrap();
        let mut tf = TrustFile::default();
        tf.trust("acme.demo", "abc123".into(), Some("https://x".into()));
        tf.save(tmp.path()).unwrap();
        let loaded = TrustFile::load(tmp.path()).unwrap();
        assert_eq!(loaded.check("acme.demo", "abc123"), TrustCheck::Ok);
        assert_eq!(loaded.check("acme.demo", "xyz"),
                   TrustCheck::HashMismatch {
                       stored: "abc123".into(),
                       actual: "xyz".into(),
                   });
        assert_eq!(loaded.check("acme.other", "abc123"), TrustCheck::Untrusted);
    }

    #[test]
    fn disabled_record_is_disabled() {
        let mut tf = TrustFile::default();
        tf.trust("acme.demo", "abc123".into(), None);
        tf.set_disabled("acme.demo", "repeated-traps");
        match tf.check("acme.demo", "abc123") {
            TrustCheck::Disabled(reason) => assert_eq!(reason, "repeated-traps"),
            other => panic!("expected disabled, got {other:?}"),
        }
    }
}
```

- [ ] **Step 3.5: Wire modules — update `crates/savvagent-plugin-wasm/src/lib.rs`.**

```rust
//! Wasmtime-backed runtime for savvagent external plugins.

#![deny(missing_docs)]

pub mod discovery;
pub mod error;
pub mod manifest;
pub mod spp_convert;
pub mod trust;

pub use savvagent_plugin_wit as wit;
```

- [ ] **Step 3.6: Run tests.**

```bash
cargo test -p savvagent-plugin-wasm
```

Expected: all manifest, discovery, and trust tests pass.

- [ ] **Step 3.7: Commit.**

```bash
git add crates/savvagent-plugin-wasm/
git commit -m "feat(plugin-wasm): manifest + four-path discovery + plugin-trust.toml + tree-hash"
```

---

Continued in `2026-05-25-external-plugins-plan-part-2.md` — Tasks 4–15 cover the three adapters, host imports, fault-injection, three-strikes-disable, TUI wiring, `/plugins` command, examples, docs, and version bump. **The plan is split for length only; treat both files as one contiguous plan and complete tasks in numeric order.**
