# External Plugins — Plan Part 2 (Tasks 4–14)

**Continuation of:** `docs/superpowers/plans/2026-05-25-external-plugins.md`

> **For agentic workers:** This is the second half of the plan. Complete Tasks 1–3 from part 1 first. Use superpowers:subagent-driven-development.

---

## Task 4: Static-world adapter + host imports + fixture + tests

**Files:**
- Create: `crates/savvagent-plugin-wasm/src/adapter/mod.rs`
- Create: `crates/savvagent-plugin-wasm/src/adapter/static_.rs` (trailing `_` avoids the keyword)
- Create: `crates/savvagent-plugin-wasm/src/host_imports/mod.rs`
- Create: `crates/savvagent-plugin-wasm/src/host_imports/log.rs`
- Create: `crates/savvagent-plugin-wasm/src/host_imports/theme.rs`
- Create: `crates/savvagent-plugin-wasm/src/engine.rs`
- Create: `crates/savvagent-plugin-wasm/src/convert.rs` (WIT ↔ savvagent_plugin Effect, Manifest, etc.)
- Create: `crates/savvagent-plugin-wasm/tests/fixtures/static.wasm` (committed binary)
- Create: `crates/savvagent-plugin-wasm/tests/fixtures-src/static/` (source for reproducibility)
- Create: `crates/savvagent-plugin-wasm/tests/static_adapter.rs`
- Modify: `crates/savvagent-plugin-wasm/src/lib.rs`

- [ ] **Step 4.1: Engine singleton — `src/engine.rs`.**

```rust
//! One shared `wasmtime::Engine` per process. Cheap to clone (internal Arc).

use std::sync::OnceLock;
use wasmtime::{Config, Engine};

static ENGINE: OnceLock<Engine> = OnceLock::new();

/// Returns the process-wide shared wasmtime engine. Initializes on first call.
pub fn shared_engine() -> Engine {
    ENGINE
        .get_or_init(|| {
            let mut cfg = Config::new();
            cfg.async_support(true);
            cfg.wasm_component_model(true);
            cfg.epoch_interruption(true);
            Engine::new(&cfg).expect("wasmtime engine init")
        })
        .clone()
}
```

- [ ] **Step 4.2: WIT↔Plugin conversions — `src/convert.rs`.**

These map between the WIT-side types from `savvagent-plugin-wit::static_world` (and `interactive_world`) and `savvagent_plugin::{Effect, Manifest, HookKind, ScreenArgs, StyledLine}`. Mechanical, like `spp_convert.rs`. Pattern:

```rust
use savvagent_plugin as sp;
use savvagent_plugin_wit::static_world::savvagent::plugin::types as wit;

pub fn effect_from_wit(e: wit::Effect) -> sp::Effect {
    match e {
        wit::Effect::PushNote(n) => sp::Effect::PushNote(sp::Note {
            text: n.text,
            level: note_level_from_wit(n.level),
        }),
        wit::Effect::OpenScreen(t) => sp::Effect::OpenScreen {
            plugin_id: sp::PluginId::new(&t.plugin_id).expect("valid id"),
            screen_id: t.screen_id,
            args: serde_json::from_str(&t.args_json).unwrap_or(sp::ScreenArgs::default()),
        },
        wit::Effect::SetTheme(slug) => sp::Effect::SetTheme(slug),
        wit::Effect::RunSlash(s) => sp::Effect::RunSlash { name: s.name, args: s.args },
        wit::Effect::SaveTranscript => sp::Effect::SaveTranscript,
        wit::Effect::ClearLog => sp::Effect::ClearLog,
        wit::Effect::RegisterKeybinding(_) => unreachable!("not emitted at runtime in v0.18"),
    }
}

pub fn manifest_from_wit(m: wit::PluginManifest) -> Result<sp::Manifest, crate::error::WasmPluginError> {
    Ok(sp::Manifest {
        id: sp::PluginId::new(&m.id)
            .map_err(|e| crate::error::WasmPluginError::InvalidId(m.id.clone(), e.to_string()))?,
        name: m.name,
        version: m.version,
        description: m.description,
        kind: match m.kind {
            wit::PluginKind::Core => sp::PluginKind::Core,
            wit::PluginKind::Optional => sp::PluginKind::Optional,
        },
        contributions: contributions_from_wit(m.contributions),
    })
}

// hook_kind_from_wit, contributions_from_wit, styled_line_from_wit, ...
// every variant has one mechanical match arm + a unit test.
```

- [ ] **Step 4.3: Log host import — `src/host_imports/log.rs`.**

```rust
//! `log(level, msg)` capability — host-imported by all three worlds.

pub fn log_message(level: u8, msg: &str) {
    match level {
        0 => tracing::trace!(target: "plugin-wasm", "{msg}"),
        1 => tracing::debug!(target: "plugin-wasm", "{msg}"),
        2 => tracing::info!(target: "plugin-wasm", "{msg}"),
        3 => tracing::warn!(target: "plugin-wasm", "{msg}"),
        _ => tracing::error!(target: "plugin-wasm", "{msg}"),
    }
}
```

The actual host-import wrapping happens inside the adapter where it can be linked into the `Linker<HostState>`. This module holds the pure logic.

- [ ] **Step 4.4: Theme host import — `src/host_imports/theme.rs`.**

```rust
//! `current-theme()` capability — returns the active theme's color map.

use std::sync::Arc;
use tokio::sync::RwLock;

pub type ThemeProvider = Arc<RwLock<Vec<(String, savvagent_plugin::ThemeColor)>>>;

/// Construct a ThemeProvider holding an initial snapshot.
pub fn provider(initial: Vec<(String, savvagent_plugin::ThemeColor)>) -> ThemeProvider {
    Arc::new(RwLock::new(initial))
}
```

The TUI calls `provider.write().await.clone_from(&new_theme_map)` whenever `SetTheme` is applied. The host import inside the adapter reads via `provider.read().await.clone()` and converts to the WIT representation.

- [ ] **Step 4.5: Adapter module skeleton — `src/adapter/mod.rs`.**

```rust
//! Adapters: bridge wasm components to `Box<dyn savvagent_plugin::Plugin>`.

mod static_;
// interactive and provider land in tasks 5 and 6.

pub use static_::StaticAdapter;
```

- [ ] **Step 4.6: Static adapter — `src/adapter/static_.rs`.**

```rust
//! Static-world adapter: load a .wasm, expose it as `Box<dyn Plugin>`.

use std::sync::Arc;
use async_trait::async_trait;
use tokio::sync::Mutex;
use wasmtime::component::{Component, Linker, InstancePre};
use wasmtime::Store;

use savvagent_plugin::{Effect, HostEvent, Manifest, Plugin, PluginError, Region, ScreenArgs,
                       ScreenStyledLineExport as _, StyledLine, ThemeEntry};
use savvagent_plugin_wit::static_world as ws;

use crate::convert::*;
use crate::engine::shared_engine;
use crate::error::WasmPluginError;
use crate::host_imports::theme::ThemeProvider;
use crate::manifest::PluginManifest as DiskManifest;

/// State stored inside the wasmtime Store. Mutable; one per StaticAdapter.
pub(crate) struct StaticHostState {
    pub plugin_id: String,
    pub theme: ThemeProvider,
}

pub struct StaticAdapter {
    cached_manifest: Manifest,
    pre: InstancePre<StaticHostState>,
    store: Mutex<Store<StaticHostState>>,
    instance: Mutex<Option<ws::PluginStatic>>,  // re-built lazily on trap
    disk_manifest: Arc<DiskManifest>,
}

impl StaticAdapter {
    pub async fn new(
        disk_manifest: Arc<DiskManifest>,
        plugin_dir: &std::path::Path,
        theme: ThemeProvider,
    ) -> Result<Self, WasmPluginError> {
        let engine = shared_engine();
        let component = Component::from_file(&engine, plugin_dir.join("plugin.wasm"))
            .map_err(WasmPluginError::Wasmtime)?;

        let mut linker: Linker<StaticHostState> = Linker::new(&engine);
        ws::PluginStatic::add_to_linker(&mut linker, |s| s)
            .map_err(WasmPluginError::Wasmtime)?;

        // Wire host imports.
        linker.root().func_wrap_async("log",
            |_caller, (level, msg): (u32, String)| Box::new(async move {
                crate::host_imports::log::log_message(level as u8, &msg);
                Ok(())
            }))
            .map_err(WasmPluginError::Wasmtime)?;

        linker.root().func_wrap_async("current-theme",
            |caller: wasmtime::StoreContextMut<'_, StaticHostState>, ()| {
                let theme = caller.data().theme.clone();
                Box::new(async move {
                    let snap = theme.read().await.clone();
                    Ok((snap.into_iter()
                        .map(|(name, color)| (name, theme_color_to_wit(color)))
                        .collect::<Vec<_>>(),))
                })
            })
            .map_err(WasmPluginError::Wasmtime)?;

        let pre = linker.instantiate_pre(&component)
            .map_err(WasmPluginError::Wasmtime)?;

        let mut store = Store::new(&engine, StaticHostState {
            plugin_id: disk_manifest.plugin.id.clone(),
            theme,
        });
        let instance = ws::PluginStatic::instantiate_pre_async(&mut store, &pre)
            .await
            .map_err(WasmPluginError::Wasmtime)?;

        // Read and cache the manifest export.
        let wit_manifest = instance.call_manifest(&mut store).await
            .map_err(WasmPluginError::Wasmtime)?
            .map_err(|e| WasmPluginError::Manifest(
                plugin_dir.join("plugin.wasm"),
                format!("{e:?}"),
            ))?;
        let cached_manifest = manifest_from_wit(wit_manifest)?;

        Ok(Self {
            cached_manifest,
            pre,
            store: Mutex::new(store),
            instance: Mutex::new(Some(instance)),
            disk_manifest,
        })
    }

    /// Drop and rebuild the store + instance. Called on trap recovery.
    async fn rebuild_instance(&self) -> Result<(), WasmPluginError> {
        let engine = shared_engine();
        let mut store = Store::new(&engine, StaticHostState {
            plugin_id: self.disk_manifest.plugin.id.clone(),
            theme: self.store.lock().await.data().theme.clone(),
        });
        let instance = ws::PluginStatic::instantiate_pre_async(&mut store, &self.pre)
            .await
            .map_err(WasmPluginError::Wasmtime)?;
        *self.store.lock().await = store;
        *self.instance.lock().await = Some(instance);
        Ok(())
    }
}

#[async_trait]
impl Plugin for StaticAdapter {
    fn manifest(&self) -> Manifest {
        self.cached_manifest.clone()
    }

    async fn handle_slash(&mut self, name: &str, args: Vec<String>)
        -> Result<Vec<Effect>, PluginError>
    {
        let inst_guard = self.instance.lock().await;
        let inst = inst_guard.as_ref()
            .ok_or_else(|| PluginError::other("wasm instance is None — recovery pending"))?
            .clone();
        drop(inst_guard);

        let mut store = self.store.lock().await;
        let wit_effects = inst
            .call_handle_slash(&mut *store, name, &args)
            .await
            .map_err(|e| PluginError::other(format!("wasm trap: {e}")))?
            .map_err(|e| PluginError::other(format!("plugin error: {e:?}")))?;
        Ok(wit_effects.into_iter().map(effect_from_wit).collect())
    }

    async fn on_event(&mut self, event: HostEvent)
        -> Result<Vec<Effect>, PluginError>
    {
        let event_json = serde_json::to_string(&event)
            .map_err(|e| PluginError::other(format!("serialize HostEvent: {e}")))?;
        let inst_guard = self.instance.lock().await;
        let inst = inst_guard.as_ref()
            .ok_or_else(|| PluginError::other("wasm instance is None"))?
            .clone();
        drop(inst_guard);

        let mut store = self.store.lock().await;
        let wit_effects = inst
            .call_on_event(&mut *store, &event_json)
            .await
            .map_err(|e| PluginError::other(format!("wasm trap: {e}")))?
            .map_err(|e| PluginError::other(format!("plugin error: {e:?}")))?;
        Ok(wit_effects.into_iter().map(effect_from_wit).collect())
    }

    fn render_slot(&self, _slot_id: &str, _region: Region) -> Vec<StyledLine> {
        // render_slot is `&self` + sync in the trait. We hold a mutex and
        // call wasm async — bridge via tokio::runtime::Handle::block_on
        // when called from a tokio context, or return empty if not.
        //
        // v0.18.0: return empty by default; static plugins that contribute
        // render slots must mark themselves as needing the async bridge.
        // Track in a later release.
        vec![]
    }

    fn themes(&self) -> Vec<ThemeEntry> {
        // Pre-fetched at construction. Cached on self.cached_themes — but
        // we didn't add that field above to keep the example short.
        // Implementer: add `cached_themes: Vec<ThemeEntry>` and populate
        // in `new()` by calling `inst.call_themes(&mut store).await`.
        Vec::new()
    }
}

fn theme_color_to_wit(c: savvagent_plugin::ThemeColor)
    -> ws::savvagent::plugin::types::ThemeColor
{
    use ws::savvagent::plugin::types as t;
    use savvagent_plugin::ThemeColor as P;
    match c {
        P::Reset => t::ThemeColor::Reset,
        P::Black => t::ThemeColor::Black,
        P::Red => t::ThemeColor::Red,
        // ... one arm per variant; see savvagent_plugin::ThemeColor
        _ => t::ThemeColor::Reset,
    }
}
```

> **Implementer note:** The exact bindgen-emitted call/type names depend on `wit-bindgen 0.34`'s rules. Where the code above uses `ws::PluginStatic`, the actual name may be `ws::PluginStatic` or `ws::exports::PluginStaticExports` — adjust at write time. The shape (long-lived store, async exports, manifest cached at construction, rebuild on trap) is the load-bearing part.

- [ ] **Step 4.7: Build the static fixture.**

Create `crates/savvagent-plugin-wasm/tests/fixtures-src/static/Cargo.toml`:

```toml
[package]
name = "fixture-static"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.34"
```

Create `crates/savvagent-plugin-wasm/tests/fixtures-src/static/src/lib.rs`:

```rust
wit_bindgen::generate!({
    path: "../../../savvagent-plugin-wit/wit",
    world: "plugin-static",
});

use exports::savvagent::plugin::types as t;

struct Component;

impl Guest for Component {
    fn manifest() -> Result<t::PluginManifest, t::PluginError> {
        Ok(t::PluginManifest {
            id: "fixture.static".into(),
            name: "fixture-static".into(),
            version: "0.1.0".into(),
            description: "test fixture".into(),
            kind: t::PluginKind::Optional,
            contributions: t::Contributions {
                slash_commands: vec!["echo".into()],
                hooks: vec![t::HookKind::TurnStart],
                screens: vec![],
                render_slots: vec![],
                keybindings: vec![],
                themes: false,
            },
        })
    }

    fn handle_slash(name: String, args: Vec<String>)
        -> Result<Vec<t::Effect>, t::PluginError>
    {
        if name == "echo" {
            return Ok(vec![t::Effect::PushNote(t::Note {
                text: args.join(" "),
                level: t::NoteLevel::Info,
            })]);
        }
        Ok(vec![])
    }

    fn on_event(_event_json: String) -> Result<Vec<t::Effect>, t::PluginError> {
        Ok(vec![])
    }

    fn render_slot(_slot_id: String, _area: t::Region) -> Vec<t::StyledLine> {
        vec![]
    }

    fn themes() -> Vec<t::ThemeEntry> {
        vec![]
    }
}

export!(Component);
```

Build via a Justfile entry:

```just
build-fixtures:
    cd crates/savvagent-plugin-wasm/tests/fixtures-src/static && \
        cargo component build --release && \
        cp target/wasm32-wasip2/release/fixture_static.wasm \
           ../../fixtures/static.wasm
    cd crates/savvagent-plugin-wasm/tests/fixtures-src/interactive && \
        cargo component build --release && \
        cp target/wasm32-wasip2/release/fixture_interactive.wasm \
           ../../fixtures/interactive.wasm
    cd crates/savvagent-plugin-wasm/tests/fixtures-src/provider && \
        cargo component build --release && \
        cp target/wasm32-wasip2/release/fixture_provider.wasm \
           ../../fixtures/provider.wasm
```

Then commit the resulting `.wasm` files. Don't worry about size; ~50 KB per fixture is expected and accepted (per spec §6).

- [ ] **Step 4.8: Static adapter integration test — `tests/static_adapter.rs`.**

```rust
use std::sync::Arc;
use savvagent_plugin::Plugin;
use savvagent_plugin_wasm::adapter::StaticAdapter;
use savvagent_plugin_wasm::host_imports::theme;
use savvagent_plugin_wasm::manifest::PluginManifest;

#[tokio::test]
async fn static_adapter_handle_slash_echo() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path();
    std::fs::write(plugin_dir.join("plugin.toml"), r#"
[plugin]
id = "fixture.static"
name = "fixture-static"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#).unwrap();
    std::fs::copy(
        "tests/fixtures/static.wasm",
        plugin_dir.join("plugin.wasm"),
    ).unwrap();

    let dm = Arc::new(PluginManifest::load(
        &plugin_dir.join("plugin.toml"), "fixture.static",
    ).unwrap());
    let theme = theme::provider(vec![]);
    let mut adapter = StaticAdapter::new(dm, plugin_dir, theme).await.unwrap();

    let effects = adapter
        .handle_slash("echo", vec!["hello".into(), "world".into()])
        .await
        .unwrap();
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        savvagent_plugin::Effect::PushNote(n) => assert_eq!(n.text, "hello world"),
        other => panic!("expected PushNote, got {other:?}"),
    }
}
```

- [ ] **Step 4.9: Run.**

```bash
just build-fixtures
cargo test -p savvagent-plugin-wasm
```

Expected: `static_adapter_handle_slash_echo` passes.

- [ ] **Step 4.10: Commit.**

```bash
git add crates/savvagent-plugin-wasm/ Justfile
git commit -m "feat(plugin-wasm): static-world adapter + log/current-theme imports + fixture"
```

---

## Task 5: Interactive-world adapter + draw imports + fixture + tests

**Files:**
- Create: `crates/savvagent-plugin-wasm/src/adapter/interactive.rs`
- Create: `crates/savvagent-plugin-wasm/src/host_imports/draw.rs`
- Create: `crates/savvagent-plugin-wasm/tests/fixtures-src/interactive/`
- Create: `crates/savvagent-plugin-wasm/tests/fixtures/interactive.wasm`
- Create: `crates/savvagent-plugin-wasm/tests/interactive_adapter.rs`
- Modify: `crates/savvagent-plugin-wasm/src/adapter/mod.rs`

- [ ] **Step 5.1: Draw-primitive host imports — `src/host_imports/draw.rs`.**

```rust
//! Buffer-bridge for interactive plugins. The TUI sets `active_buffer`
//! in InteractiveHostState before calling `render`; draw imports
//! deref the pointer under the per-screen-open store lock.
//!
//! INVARIANT: `active_buffer` is `Some(non-null)` only for the duration
//! of a single `render` call. The pointer's lifetime is guaranteed by:
//!   1. The Buffer reference held in the caller's frame for the whole
//!      adapter's `render` await.
//!   2. The per-screen-open Store, so no other render can be in flight.
//!   3. Cleared in the adapter's `render` after the wasm call returns.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

pub struct DrawState {
    /// Raw pointer into the active render frame's buffer. See INVARIANT.
    pub active_buffer: Option<*mut Buffer>,
}

// SAFETY: Send is required because Store<DrawState> is moved between awaits.
// Reads/writes via the pointer are serialized by the per-screen-open Mutex
// in WasmScreen.
unsafe impl Send for DrawState {}

pub fn draw_text(
    state: &mut DrawState,
    x: u16, y: u16, text: &str,
    fg: Color, bg: Color, modifier: Modifier,
) {
    let Some(buf_ptr) = state.active_buffer else { return };
    // SAFETY: see INVARIANT above. Only this thread can mutate the buffer
    // while the render call is in flight.
    let buf = unsafe { &mut *buf_ptr };
    let area = buf.area;
    let mut col = x;
    for (ch_index, ch) in text.chars().enumerate() {
        let cx = x + ch_index as u16;
        if cx >= area.width || y >= area.height { break }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(Style::default().fg(fg).bg(bg).add_modifier(modifier));
        }
        col = cx;
    }
    let _ = col;
}

pub fn clear_area(state: &mut DrawState, area: Rect, bg: Color) {
    let Some(buf_ptr) = state.active_buffer else { return };
    let buf = unsafe { &mut *buf_ptr };
    for y in area.y..(area.y + area.height) {
        for x in area.x..(area.x + area.width) {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_bg(bg);
            }
        }
    }
}

// draw_block: render a ratatui::widgets::Block with the given border and title.
// draw_line: horizontal/vertical line via U+2500 / U+2502.
// Implementations are mechanical; see ratatui's Block / Line widgets.
```

- [ ] **Step 5.2: Interactive adapter — `src/adapter/interactive.rs`.**

The key difference from static: per-screen-open Store. The `Plugin` impl produces a fresh `Box<dyn Screen>` from `create_screen`.

```rust
use std::sync::Arc;
use tokio::sync::Mutex;
use async_trait::async_trait;
use wasmtime::component::{Component, Linker, InstancePre, ResourceAny};
use wasmtime::Store;

use savvagent_plugin::{Effect, Manifest, Plugin, PluginError, Screen, ScreenArgs};
use savvagent_plugin_wit::interactive_world as wi;

use crate::convert::*;
use crate::engine::shared_engine;
use crate::error::WasmPluginError;
use crate::host_imports::draw::DrawState;
use crate::host_imports::theme::ThemeProvider;
use crate::manifest::PluginManifest as DiskManifest;

pub(crate) struct InteractiveHostState {
    pub plugin_id: String,
    pub theme: ThemeProvider,
    pub draw: DrawState,
    pub cached_tips: String,
}

pub struct InteractiveAdapter {
    cached_manifest: Manifest,
    pre: Arc<InstancePre<InteractiveHostState>>,
    theme: ThemeProvider,
    disk_manifest: Arc<DiskManifest>,
}

impl InteractiveAdapter {
    pub async fn new(
        disk_manifest: Arc<DiskManifest>,
        plugin_dir: &std::path::Path,
        theme: ThemeProvider,
    ) -> Result<Self, WasmPluginError> {
        let engine = shared_engine();
        let component = Component::from_file(&engine, plugin_dir.join("plugin.wasm"))
            .map_err(WasmPluginError::Wasmtime)?;

        let mut linker: Linker<InteractiveHostState> = Linker::new(&engine);
        // Wire log, current-theme, draw-text, draw-block, draw-line, clear-area.
        // See task 4's adapter for the log/current-theme pattern.
        wi::PluginInteractive::add_to_linker(&mut linker, |s| s)
            .map_err(WasmPluginError::Wasmtime)?;
        link_draw_primitives(&mut linker)?;

        let pre = linker.instantiate_pre(&component)
            .map_err(WasmPluginError::Wasmtime)?;
        let pre = Arc::new(pre);

        // Read manifest by instantiating once and dropping.
        let mut store = Store::new(&engine, InteractiveHostState {
            plugin_id: disk_manifest.plugin.id.clone(),
            theme: theme.clone(),
            draw: DrawState { active_buffer: None },
            cached_tips: String::new(),
        });
        let instance = wi::PluginInteractive::instantiate_pre_async(&mut store, &pre)
            .await
            .map_err(WasmPluginError::Wasmtime)?;
        let wit_manifest = instance.call_manifest(&mut store).await
            .map_err(WasmPluginError::Wasmtime)?
            .map_err(|e| WasmPluginError::Manifest(plugin_dir.into(), format!("{e:?}")))?;
        let cached_manifest = manifest_from_wit(wit_manifest)?;

        Ok(Self { cached_manifest, pre, theme, disk_manifest })
    }
}

#[async_trait]
impl Plugin for InteractiveAdapter {
    fn manifest(&self) -> Manifest {
        self.cached_manifest.clone()
    }

    fn create_screen(&self, id: &str, args: ScreenArgs)
        -> Result<Box<dyn Screen>, PluginError>
    {
        // Block on the async path. Real impl uses tokio::runtime::Handle::current()
        // since `create_screen` is sync in the trait. If we're not in a tokio
        // context we error.
        let engine = shared_engine();
        let mut store = Store::new(&engine, InteractiveHostState {
            plugin_id: self.disk_manifest.plugin.id.clone(),
            theme: self.theme.clone(),
            draw: DrawState { active_buffer: None },
            cached_tips: String::new(),
        });
        let invocation_json = serde_json::to_string(&args)
            .map_err(|e| PluginError::other(format!("serialize ScreenArgs: {e}")))?;

        let pre = self.pre.clone();
        let id = id.to_string();
        let (store, handle, instance) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let instance = wi::PluginInteractive::instantiate_pre_async(&mut store, &pre)
                    .await
                    .map_err(|e| PluginError::other(format!("instantiate: {e}")))?;
                let handle = instance.call_create_screen(
                    &mut store,
                    &id,
                    &wi::exports::savvagent::plugin::types::ScreenArgs {
                        invocation_json,
                        terminal_width: 80,    // re-passed at render time
                        terminal_height: 24,
                    },
                ).await
                    .map_err(|e| PluginError::other(format!("create_screen trap: {e}")))?
                    .map_err(|e| PluginError::other(format!("plugin error: {e:?}")))?;
                Ok::<_, PluginError>((store, handle, instance))
            })
        })?;

        Ok(Box::new(WasmScreen {
            store: Mutex::new(store),
            instance,
            handle,
        }))
    }
}

pub struct WasmScreen {
    store: Mutex<Store<InteractiveHostState>>,
    instance: wi::PluginInteractive,
    handle: ResourceAny,  // ScreenInstance resource handle
}

#[async_trait]
impl Screen for WasmScreen {
    fn id(&self) -> String { "wasm".into() }

    async fn on_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> Vec<Effect> {
        let mut store = self.store.lock().await;
        let key_wit = key_event_to_wit(key);
        let wit_effects = self.instance
            .screen_instance()
            .call_on_key(&mut *store, self.handle, key_wit)
            .await
            .unwrap_or_default()
            .unwrap_or_default();
        // Refresh tips cache.
        store.data_mut().cached_tips = self.instance
            .screen_instance()
            .call_tips(&mut *store, self.handle)
            .await
            .unwrap_or_default();
        wit_effects.into_iter().map(effect_from_wit).collect()
    }

    fn render(&self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        // sync render -> async wasm: block_in_place + block_on the tokio handle.
        let mut store = match self.store.try_lock() {
            Ok(g) => g,
            Err(_) => return,  // contended; skip frame
        };
        store.data_mut().draw.active_buffer = Some(buf as *mut _);
        let _ = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(
                self.instance.screen_instance().call_render(&mut *store, self.handle, area.into())
            )
        });
        store.data_mut().draw.active_buffer = None;
    }

    fn tips(&self) -> Option<String> {
        // sync read of the cached value.
        match self.store.try_lock() {
            Ok(g) => Some(g.data().cached_tips.clone()),
            Err(_) => None,
        }
    }
}

fn link_draw_primitives(_linker: &mut Linker<InteractiveHostState>)
    -> Result<(), WasmPluginError>
{
    // Bind draw-text/draw-block/draw-line/clear-area to the
    // host_imports::draw functions, reading state via Caller::data_mut().
    // ~30 LoC; the actual wasmtime API names depend on bindgen output.
    Ok(())
}

fn key_event_to_wit(_key: ratatui::crossterm::event::KeyEvent)
    -> wi::savvagent::plugin::types::KeyEventPortable
{
    // mechanical conversion; one match arm per KeyCode variant.
    todo!("key conversion")
}
```

> **Implementer note:** The exact `Screen` trait shape may differ — check `crates/savvagent-plugin/src/screen.rs` for the canonical methods (`on_key`, `render`, `tips`, `id`) and adjust the impl accordingly. Drop the `todo!()` before commit.

- [ ] **Step 5.3: Interactive fixture source.**

Create `tests/fixtures-src/interactive/Cargo.toml` and `src/lib.rs` mirroring task 4.7's pattern, but for `plugin-interactive`. The fixture:
- Returns a fixed manifest from `manifest()`.
- `create_screen("test", args)` returns a resource.
- `on_key(key)` records the key sequence.
- `render(area)` calls `draw_text(0, 0, "hello", green, black, no-mods)`.
- `tips()` returns "tips text".

Build via `just build-fixtures`. Commit `tests/fixtures/interactive.wasm`.

- [ ] **Step 5.4: Integration test — `tests/interactive_adapter.rs`.**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn interactive_adapter_renders_hello() {
    // Load fixture as in task 4.8, then call create_screen, render to a
    // 10x3 buffer, assert that the buffer contains "hello" at (0,0).
}
```

- [ ] **Step 5.5: Commit.**

```bash
git add crates/savvagent-plugin-wasm/
git commit -m "feat(plugin-wasm): interactive-world adapter + draw imports + fixture"
```

---

## Task 6: Provider-world adapter + http/keyring/progress imports + fixture + tests

**Files:**
- Create: `crates/savvagent-plugin-wasm/src/adapter/provider.rs`
- Create: `crates/savvagent-plugin-wasm/src/host_imports/http.rs`
- Create: `crates/savvagent-plugin-wasm/src/host_imports/keyring.rs`
- Create: `crates/savvagent-plugin-wasm/src/host_imports/progress.rs`
- Create: `crates/savvagent-plugin-wasm/tests/fixtures-src/provider/`
- Create: `crates/savvagent-plugin-wasm/tests/fixtures/provider.wasm`
- Create: `crates/savvagent-plugin-wasm/tests/provider_adapter.rs`

- [ ] **Step 6.1: HTTP host import — `src/host_imports/http.rs`.**

```rust
//! HTTP capability for provider plugins. Enforces allowed-hosts at every call.

use std::sync::Arc;
use std::time::Duration;
use reqwest::Client;
use savvagent_plugin_wit::provider_world::savvagent::plugin::http_capability as wit;

pub struct HttpState {
    pub client: Client,
    pub allowed_hosts: Arc<Vec<String>>,
}

impl HttpState {
    pub fn new(allowed_hosts: Vec<String>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .use_rustls_tls()
            .build()
            .expect("reqwest client");
        Self { client, allowed_hosts: Arc::new(allowed_hosts) }
    }

    pub async fn fetch(&self, req: wit::HttpRequest)
        -> Result<wit::HttpResponse, wit::HttpError>
    {
        let url = url::Url::parse(&req.url)
            .map_err(|e| wit::HttpError::Transport(e.to_string()))?;
        let host = url.host_str().unwrap_or("").to_string();
        if !self.allowed_hosts.iter().any(|h| h == &host) {
            return Err(wit::HttpError::DeniedHost(host));
        }
        let method = req.method.parse::<reqwest::Method>()
            .map_err(|_| wit::HttpError::DeniedMethod(req.method.clone()))?;
        let mut rb = self.client.request(method, url);
        for (k, v) in req.headers { rb = rb.header(k, v); }
        if let Some(body) = req.body { rb = rb.body(body); }
        if let Some(ms) = req.timeout_ms {
            rb = rb.timeout(Duration::from_millis(ms.min(300_000) as u64));
        }
        let resp = rb.send().await
            .map_err(|e| if e.is_timeout() {
                wit::HttpError::Timeout
            } else {
                wit::HttpError::Transport(e.to_string())
            })?;
        let status = resp.status().as_u16();
        let headers: Vec<(String, String)> = resp.headers().iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let body = resp.bytes().await
            .map_err(|e| wit::HttpError::Transport(e.to_string()))?
            .to_vec();
        if body.len() > 32 * 1024 * 1024 {
            return Err(wit::HttpError::BodyTooLarge(body.len() as u64));
        }
        Ok(wit::HttpResponse { status, headers, body })
    }
}
```

- [ ] **Step 6.2: Keyring host import — `src/host_imports/keyring.rs`.**

```rust
//! Keyring capability — fixed service "savvagent", account-whitelist enforced.

use std::sync::Arc;
use savvagent_plugin_wit::provider_world::savvagent::plugin::keyring_capability as wit;

pub struct KeyringState {
    pub allowed_accounts: Arc<Vec<String>>,
}

impl KeyringState {
    pub fn get(&self, account: &str) -> Result<String, wit::KeyringError> {
        if !self.allowed_accounts.iter().any(|a| a == account) {
            return Err(wit::KeyringError::Denied(account.to_string()));
        }
        let entry = keyring::Entry::new("savvagent", account)
            .map_err(|e| wit::KeyringError::Backend(e.to_string()))?;
        match entry.get_password() {
            Ok(s) => Ok(s),
            Err(keyring::Error::NoEntry) => Err(wit::KeyringError::NotFound),
            Err(e) => Err(wit::KeyringError::Backend(e.to_string())),
        }
    }
}
```

- [ ] **Step 6.3: Progress host import — `src/host_imports/progress.rs`.**

```rust
//! Progress capability — forwards stream-events to the active emitter.

use savvagent_mcp::StreamEmitter;
use savvagent_plugin_wit::provider_world::savvagent::spp::types as wit;
use savvagent_protocol::StreamEvent;

pub struct ProgressState {
    pub active_emitter: Option<Box<dyn StreamEmitter>>,
}

impl ProgressState {
    pub async fn emit(&mut self, event: wit::StreamEvent) {
        let Some(emitter) = self.active_emitter.as_mut() else { return };
        let spp_event: StreamEvent = event.into();
        let _ = emitter.emit(spp_event).await;
    }
}
```

- [ ] **Step 6.4: Provider adapter — `src/adapter/provider.rs`.**

```rust
//! Provider-world adapter: bridges a wasm component to
//! `Box<dyn savvagent_mcp::ProviderClient>`.

use std::sync::Arc;
use async_trait::async_trait;
use tokio::sync::Mutex;
use wasmtime::component::{Component, Linker, InstancePre};
use wasmtime::Store;

use savvagent_mcp::{ProviderClient, StreamEmitter};
use savvagent_protocol::{CompleteRequest, CompleteResponse, ProviderError,
                          ModelInfo, CountTokensRequest, CountTokensResponse};
use savvagent_plugin_wit::provider_world as wp;

use crate::engine::shared_engine;
use crate::error::WasmPluginError;
use crate::host_imports::{http::HttpState, keyring::KeyringState, progress::ProgressState};
use crate::manifest::PluginManifest as DiskManifest;
use crate::spp_convert::*;

pub(crate) struct ProviderHostState {
    pub plugin_id: String,
    pub http: HttpState,
    pub keyring: KeyringState,
    pub progress: ProgressState,
}

pub struct WasmProviderClient {
    pre: Arc<InstancePre<ProviderHostState>>,
    disk_manifest: Arc<DiskManifest>,
    // Serialize concurrent calls; one store per call from a small pool.
    pool: Mutex<Vec<Store<ProviderHostState>>>,
}

impl WasmProviderClient {
    pub async fn new(disk_manifest: Arc<DiskManifest>, plugin_dir: &std::path::Path)
        -> Result<Self, WasmPluginError>
    {
        let engine = shared_engine();
        let component = Component::from_file(&engine, plugin_dir.join("plugin.wasm"))
            .map_err(WasmPluginError::Wasmtime)?;

        let mut linker: Linker<ProviderHostState> = Linker::new(&engine);
        wp::PluginProvider::add_to_linker(&mut linker, |s| s)
            .map_err(WasmPluginError::Wasmtime)?;
        // Wire log, http, keyring, progress imports.
        // (~80 LoC of func_wrap_async calls; mechanical.)

        let pre = Arc::new(linker.instantiate_pre(&component)
            .map_err(WasmPluginError::Wasmtime)?);

        Ok(Self {
            pre,
            disk_manifest,
            pool: Mutex::new(Vec::new()),
        })
    }

    fn new_store(&self, emitter: Option<Box<dyn StreamEmitter>>) -> Store<ProviderHostState> {
        let security = self.disk_manifest.security.clone().unwrap_or_else(|| {
            crate::manifest::SecuritySection {
                allowed_hosts: vec![],
                keyring_accounts: vec![],
            }
        });
        Store::new(&shared_engine(), ProviderHostState {
            plugin_id: self.disk_manifest.plugin.id.clone(),
            http: HttpState::new(security.allowed_hosts),
            keyring: KeyringState {
                allowed_accounts: Arc::new(security.keyring_accounts),
            },
            progress: ProgressState { active_emitter: emitter },
        })
    }
}

#[async_trait]
impl ProviderClient for WasmProviderClient {
    async fn complete(
        &self,
        req: CompleteRequest,
        emitter: Box<dyn StreamEmitter>,
    ) -> Result<CompleteResponse, ProviderError> {
        let mut store = self.new_store(Some(emitter));
        let instance = wp::PluginProvider::instantiate_pre_async(&mut store, &self.pre)
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let wit_req: wp::savvagent::spp::types::CompleteRequest = req.into();
        let res = instance.call_complete(&mut store, &wit_req).await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        res.map(Into::into).map_err(|e| {
            let pe: savvagent_protocol::ProviderError = e.into();
            pe
        })
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let mut store = self.new_store(None);
        let instance = wp::PluginProvider::instantiate_pre_async(&mut store, &self.pre)
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let res = instance.call_list_models(&mut store).await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        res.map(|v| v.into_iter().map(Into::into).collect())
            .map_err(|e| { let pe: savvagent_protocol::ProviderError = e.into(); pe })
    }

    async fn count_tokens(&self, req: CountTokensRequest)
        -> Result<CountTokensResponse, ProviderError>
    {
        let mut store = self.new_store(None);
        let instance = wp::PluginProvider::instantiate_pre_async(&mut store, &self.pre)
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let wit_req: wp::savvagent::spp::types::CountTokensRequest = req.into();
        let res = instance.call_count_tokens(&mut store, &wit_req).await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        res.map(Into::into)
            .map_err(|e| { let pe: savvagent_protocol::ProviderError = e.into(); pe })
    }
}
```

- [ ] **Step 6.5: Provider fixture source.**

Create `tests/fixtures-src/provider/` mirroring task 4.7. The fixture:
- `init()` returns a `ProviderManifest` with `provider-id = "fixture"` and one model `"fixture-model-1"`.
- `complete(req)` calls `http::fetch(http-request{ method:"POST", url:"http://api.fixture.example/v1/messages", ... })`. (For unit tests we wrap with a mock HTTP, see step 6.6.)
- `complete` emits one ContentBlockStart and one MessageStop via `progress::emit-stream-event`, then returns a canned `CompleteResponse`.

Build via `just build-fixtures`.

- [ ] **Step 6.6: Provider adapter integration tests — `tests/provider_adapter.rs`.**

```rust
// Tests should cover:
//
// 1. provider_list_models_returns_fixture_model
// 2. provider_complete_calls_emitter_with_expected_events
// 3. provider_complete_denies_unlisted_host
// 4. provider_complete_denies_unlisted_keyring_account
//
// Use a mock StreamEmitter that records events into a Vec.
// For HTTP isolation, point the fixture at a local httpmock server; the
// fixture's manifest [security] allowed-hosts = ["127.0.0.1"] so the
// host filter passes.
```

- [ ] **Step 6.7: Commit.**

```bash
git add crates/savvagent-plugin-wasm/
git commit -m "feat(plugin-wasm): provider-world adapter + http/keyring/progress + fixture"
```

---

## Task 7: Fault-injection fixtures + tests

**Files:**
- Create: `crates/savvagent-plugin-wasm/tests/fixtures-src/{trap,timeout,denied-host,denied-account,bad-export}/`
- Create: `crates/savvagent-plugin-wasm/tests/fixtures/{trap,timeout,denied-host,denied-account,bad-export}.wasm`
- Create: `crates/savvagent-plugin-wasm/tests/fault_injection.rs`

- [ ] **Step 7.1: Author the five fault fixtures.**

Each is a tiny static-world plugin that violates one invariant:

- `trap.wasm`: `handle_slash` calls `core::arch::wasm32::unreachable()` or similar.
- `timeout.wasm`: `handle_slash` enters `loop {}`.
- `denied-host.wasm`: provider that calls `http::fetch("https://evil.example/x")` (not in allowed-hosts).
- `denied-account.wasm`: provider that calls `keyring::get("not-listed")`.
- `bad-export.wasm`: static plugin whose manifest declares `themes = true` but the wasm doesn't actually export `themes`.

Build via `just build-fixtures`.

- [ ] **Step 7.2: Tests — `tests/fault_injection.rs`.**

```rust
use std::sync::Arc;
use savvagent_plugin::Plugin;

#[tokio::test]
async fn trap_surfaces_as_plugin_error() {
    let (mut adapter, _td) = load_static("trap").await;
    let err = adapter.handle_slash("trap-me", vec![]).await.unwrap_err();
    assert!(err.to_string().contains("trap"));
}

#[tokio::test]
async fn timeout_cancels_via_epoch_interruption() {
    let (mut adapter, _td) = load_static("timeout").await;
    let start = std::time::Instant::now();
    let err = adapter.handle_slash("forever", vec![]).await.unwrap_err();
    let elapsed = start.elapsed();
    assert!(elapsed.as_secs() < 10,
            "should have cancelled around 5s but ran {elapsed:?}");
    assert!(err.to_string().contains("timeout") || err.to_string().contains("epoch"));
}

#[tokio::test]
async fn denied_host_returns_capability_denied() {
    let (provider_client, _td) = load_provider("denied-host").await;
    // Build a minimal CompleteRequest and a recording emitter.
    let res = provider_client
        .complete(canned_complete_request(), Box::new(RecordingEmitter::default()))
        .await;
    let err = res.unwrap_err();
    assert!(format!("{err}").contains("denied-host") ||
            format!("{err}").contains("evil.example"));
}

#[tokio::test]
async fn denied_account_returns_capability_denied() {
    let (provider_client, _td) = load_provider("denied-account").await;
    let res = provider_client
        .complete(canned_complete_request(), Box::new(RecordingEmitter::default()))
        .await;
    let err = res.unwrap_err();
    assert!(format!("{err}").contains("Denied") ||
            format!("{err}").contains("not-listed"));
}

#[tokio::test]
async fn bad_export_rejected_at_load_time() {
    let tmp = tempfile::tempdir().unwrap();
    write_manifest_for(&tmp, "fixture.bad-export", "plugin-static");
    std::fs::copy("tests/fixtures/bad-export.wasm",
                  tmp.path().join("plugin.wasm")).unwrap();

    let dm = Arc::new(crate::test_helpers::load_manifest(&tmp));
    let theme = savvagent_plugin_wasm::host_imports::theme::provider(vec![]);
    let err = savvagent_plugin_wasm::adapter::StaticAdapter::new(dm, tmp.path(), theme)
        .await
        .unwrap_err();
    assert!(matches!(err, savvagent_plugin_wasm::error::WasmPluginError::ExportMismatch(..) |
                          savvagent_plugin_wasm::error::WasmPluginError::Wasmtime(_)),
            "expected export mismatch, got {err:?}");
}
```

Add helper fns `load_static`, `load_provider`, `canned_complete_request`,
`RecordingEmitter` in `tests/test_helpers.rs` (sibling). Keep the helpers
≤ 80 LoC.

- [ ] **Step 7.3: Commit.**

```bash
git add crates/savvagent-plugin-wasm/
git commit -m "test(plugin-wasm): fault-injection fixtures (trap, timeout, denied caps, bad-export)"
```

---

## Task 8: Three-strikes-disable + trap recovery + tests

**Files:**
- Create: `crates/savvagent-plugin-wasm/src/strikes.rs`
- Modify: `crates/savvagent-plugin-wasm/src/adapter/static_.rs` (count traps, ask strikes)
- Modify: `crates/savvagent-plugin-wasm/src/adapter/interactive.rs`
- Modify: `crates/savvagent-plugin-wasm/src/adapter/provider.rs`
- Create: `crates/savvagent-plugin-wasm/tests/strikes.rs`

- [ ] **Step 8.1: Strike counter — `src/strikes.rs`.**

```rust
//! Rolling 10-minute trap counter. Three strikes = disable.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(600);
const LIMIT: usize = 3;

#[derive(Default)]
pub struct StrikeCounter {
    inner: Mutex<VecDeque<Instant>>,
}

impl StrikeCounter {
    pub fn record(&self) -> StrikeOutcome {
        let mut q = self.inner.lock().expect("strike mutex");
        let now = Instant::now();
        // Drop expired entries.
        while q.front().map_or(false, |t| now.duration_since(*t) > WINDOW) {
            q.pop_front();
        }
        q.push_back(now);
        if q.len() >= LIMIT {
            StrikeOutcome::Disable
        } else {
            StrikeOutcome::Continue { count: q.len(), window: WINDOW }
        }
    }

    pub fn reset(&self) {
        self.inner.lock().expect("strike mutex").clear();
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StrikeOutcome {
    Continue { count: usize, window: Duration },
    Disable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_strikes_disable() {
        let c = StrikeCounter::default();
        assert!(matches!(c.record(), StrikeOutcome::Continue { count: 1, .. }));
        assert!(matches!(c.record(), StrikeOutcome::Continue { count: 2, .. }));
        assert_eq!(c.record(), StrikeOutcome::Disable);
    }

    #[test]
    fn reset_clears() {
        let c = StrikeCounter::default();
        c.record();
        c.reset();
        assert!(matches!(c.record(), StrikeOutcome::Continue { count: 1, .. }));
    }
}
```

- [ ] **Step 8.2: Wire strikes into each adapter.**

In `StaticAdapter::handle_slash` and `on_event` (and the equivalents in interactive/provider), wrap the wasm call. On trap (any `WasmPluginError::Wasmtime`), call `self.strikes.record()`. If `StrikeOutcome::Disable`, return a special `PluginError::other("disabled-by-strikes")` and on the next call short-circuit. Otherwise call `self.rebuild_instance().await` and continue.

The disable signal needs to bubble up to the registry so the `internal:plugins` plugin can persist `disabled-reason = "repeated-traps"` to the trust file. Implementation:

```rust
// adapter/static_.rs (excerpt)
pub struct StaticAdapter {
    // ... fields as before ...
    strikes: Arc<crate::strikes::StrikeCounter>,
    disabled_tx: tokio::sync::mpsc::UnboundedSender<String>,  // sends plugin_id when disabled
}
```

The `disabled_tx` is constructed in `register_external` (Task 9) and drained by a background task that updates the trust file.

- [ ] **Step 8.3: Integration test — `tests/strikes.rs`.**

```rust
#[tokio::test]
async fn three_traps_in_window_disable_plugin() {
    let (mut adapter, _td) = load_static("trap").await;
    for _ in 0..2 {
        let _ = adapter.handle_slash("trap-me", vec![]).await;
    }
    let last = adapter.handle_slash("trap-me", vec![]).await;
    assert!(last.is_err());
    // The next call must short-circuit without invoking wasm.
    let after = adapter.handle_slash("trap-me", vec![]).await;
    assert!(after.unwrap_err().to_string().contains("disabled-by-strikes"));
}
```

- [ ] **Step 8.4: Commit.**

```bash
git add crates/savvagent-plugin-wasm/
git commit -m "feat(plugin-wasm): three-strikes-disable + trap recovery"
```

---

## Task 9: Wire `register_external` into `register_all` in TUI

**Files:**
- Create: `crates/savvagent-plugin-wasm/src/register.rs`
- Modify: `crates/savvagent/src/plugin/registry.rs`
- Modify: `crates/savvagent/src/plugin/mod.rs`
- Modify: `crates/savvagent/Cargo.toml` (depend on savvagent-plugin-wasm)
- Modify: `crates/savvagent/src/main.rs`
- Create: `crates/savvagent/tests/external_plugins.rs`

- [ ] **Step 9.1: `register_external` — `crates/savvagent-plugin-wasm/src/register.rs`.**

```rust
//! Discovery → validation → instantiation → adapter wrapping.
//! Produces `Vec<Box<dyn Plugin>>` + a list of provider clients
//! (consumed by the TUI's PROVIDERS extender).

use std::path::PathBuf;
use std::sync::Arc;
use savvagent_plugin::Plugin;
use savvagent_mcp::ProviderClient;

use crate::adapter::{StaticAdapter, InteractiveAdapter};
use crate::adapter::provider::WasmProviderClient;
use crate::discovery::{discover, SourceScope};
use crate::error::WasmPluginError;
use crate::host_imports::theme::ThemeProvider;
use crate::manifest::PluginWorld;
use crate::trust::{TrustFile, TrustCheck, tree_hash};

pub struct RegisterResult {
    pub plugins: Vec<Box<dyn Plugin>>,
    pub provider_clients: Vec<(String, Arc<WasmProviderClient>)>,  // (id, client)
    pub warnings: Vec<String>,
}

pub async fn register_external(
    project_root: Option<&std::path::Path>,
    home_dir: &std::path::Path,
    theme: ThemeProvider,
) -> Result<RegisterResult, WasmPluginError> {
    let discovery = discover(project_root, Some(home_dir));
    let mut trust = TrustFile::load(home_dir)?;
    let mut plugins: Vec<Box<dyn Plugin>> = Vec::new();
    let mut provider_clients = Vec::new();
    let mut warnings = discovery.warnings;

    for dp in discovery.plugins {
        let hash = tree_hash(&dp.dir)?;
        match trust.check(&dp.manifest.plugin.id, &hash) {
            TrustCheck::Ok => {}
            TrustCheck::Untrusted => {
                warnings.push(format!(
                    "[plugins] {} is untrusted; run /plugins trust {0}",
                    dp.manifest.plugin.id
                ));
                continue;
            }
            TrustCheck::HashMismatch { stored, actual } => {
                trust.revoke(&dp.manifest.plugin.id);
                warnings.push(format!(
                    "[plugins] {} hash mismatch (stored={stored} actual={actual}); trust revoked",
                    dp.manifest.plugin.id
                ));
                continue;
            }
            TrustCheck::Disabled(reason) => {
                warnings.push(format!(
                    "[plugins] {} disabled: {reason}",
                    dp.manifest.plugin.id
                ));
                continue;
            }
        }

        let dm = Arc::new(dp.manifest.clone());
        match dp.manifest.plugin.world {
            PluginWorld::PluginStatic => {
                let adapter = StaticAdapter::new(dm, &dp.dir, theme.clone()).await?;
                plugins.push(Box::new(adapter));
            }
            PluginWorld::PluginInteractive => {
                let adapter = InteractiveAdapter::new(dm, &dp.dir, theme.clone()).await?;
                plugins.push(Box::new(adapter));
            }
            PluginWorld::PluginProvider => {
                let provider_id = dp.manifest.exports.provider_id
                    .clone().expect("validated earlier");
                let client = WasmProviderClient::new(dm, &dp.dir).await?;
                provider_clients.push((provider_id, Arc::new(client)));
            }
        }
    }

    trust.save(home_dir).ok();
    Ok(RegisterResult { plugins, provider_clients, warnings })
}
```

- [ ] **Step 9.2: Plug into `register_all` — modify `crates/savvagent/src/plugin/registry.rs`.**

Find the `register_all` (or equivalent) entry-point — the spot where `register_builtins(&mut reg)` is called. Add an async equivalent:

```rust
// crates/savvagent/src/plugin/registry.rs (top of file)
use savvagent_plugin_wasm::register::register_external as wasm_register_external;

// new fn alongside register_all
pub async fn register_all_with_external(
    project_root: Option<&std::path::Path>,
    home_dir: &std::path::Path,
    theme_provider: savvagent_plugin_wasm::host_imports::theme::ThemeProvider,
) -> Result<PluginRegistry, RegistryError> {
    let builtins = crate::plugin::register_builtins();
    let mut set = builtins;
    let external = wasm_register_external(project_root, home_dir, theme_provider).await
        .unwrap_or_else(|e| {
            tracing::warn!("external-plugin registration failed: {e}");
            savvagent_plugin_wasm::register::RegisterResult {
                plugins: vec![], provider_clients: vec![], warnings: vec![],
            }
        });
    for warn in external.warnings { tracing::warn!("{warn}"); }
    set.plugins.extend(external.plugins);
    // Provider clients land in a separate channel for the TUI's
    // `effective_providers()` extender — see step 9.3.
    let registry = PluginRegistry::new(set);
    Ok(registry)
}
```

(Adapt the exact name and signature to the current `register_all` shape; the goal is one async entry-point that the TUI bootstrap calls.)

- [ ] **Step 9.3: Extend `PROVIDERS` with discovered wasm providers.**

Add a runtime extender. Edit `crates/savvagent/src/providers.rs`:

```rust
use std::sync::OnceLock;
static EXTERNAL_PROVIDERS: OnceLock<Vec<ProviderSpec>> = OnceLock::new();

pub fn install_external_providers(specs: Vec<ProviderSpec>) {
    let _ = EXTERNAL_PROVIDERS.set(specs);
}

pub fn effective_providers() -> Vec<&'static ProviderSpec> {
    let mut v: Vec<&ProviderSpec> = PROVIDERS.iter().collect();
    if let Some(ext) = EXTERNAL_PROVIDERS.get() {
        for s in ext { v.push(s); }
    }
    v
}
```

Replace every `PROVIDERS.iter()` callsite in the TUI with `effective_providers().into_iter()`. Use `grep -RIn "PROVIDERS\.iter\|PROVIDERS\[" crates/savvagent` to find them.

- [ ] **Step 9.4: Call from main.rs.**

In `crates/savvagent/src/main.rs`, replace the `register_all` call with the async one. Provide `project_root` via the existing project-root discovery (search for `SAVVAGENT.md` lookup; the same code answers this question). `home_dir` is `dirs::home_dir()`.

- [ ] **Step 9.5: Integration test — `crates/savvagent/tests/external_plugins.rs`.**

```rust
//! End-to-end: with a temp HOME containing a trusted static plugin,
//! the registry reports the plugin's slash command.

use savvagent::plugin::registry::register_all_with_external;

#[tokio::test]
async fn registry_includes_trusted_static_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let plugin_dir = home.join(".savvagent/plugins/acme.demo");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.toml"), r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#).unwrap();
    std::fs::copy(
        "../../crates/savvagent-plugin-wasm/tests/fixtures/static.wasm",
        plugin_dir.join("plugin.wasm"),
    ).unwrap();

    // Pre-trust the plugin.
    let hash = savvagent_plugin_wasm::trust::tree_hash(&plugin_dir).unwrap();
    let mut tf = savvagent_plugin_wasm::trust::TrustFile::default();
    tf.trust("acme.demo", hash, None);
    tf.save(home).unwrap();

    let theme = savvagent_plugin_wasm::host_imports::theme::provider(vec![]);
    let registry = register_all_with_external(None, home, theme).await.unwrap();
    let id = savvagent_plugin::PluginId::new("acme.demo").unwrap();
    assert!(registry.get(&id).is_some(),
            "trusted plugin must appear in the registry");
}
```

- [ ] **Step 9.6: Commit.**

```bash
git add crates/savvagent-plugin-wasm/ crates/savvagent/
git commit -m "feat(savvagent): wire register_external into register_all"
```

---

## Task 10: `internal:plugins` built-in + plugin manager screen

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/plugins/mod.rs`
- Create: `crates/savvagent/src/plugin/builtin/plugins/screen.rs`
- Modify: `crates/savvagent/src/plugin/builtin/mod.rs` (register the new built-in)
- Create: `crates/savvagent/tests/plugins_manager.rs`

Follow the prior-art pattern in `crates/savvagent/src/plugin/builtin/connect/` — that built-in owns both a slash entrypoint and a screen.

- [ ] **Step 10.1: Manager-plugin shell — `plugins/mod.rs`.**

```rust
//! `internal:plugins` built-in. Owns `/plugins` and the manager screen.

use async_trait::async_trait;
use savvagent_plugin::{Contributions, Effect, HostEvent, Manifest, Plugin,
                       PluginError, PluginId, PluginKind, Screen, ScreenArgs};

mod screen;
pub use screen::PluginsManagerScreen;

pub struct PluginsBuiltin;

#[async_trait]
impl Plugin for PluginsBuiltin {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: PluginId::new("internal:plugins").unwrap(),
            name: "Plugins".into(),
            version: "0.18.0".into(),
            description: "External plugin manager".into(),
            kind: PluginKind::Core,
            contributions: Contributions {
                slash_commands: vec!["plugins".into()],
                screens: vec!["plugins.manager".into()],
                ..Default::default()
            },
        }
    }

    async fn handle_slash(&mut self, name: &str, args: Vec<String>)
        -> Result<Vec<Effect>, PluginError>
    {
        if name != "plugins" { return Ok(vec![]) }
        match args.first().map(String::as_str) {
            None | Some("list") => Ok(vec![Effect::OpenScreen {
                plugin_id: PluginId::new("internal:plugins").unwrap(),
                screen_id: "plugins.manager".into(),
                args: ScreenArgs::default(),
            }]),
            Some(other) => {
                // install/trust/revoke/remove/enable/disable land in task 11.
                Ok(vec![Effect::push_note(format!("/plugins {other}: not yet implemented"))])
            }
        }
    }

    fn create_screen(&self, id: &str, args: ScreenArgs)
        -> Result<Box<dyn Screen>, PluginError>
    {
        if id == "plugins.manager" {
            Ok(Box::new(PluginsManagerScreen::new(args)))
        } else {
            Err(PluginError::ScreenNotFound(id.into()))
        }
    }

    async fn on_event(&mut self, _event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        Ok(vec![])
    }
}
```

- [ ] **Step 10.2: Manager screen — `plugins/screen.rs`.**

Lists discovered plugins in three sections: trusted, untrusted, disabled. Uses the existing `MultiSelectList` widget from `crates/savvagent/src/plugin/widgets/multi_select_list.rs` to display rows. Each row shows id, world, version, source-scope. Key bindings (per existing manager-screen patterns):

- `Enter`: details modal for the highlighted plugin
- `t`: `/plugins trust <id>` — only for untrusted
- `r`: `/plugins revoke <id>`
- `R`: `/plugins remove <id>` (with confirm)
- `d`: `/plugins disable <id>`
- `e`: `/plugins enable <id>`

Implementation is ~200 LoC ratatui rendering + key dispatch. Follow `crates/savvagent/src/plugin/builtin/connect/screen.rs` line-for-line for shape.

- [ ] **Step 10.3: Register the built-in.**

In `crates/savvagent/src/plugin/builtin/mod.rs`, find where every other built-in is constructed (look for `Box::new(connect::ConnectBuiltin::new())` or similar). Add:

```rust
pub mod plugins;

// inside register_builtins() at construction time
set.plugins.push(Box::new(plugins::PluginsBuiltin));
```

- [ ] **Step 10.4: Test — `plugins_manager.rs`.**

Smoke test:

```rust
#[tokio::test]
async fn slash_plugins_opens_manager() {
    let mut p = savvagent::plugin::builtin::plugins::PluginsBuiltin;
    let effects = p.handle_slash("plugins", vec![]).await.unwrap();
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0],
        savvagent_plugin::Effect::OpenScreen { screen_id, .. }
            if screen_id == "plugins.manager"));
}
```

- [ ] **Step 10.5: Commit.**

```bash
git add crates/savvagent/
git commit -m "feat(savvagent): internal:plugins built-in + manager screen"
```

---

## Task 11: `/plugins` install / trust / revoke / remove / enable / disable

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/plugins/mod.rs`
- Create: `crates/savvagent/src/plugin/builtin/plugins/install.rs`
- Create: `crates/savvagent/src/plugin/builtin/plugins/trust_modal.rs`
- Create: `crates/savvagent/tests/plugins_install.rs`

- [ ] **Step 11.1: Install handler — `install.rs`.**

```rust
//! Fetch plugin.toml, validate, fetch wasm, hash, prompt user, persist.

use std::path::Path;
use reqwest::Client;
use savvagent_plugin::{Effect, PluginError};
use savvagent_plugin_wasm::manifest::PluginManifest;
use savvagent_plugin_wasm::trust::{TrustFile, tree_hash};

const MAX_TOML_BYTES: usize = 64 * 1024;
const MAX_WASM_BYTES: usize = 32 * 1024 * 1024;

pub async fn install(home_dir: &Path, toml_url: &str)
    -> Result<Vec<Effect>, PluginError>
{
    let client = Client::builder().use_rustls_tls().build()
        .map_err(|e| PluginError::other(format!("reqwest: {e}")))?;

    let toml_text = fetch_capped(&client, toml_url, MAX_TOML_BYTES).await?;
    if toml_text.as_bytes().len() > MAX_TOML_BYTES {
        return Err(PluginError::other("plugin.toml exceeds 64 KB"));
    }

    // Stage in temp dir.
    let staging = tempfile::tempdir()
        .map_err(|e| PluginError::other(format!("tempdir: {e}")))?;
    std::fs::write(staging.path().join("plugin.toml"), &toml_text)
        .map_err(|e| PluginError::other(format!("write: {e}")))?;

    // Read manifest from staging.
    let parsed_id = extract_id(&toml_text)?;
    let manifest = PluginManifest::load(
        &staging.path().join("plugin.toml"), &parsed_id,
    ).map_err(|e| PluginError::other(format!("{e}")))?;

    let wasm_url = manifest.plugin.wasm.as_deref()
        .ok_or_else(|| PluginError::other("plugin.toml missing wasm = URL"))?;
    let wasm_bytes = fetch_capped(&client, wasm_url, MAX_WASM_BYTES).await?;
    std::fs::write(staging.path().join("plugin.wasm"), wasm_bytes.as_bytes())
        .map_err(|e| PluginError::other(format!("write wasm: {e}")))?;

    let hash = tree_hash(staging.path())
        .map_err(|e| PluginError::other(format!("hash: {e}")))?;

    // Build the trust-prompt modal payload; emit OpenScreen → trust_modal.
    Ok(vec![Effect::OpenScreen {
        plugin_id: savvagent_plugin::PluginId::new("internal:plugins").unwrap(),
        screen_id: "plugins.trust-modal".into(),
        args: savvagent_plugin::ScreenArgs::with_json(serde_json::json!({
            "id": parsed_id,
            "source_url": toml_url,
            "hash": hash,
            "manifest": &manifest.plugin,
            "staging_dir": staging.path().to_string_lossy(),
        })),
    }])
    // The staging tempdir leaks if the user cancels; the trust_modal
    // screen cleans it up in its on_close handler.
}

async fn fetch_capped(client: &Client, url: &str, cap: usize) -> Result<String, PluginError> {
    let resp = client.get(url).send().await
        .map_err(|e| PluginError::other(format!("fetch {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(PluginError::other(format!("{url} returned {}", resp.status())));
    }
    let bytes = resp.bytes().await
        .map_err(|e| PluginError::other(format!("read body: {e}")))?;
    if bytes.len() > cap {
        return Err(PluginError::other(format!("{url} exceeds {cap}-byte cap")));
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|_| PluginError::other("not utf-8"))
}

fn extract_id(toml_text: &str) -> Result<String, PluginError> {
    let v: toml::Value = toml::from_str(toml_text)
        .map_err(|e| PluginError::other(format!("parse toml: {e}")))?;
    v.get("plugin")
        .and_then(|p| p.get("id"))
        .and_then(|i| i.as_str())
        .map(String::from)
        .ok_or_else(|| PluginError::other("[plugin] id missing"))
}
```

- [ ] **Step 11.2: Trust modal screen — `trust_modal.rs`.**

A screen that renders the manifest summary, hash, and source URL with two buttons (`Enter` confirm / `Esc` cancel). On confirm: move staging dir to `~/.savvagent/plugins/<id>/`, write trust record, emit `Effect::PushNote("plugin <id> installed")`. On cancel: delete staging dir. Mirror `crates/savvagent/src/plugin/builtin/connect/screen.rs` for shape; ~150 LoC.

- [ ] **Step 11.3: Wire subcommands — modify `plugins/mod.rs`.**

Replace the "not yet implemented" branch with:

```rust
Some("install") => match args.get(1) {
    Some(url) => install::install(&home_dir(), url).await,
    None => Ok(vec![Effect::push_note("usage: /plugins install <plugin.toml URL>")]),
},
Some("trust") => trust_cmd(&args, &home_dir()).await,
Some("revoke") => revoke_cmd(&args, &home_dir()).await,
Some("remove") => remove_cmd(&args, &home_dir()).await,
Some("enable") => enable_cmd(&args, &home_dir()).await,
Some("disable") => disable_cmd(&args, &home_dir()).await,
Some("list") => Ok(vec![Effect::OpenScreen { /* manager */ }]),
```

Each `*_cmd` is 5–15 LoC of `TrustFile::load → mutate → save → PushNote`. Show the user a confirmation note on every action.

- [ ] **Step 11.4: Test — `tests/plugins_install.rs`.**

Use `httpmock` to stand up a fake plugin.toml + plugin.wasm endpoint. Test that:
- successful `install` opens the trust-modal screen with the right payload
- toml > 64 KB is rejected
- wasm > 32 MB is rejected
- non-200 status is rejected

- [ ] **Step 11.5: Commit.**

```bash
git add crates/savvagent/
git commit -m "feat(savvagent): /plugins install/trust/revoke/remove/enable/disable"
```

---

## Task 12: Three example plugins + separate wasm CI job

**Files:**
- Create: `examples/plugin-hello-static/Cargo.toml`, `src/lib.rs`
- Create: `examples/plugin-hello-interactive/Cargo.toml`, `src/lib.rs`
- Create: `examples/plugin-hello-provider/Cargo.toml`, `src/lib.rs`
- Create: `examples/plugin-hello-static/plugin.toml`
- Create: `examples/plugin-hello-interactive/plugin.toml`
- Create: `examples/plugin-hello-provider/plugin.toml`
- Create: `.github/workflows/example-plugins.yml`
- Modify: `Cargo.toml` (workspace) — examples are NOT workspace members; built separately to avoid hitting the main `cargo test` with `wasm32-wasip2` target needs

- [ ] **Step 12.1: hello-static.**

A slash command `/hello`. `handle_slash("hello", _)` → `PushNote("Hello from WASM!")`. ~30 LoC + manifest.

- [ ] **Step 12.2: hello-interactive.**

A screen at id `hello.modal`. `on_key(Enter)` → emits one `Effect::PushNote`. `render` calls `draw_block(area, plain, Some("Hello"))` plus `draw_text(2, 1, "Hello, world!", white, reset, no-mods)`. `tips()` → "press Enter to greet". ~80 LoC.

- [ ] **Step 12.3: hello-provider.**

A trivial provider that echoes the input as output:
- `init()` → `provider-manifest` with id `"hello-echo"`, one model `"echo-1"`.
- `complete(req)` → reads the last user message's text, returns it as the response. Emits one `MessageStart`, one `ContentBlockDelta`, one `MessageStop`.
- No HTTP needed; the fixture demonstrates the wire without external calls.

Manifest:
```toml
[plugin]
id = "savvagent.hello-provider"
name = "Hello Provider"
version = "0.1.0"
world = "plugin-provider"
savvagent = "^0.18"

[exports]
provider-id = "hello-echo"
```

No `[security]` because no `http` calls.

- [ ] **Step 12.4: CI workflow — `.github/workflows/example-plugins.yml`.**

```yaml
name: Example plugins build

on:
  push: { paths: [ "examples/plugin-hello-*/**", "crates/savvagent-plugin-wit/**" ] }
  pull_request: { paths: [ "examples/plugin-hello-*/**" ] }

jobs:
  build:
    runs-on: ubuntu-latest
    continue-on-error: true  # supplemental; doesn't gate merge
    steps:
      - uses: actions/checkout@v4
      - name: Install rust + wasm32-wasip2
        run: |
          rustup target add wasm32-wasip2
          cargo install cargo-component --locked
      - name: Build each example
        run: |
          for dir in examples/plugin-hello-*; do
            (cd "$dir" && cargo component build --release)
          done
```

- [ ] **Step 12.5: Commit.**

```bash
git add examples/ .github/workflows/example-plugins.yml
git commit -m "feat: example external plugins (static/interactive/provider) + supplemental CI"
```

---

## Task 13: README + plugin-authoring docs + CHANGELOG

**Files:**
- Modify: `README.md` — add "Authoring external plugins" section
- Create: `docs/plugins/authoring.md`
- Modify: `CHANGELOG.md` — v0.18.0 entry

- [ ] **Step 13.1: README addition.**

Add a new top-level section "Authoring external plugins" after the existing "Extending" section. Include:
- Discovery paths (the four)
- plugin.toml schema (mirror Section 1 of the spec)
- Each world's required exports + a code snippet
- `/plugins install <plugin.toml URL>` and trust-prompt walkthrough
- Where to find example plugins (`examples/plugin-hello-*`)
- Link to `docs/plugins/authoring.md`

- [ ] **Step 13.2: `docs/plugins/authoring.md` — long-form guide.**

Sections:
- "Quickstart: ship a static plugin in 10 minutes" with full code
- "WIT contract reference" — link to the three .wit files
- "Capabilities by world" — table from spec §5
- "Trust + install" — flow walk-through
- "Limitations in v0.18.0" — link to spec §7 non-goals

- [ ] **Step 13.3: CHANGELOG.md v0.18.0 entry.**

Follow existing CHANGELOG format. Highlights:
- External plugins via WASM (three worlds)
- `/plugins install <toml-url>` + manager screen
- Two new crates: `savvagent-plugin-wit`, `savvagent-plugin-wasm`
- wasmtime 24.0 pinned
- Sub-projects A/B/C shipped in v0.17.0 (link to that section)

- [ ] **Step 13.4: Commit.**

```bash
git add README.md docs/plugins/ CHANGELOG.md
git commit -m "docs(0.18.0): plugin authoring guide + README section + CHANGELOG"
```

---

## Task 14: Version bump → 0.18.0

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.package]` + every `[workspace.dependencies]` literal)
- Modify: every crate's `Cargo.toml` that pins its own version (if any do — `version.workspace = true` should cover all crates)

- [ ] **Step 14.1: Bump workspace version.**

In `Cargo.toml`:

```toml
[workspace.package]
version = "0.18.0"
```

And update every local-crate path-dep version literal:

```toml
savvagent-plugin = { path = "crates/savvagent-plugin", version = "0.18.0" }
savvagent-plugin-wit = { path = "crates/savvagent-plugin-wit", version = "0.18.0" }
savvagent-plugin-wasm = { path = "crates/savvagent-plugin-wasm", version = "0.18.0" }
savvagent-protocol = { path = "crates/savvagent-protocol", version = "0.18.0" }
# ... and so on for every local crate
```

Per `feedback_semver.md`: mirror the bump in `[workspace.dependencies]` literals.

- [ ] **Step 14.2: Verify build.**

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Expected: clean.

- [ ] **Step 14.3: Commit.**

```bash
git add Cargo.toml
git commit -m "release(0.18.0): external plugins (sub-project D)"
```

---

## Final verification before opening PR

Run, **in order**:

```bash
# Confirm CI parity per feedback_match_ci_toolchain_locally.md
rustup run stable cargo fmt --all -- --check
rustup run stable cargo clippy --workspace --all-targets -- -D warnings
rustup run stable cargo test --workspace
```

Expected: green. If clippy raises something on `unsafe`-adjacent code in
`host_imports/draw.rs`, address it with a doc-comment and a
`#[allow(unsafe_code)]` only on the specific function — do not silence
crate-wide.

Open the PR with:
- Title: `feat: external plugins (sub-project D) — v0.18.0 rollup`
- Body: link the spec, summarize the 14 commits, paste the §6 risk register, and call out the wasm-toolchain-isolation point.
- Per `feedback_keep_issue_updated.md`: update any tracking issue (e.g., a v0.18.0 milestone or sub-project-D issue) with running comments as commits land.

Per `feedback_cargo_dist_release.md` and `feedback_phase_release_rollup.md`:
- Do NOT run `gh release create` after merge; cargo-dist's Release workflow on the v0.18.0 tag publishes binaries.
- The tag is the **next-version-after-last-real-tag** — v0.17.0 must be tagged from master first (A/B/C rollup), then v0.18.0 tag is pushed after this PR merges.

Per `feedback_verify_ci_after_push.md`: never claim "push is good" without
`gh run` confirming green for the merge SHA.

---

**Plan complete. The spec and plan together cover ~12 hours of focused implementation. Treat both this file and `2026-05-25-external-plugins.md` as one contiguous plan; execute tasks in numeric order.**
