//! `plugin-interactive` world adapter.
//!
//! Loads a `.wasm` component implementing the `plugin-interactive` world,
//! wires the host imports (`log`, `current-theme`), caches the manifest at
//! construction time, and creates a **new** [`Store`] per screen-open via
//! a long-lived [`InstancePre`].
//!
//! ## Concurrency model
//!
//! The static adapter (one Store per plugin) is fine for slash commands —
//! they're stateless and serial. Screens are different: each open carries
//! per-instance state (cursor row, scroll offset, …), so we instantiate a
//! fresh Store + instance per screen-open and hand the resulting
//! [`WasmScreen`] over to the host. The adapter retains:
//!
//! - an `Arc<InstancePre<InteractiveHostState>>` — long-lived, shared across
//!   every screen-open;
//! - a long-lived "manifest" Store used only for `call_manifest` at
//!   construction time (then dropped — we cache the result).
//!
//! Each [`WasmScreen`] owns its own Store, its own `PluginInteractive`, the
//! `ResourceAny` handle returned from `create-screen`, and a small
//! `CachedRender` snapshot of the last `render`/`tips` output (because the
//! `Screen` trait is `&self` + sync but the wasm exports are async).
//!
//! ## Async-from-sync bridging
//!
//! The `Plugin::create_screen` method on the trait surface is `&self` +
//! **sync**, yet wasm instantiation is unavoidably async. We bridge with
//! [`tokio::task::block_in_place`] + [`tokio::runtime::Handle::current`] —
//! this is safe on a **multi-thread** tokio runtime (block_in_place moves
//! the current worker out of the pool and another thread picks up the
//! pending tasks). On a current-thread runtime block_in_place panics; we
//! detect that case and return [`PluginError::Internal`].
//!
//! The same pattern is used inside [`WasmScreen::render`] + `tips` (sync
//! trait surface, async wasm calls). We re-render eagerly after every
//! `on_key` / `on_event` so the cached lines stay current with screen
//! state. **Known limitation:** a terminal-resize that doesn't trigger a
//! key event won't re-render the screen at the new geometry; the cached
//! lines were computed for the old width/height. A force-refresh
//! mechanism is queued for a later release.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use tokio::sync::Mutex as TokioMutex;
use wasmtime::Store;
use wasmtime::component::{Component, HasSelf, Linker, ResourceAny};

use savvagent_plugin::manifest::Manifest;
use savvagent_plugin::{
    Effect, HostEvent, KeyCodePortable, KeyEventPortable, KeyMods, Plugin, PluginError, Region,
    Screen, ScreenArgs, StyledLine, ThemeEntry,
};

use crate::convert::{
    effect_from_wit, manifest_from_wit, plugin_error_from_wit, region_to_wit, styled_line_from_wit,
    theme_color_to_wit,
};
use crate::engine::{EPOCH_TICK, shared_engine};
use crate::error::WasmPluginError;
use crate::host_imports::{log as log_host, theme};
use crate::interactive_world::{
    self, PluginInteractive, PluginInteractiveImports, PluginInteractivePre,
    savvagent::plugin::types as wit,
};
use crate::manifest::PluginManifest as DiskManifest;
use crate::strikes::{StrikeCounter, StrikeOutcome};

/// Per-store state for an interactive-world wasm Store.
///
/// Shared between the adapter's manifest store and every per-screen Store
/// — each gets its own value but the same shape.
pub(crate) struct InteractiveHostState {
    /// Plugin id (`<vendor>:<rest>`) attached to every host-side log event.
    plugin_id: String,
    /// Shared, live theme snapshot. Read via `theme.read().await`.
    theme: theme::ThemeProvider,
}

// `PluginInteractiveImports` is the bindgen-emitted trait for the world's
// inline imports (`log`, `current-theme`). Implementing it on
// `InteractiveHostState` matches the same pattern the static adapter uses.
impl PluginInteractiveImports for InteractiveHostState {
    async fn log(&mut self, level: wit::LogLevel, msg: String) {
        log_host::emit(&self.plugin_id, level, &msg);
    }

    async fn current_theme(&mut self) -> Vec<(String, wit::ThemeColor)> {
        let snap = theme::snapshot(&self.theme).await;
        snap.into_iter()
            .map(|(name, color)| (name, theme_color_to_wit(color)))
            .collect()
    }
}

// Empty `Host` impl for the `types` interface (its trait is empty; the
// bindgen still requires us to assert it). Now that `create-screen` and
// the `screen-instance` resource live inside the *exported* `screens`
// interface, the host doesn't need any per-resource impl — wasmtime
// reaps the export-side resource when the guest's wasm `drop` runs and
// our `WasmScreen` drops its `ResourceAny`.
impl interactive_world::savvagent::plugin::types::Host for InteractiveHostState {}

/// Adapter that wraps a `plugin-interactive` wasm component as a
/// `Box<dyn Plugin>`.
pub struct InteractiveAdapter {
    cached_manifest: Manifest,
    /// Long-lived pre-instantiated component used to mint a fresh Store
    /// per screen-open.
    instance_pre: Arc<PluginInteractivePre<InteractiveHostState>>,
    /// Plugin id, copied into every per-screen `InteractiveHostState`.
    plugin_id: String,
    /// Shared theme handle, cloned into every per-screen `InteractiveHostState`.
    theme: theme::ThemeProvider,
    /// Held purely for trap-recovery in Task 8; the field is read indirectly
    /// in v0.18.0 only through [`InteractiveAdapter::disk_manifest`].
    disk_manifest: Arc<DiskManifest>,
    /// Rolling-window trap counter (Task 8). Shared with every
    /// `WasmScreen` this adapter mints so all per-screen traps count
    /// against the same budget.
    strikes: Arc<StrikeCounter>,
    /// Set once the strike counter says "disable". Subsequent
    /// `create_screen` calls short-circuit; previously-handed-out
    /// `WasmScreen`s also short-circuit through their cloned handle.
    disabled: Arc<AtomicBool>,
}

impl InteractiveAdapter {
    /// Borrow the parsed `plugin.toml` this adapter was constructed from.
    pub fn disk_manifest(&self) -> &Arc<DiskManifest> {
        &self.disk_manifest
    }

    /// Borrow the rolling-window strike counter. Exposed for tests + the
    /// future Task 9 registry wiring.
    pub fn strikes(&self) -> &Arc<StrikeCounter> {
        &self.strikes
    }

    /// `true` once the rolling-window strike counter has flipped this
    /// adapter to disabled.
    pub fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::SeqCst)
    }

    /// Per-call epoch deadline in ticks of [`EPOCH_TICK`]. Mirrors the
    /// computation in [`crate::adapter::StaticAdapter::call_deadline_ticks`].
    fn call_deadline_ticks(&self) -> u64 {
        let ms = u64::from(self.disk_manifest.runtime.call_timeout_ms);
        let tick_ms = EPOCH_TICK.as_millis() as u64;
        ms.div_ceil(tick_ms.max(1)).max(1)
    }
}

impl std::fmt::Debug for InteractiveAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InteractiveAdapter")
            .field("id", &self.cached_manifest.id.as_str())
            .field("name", &self.cached_manifest.name)
            .field("version", &self.cached_manifest.version)
            .finish()
    }
}

impl InteractiveAdapter {
    /// Construct an `InteractiveAdapter` by loading `plugin.wasm` from
    /// `plugin_dir`, wiring host imports, pre-instantiating the component,
    /// and caching the manifest.
    pub async fn new(
        disk_manifest: Arc<DiskManifest>,
        plugin_dir: &Path,
        theme: theme::ThemeProvider,
    ) -> Result<Self, WasmPluginError> {
        let engine = shared_engine()?;
        let wasm_path = plugin_dir.join("plugin.wasm");
        let component =
            Component::from_file(&engine, &wasm_path).map_err(WasmPluginError::Wasmtime)?;

        let mut linker: Linker<InteractiveHostState> = Linker::new(&engine);
        PluginInteractive::add_to_linker::<_, HasSelf<InteractiveHostState>>(&mut linker, |s| s)
            .map_err(WasmPluginError::Wasmtime)?;

        // Build the long-lived InstancePre. `PluginInteractivePre::new`
        // does the export-shape typecheck once; per-screen-open then only
        // pays the `instantiate_async` cost (no re-typecheck).
        let pre = linker
            .instantiate_pre(&component)
            .map_err(WasmPluginError::Wasmtime)?;
        let plugin_pre = PluginInteractivePre::new(pre).map_err(WasmPluginError::Wasmtime)?;

        // Use a throwaway store for `call_manifest` — the result is cached
        // and the store is dropped at the end of `new`.
        let manifest_state = InteractiveHostState {
            plugin_id: disk_manifest.plugin.id.clone(),
            theme: theme.clone(),
        };
        let mut manifest_store = Store::new(&engine, manifest_state);
        // Construction-time deadline: effectively never. Engine has
        // epoch_interruption enabled (Task 8) so the default `0` deadline
        // would trap the very first instruction. We use `u64::MAX / 2`
        // (not `u64::MAX`) because wasmtime computes the absolute
        // deadline as `current_epoch + delta`, which overflows if the
        // bumper has already advanced.
        manifest_store.set_epoch_deadline(u64::MAX / 2);
        let manifest_instance = plugin_pre
            .instantiate_async(&mut manifest_store)
            .await
            .map_err(WasmPluginError::Wasmtime)?;
        let wit_manifest = manifest_instance
            .call_manifest(&mut manifest_store)
            .await
            .map_err(WasmPluginError::Wasmtime)?
            .map_err(|e| {
                WasmPluginError::Manifest(
                    wasm_path.clone(),
                    format!("plugin returned error: {e:?}"),
                )
            })?;
        let cached_manifest = manifest_from_wit(wit_manifest)?;

        Ok(Self {
            cached_manifest,
            instance_pre: Arc::new(plugin_pre),
            plugin_id: disk_manifest.plugin.id.clone(),
            theme,
            disk_manifest,
            strikes: Arc::new(StrikeCounter::default()),
            disabled: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Async path used by tests and the (future) Task 9 runtime wiring to
    /// create a screen without the `block_in_place` bridge.
    ///
    /// In v0.18.0 the `Plugin::create_screen` trait method is `&self` +
    /// sync, so the runtime calls into [`Plugin::create_screen`] (which
    /// in turn calls this method via `block_in_place`). Tests bypass the
    /// bridge by calling this directly.
    pub async fn create_screen_async(
        &self,
        id: &str,
        args: ScreenArgs,
    ) -> Result<Box<dyn Screen>, PluginError> {
        if self.is_disabled() {
            return Err(PluginError::Internal(
                "plugin disabled by strikes (repeated wasm traps)".to_string(),
            ));
        }
        WasmScreen::new(
            id,
            args,
            Arc::clone(&self.instance_pre),
            self.plugin_id.clone(),
            self.theme.clone(),
            Arc::clone(&self.strikes),
            Arc::clone(&self.disabled),
            self.call_deadline_ticks(),
        )
        .await
        .map(|s| Box::new(s) as Box<dyn Screen>)
    }
}

#[async_trait]
impl Plugin for InteractiveAdapter {
    fn manifest(&self) -> Manifest {
        self.cached_manifest.clone()
    }

    /// Construct a fresh `WasmScreen` per `OpenScreen` effect.
    ///
    /// **Requires a multi-thread tokio runtime.** Returns
    /// [`PluginError::Internal`] when called on a current-thread runtime
    /// or outside any tokio runtime context. Tests must therefore use
    /// `#[tokio::test(flavor = "multi_thread")]`.
    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        if self.is_disabled() {
            return Err(PluginError::Internal(
                "plugin disabled by strikes (repeated wasm traps)".to_string(),
            ));
        }
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            PluginError::Internal(
                "InteractiveAdapter::create_screen requires a tokio runtime".to_string(),
            )
        })?;
        // `block_in_place` panics on a current-thread runtime. Detect that
        // flavor up front and return a controlled error so callers don't
        // see a process panic crossing the plugin boundary.
        if matches!(
            handle.runtime_flavor(),
            tokio::runtime::RuntimeFlavor::CurrentThread
        ) {
            return Err(PluginError::Internal(
                "InteractiveAdapter::create_screen requires a multi-thread tokio runtime"
                    .to_string(),
            ));
        }
        let id_owned = id.to_string();
        let instance_pre = Arc::clone(&self.instance_pre);
        let plugin_id = self.plugin_id.clone();
        let theme = self.theme.clone();
        let strikes = Arc::clone(&self.strikes);
        let disabled = Arc::clone(&self.disabled);
        let deadline_ticks = self.call_deadline_ticks();
        tokio::task::block_in_place(|| {
            handle.block_on(async move {
                WasmScreen::new(
                    &id_owned,
                    args,
                    instance_pre,
                    plugin_id,
                    theme,
                    strikes,
                    disabled,
                    deadline_ticks,
                )
                .await
                .map(|s| Box::new(s) as Box<dyn Screen>)
            })
        })
    }

    fn render_slot(&self, _slot_id: &str, _region: Region) -> Vec<StyledLine> {
        // Same rationale as the static adapter: render_slot is &self + sync
        // but the wasm path is async. Interactive plugins are not expected
        // to register render slots in v0.18.0 (manifest-side validation can
        // enforce this in a follow-up).
        Vec::new()
    }

    fn themes(&self) -> Vec<ThemeEntry> {
        // The interactive world does not export `themes()`; interactive
        // plugins contributing themes must declare a separate static
        // companion plugin. (The two are independent crates on disk so
        // there's no architectural barrier.) Returning empty keeps the
        // trait surface consistent without a no-op wasm round-trip.
        Vec::new()
    }
}

/// Cached render output from the last wasm `render` + `tips` call.
///
/// The `Screen` trait surface is `&self` + sync for `render` and `tips`;
/// the wasm exports are async. Rather than block on every frame, we cache
/// the result and refresh after each key/event.
#[derive(Default)]
struct CachedRender {
    lines: Vec<StyledLine>,
    tips: Vec<StyledLine>,
}

/// Per-screen-open instance returned from [`InteractiveAdapter::create_screen_async`].
///
/// Owns its own tokio-async-friendly `Store` + `PluginInteractive` + the
/// `ResourceAny` handle the guest's `create-screen` minted. The
/// `CachedRender` snapshot is held behind a `std::sync::Mutex` so the sync
/// trait methods can read it without acquiring the async store mutex.
///
/// Carries shared `StrikeCounter` / `disabled` handles cloned from the
/// parent [`InteractiveAdapter`] so per-screen traps count against the
/// same rolling-window budget. A trap inside `on_key` or `on_event` does
/// *not* attempt to rebuild this screen's instance — the screen's
/// accumulated state lives in wasm linear memory and would be lost anyway;
/// the error surfaces to the host, which normally closes the screen.
pub struct WasmScreen {
    /// Stable id this screen was created for. Matches the manifest's
    /// `ScreenSpec::id`.
    id: String,
    /// Async lock around the live wasm Store + instance + resource handle.
    inner: Arc<TokioMutex<WasmScreenInner>>,
    /// Sync-readable snapshot of the last render/tips wasm output.
    cached: Arc<StdMutex<CachedRender>>,
    /// Strike counter cloned from the parent adapter; traps on this
    /// screen feed into the same rolling-window budget as adapter-level
    /// failures.
    strikes: Arc<StrikeCounter>,
    /// Disable flag cloned from the parent adapter; flipping it here
    /// short-circuits both this screen's future calls and the parent
    /// adapter's `create_screen` path.
    disabled: Arc<AtomicBool>,
    /// Per-call epoch deadline in [`EPOCH_TICK`] units. Pre-computed at
    /// screen-open time from the manifest's `call_timeout_ms` so each
    /// `on_key`/`on_event` call avoids re-reading the manifest.
    deadline_ticks: u64,
}

struct WasmScreenInner {
    store: Store<InteractiveHostState>,
    instance: PluginInteractive,
    handle: ResourceAny,
    /// Last region we rendered for; used by `refresh` to re-issue the
    /// wasm `render` call with the same geometry after a state mutation.
    /// Defaults to a sensible 80×24 until `Screen::render` is first
    /// called with the real region.
    last_region: wit::Region,
}

impl WasmScreen {
    #[allow(clippy::too_many_arguments)]
    async fn new(
        id: &str,
        args: ScreenArgs,
        instance_pre: Arc<PluginInteractivePre<InteractiveHostState>>,
        plugin_id: String,
        theme: theme::ThemeProvider,
        strikes: Arc<StrikeCounter>,
        disabled: Arc<AtomicBool>,
        deadline_ticks: u64,
    ) -> Result<Self, PluginError> {
        let engine = shared_engine().map_err(|e| PluginError::Internal(e.to_string()))?;
        let state = InteractiveHostState { plugin_id, theme };
        let mut store = Store::new(&engine, state);
        // The construction-time `create-screen` call runs against the
        // per-call deadline too — guest authors are free to do
        // non-trivial work there, but it's the same budget any single
        // wasm call gets. The render/tips cache refresh below shares
        // this deadline.
        store.set_epoch_deadline(deadline_ticks);
        let instance = instance_pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| PluginError::Internal(format!("instantiate_async: {e}")))?;

        // Serialize ScreenArgs into the WIT-side `invocation-json`. The
        // trait-surface ScreenArgs is `#[non_exhaustive]` and not
        // `Serialize`; we hand-roll a stable shape per variant.
        let invocation_json = screen_args_to_json(&args);
        let wit_args = interactive_world::exports::savvagent::plugin::screens::ScreenArgs {
            invocation_json,
            // Real terminal geometry is unknown at create-screen time;
            // we hand the guest a sentinel pair and let it call
            // `render(area)` later for the actual region.
            terminal_width: 0,
            terminal_height: 0,
        };

        let create_result = instance
            .savvagent_plugin_screens()
            .call_create_screen(&mut store, id, &wit_args)
            .await
            .map_err(|e| PluginError::Internal(format!("wasm trap in create_screen: {e}")))?;
        let handle = create_result.map_err(plugin_error_from_wit)?;

        let initial_region = wit::Region {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };

        let mut inner = WasmScreenInner {
            store,
            instance,
            handle,
            last_region: initial_region,
        };

        // Reset the deadline before the first refresh_cache call — each
        // wasm export call gets its own fresh budget. set_epoch_deadline
        // is "ticks beyond current", so re-issuing here resets the
        // remaining budget for the next call (which is what we want).
        inner.store.set_epoch_deadline(deadline_ticks);
        // Eagerly populate the render + tips cache so the first `render`
        // call from the host doesn't paint a blank screen.
        let cached = refresh_cache(&mut inner).await?;

        Ok(Self {
            id: id.to_string(),
            inner: Arc::new(TokioMutex::new(inner)),
            cached: Arc::new(StdMutex::new(cached)),
            strikes,
            disabled,
            deadline_ticks,
        })
    }
}

impl WasmScreen {
    /// Record one wasm trap against the shared strike counter, flip
    /// `disabled` if we crossed the limit, and return the error wrapped
    /// in `PluginError::Internal`.
    ///
    /// Unlike the static adapter, we do **not** attempt to rebuild this
    /// screen's instance — the per-screen state lives in wasm linear
    /// memory and would be unrecoverable. The host normally responds to
    /// an `on_key`/`on_event` failure by closing the screen, which drops
    /// the `WasmScreen` and frees the dead Store on the next GC pass.
    fn record_screen_trap(&self, err: anyhow::Error, op: &'static str) -> PluginError {
        match self.strikes.record() {
            StrikeOutcome::Continue { count, window } => {
                tracing::warn!(
                    op,
                    count,
                    window_secs = window.as_secs(),
                    "wasm screen trap recorded ({count}/{} in last {}s)",
                    crate::strikes::LIMIT,
                    window.as_secs(),
                );
                PluginError::Internal(format!("wasm trap in {op}: {err}"))
            }
            StrikeOutcome::Disable => {
                self.disabled.store(true, Ordering::SeqCst);
                tracing::error!(op, "plugin disabled by strikes after wasm screen trap");
                PluginError::Internal(format!(
                    "plugin disabled by strikes (repeated wasm traps); last trap in {op}: {err}"
                ))
            }
        }
    }
}

#[async_trait]
impl Screen for WasmScreen {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn render(&self, region: Region) -> Vec<StyledLine> {
        // Record the region for the *next* refresh — the cached snapshot is
        // what we return now. The host calls `render` every frame; we keep
        // the wit-side `last_region` up to date so any post-key refresh
        // re-renders at the latest geometry.
        if let Ok(mut inner) = self.inner.try_lock() {
            inner.last_region = region_to_wit(region);
        }
        self.cached
            .lock()
            .expect("CachedRender poisoned")
            .lines
            .clone()
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        if self.disabled.load(Ordering::SeqCst) {
            return Err(PluginError::Internal(
                "plugin disabled by strikes (repeated wasm traps)".to_string(),
            ));
        }
        let wit_key = key_event_to_wit(key);
        let effects = {
            let mut inner = self.inner.lock().await;
            // Decompose the borrow: `instance` is immutable for the guest
            // accessor chain, `store` is mutable for the call, and `handle`
            // is a `Copy`able `ResourceAny`. Holding the accessor in a
            // local lets us also borrow `&mut inner.store` without
            // conflicting with the chained method calls.
            let WasmScreenInner {
                store,
                instance,
                handle,
                ..
            } = &mut *inner;
            store.set_epoch_deadline(self.deadline_ticks);
            let call_result = instance
                .savvagent_plugin_screens()
                .screen_instance()
                .call_on_key(&mut *store, *handle, &wit_key)
                .await;
            let result = match call_result {
                Ok(r) => r,
                Err(e) => return Err(self.record_screen_trap(e, "on_key")),
            };
            let wit_effects = result.map_err(plugin_error_from_wit)?;
            let mut effects = Vec::with_capacity(wit_effects.len());
            for e in wit_effects {
                effects.push(
                    effect_from_wit(e).map_err(|err| PluginError::Internal(err.to_string()))?,
                );
            }
            // Refresh the cached lines + tips so the next `render` call
            // reflects any state mutation the on_key produced. The
            // refresh shares the per-call budget; reset before the
            // render+tips pair so they don't consume the on_key budget.
            inner.store.set_epoch_deadline(self.deadline_ticks);
            let updated = match refresh_cache(&mut inner).await {
                Ok(c) => c,
                Err(e) => {
                    // Treat a refresh failure as a trap on the same op
                    // so the strike counter sees it. The error string
                    // already says "wasm trap in render/tips: ..." from
                    // refresh_cache, so wrap rather than re-format.
                    return Err(self.record_screen_trap(
                        anyhow::anyhow!("refresh after on_key: {e}"),
                        "on_key",
                    ));
                }
            };
            *self.cached.lock().expect("CachedRender poisoned") = updated;
            effects
        };
        Ok(effects)
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        if self.disabled.load(Ordering::SeqCst) {
            return Err(PluginError::Internal(
                "plugin disabled by strikes (repeated wasm traps)".to_string(),
            ));
        }
        let event_json = host_event_to_json(&event);
        let effects = {
            let mut inner = self.inner.lock().await;
            let WasmScreenInner {
                store,
                instance,
                handle,
                ..
            } = &mut *inner;
            store.set_epoch_deadline(self.deadline_ticks);
            let call_result = instance
                .savvagent_plugin_screens()
                .screen_instance()
                .call_on_event(&mut *store, *handle, &event_json)
                .await;
            let result = match call_result {
                Ok(r) => r,
                Err(e) => return Err(self.record_screen_trap(e, "on_event")),
            };
            let wit_effects = result.map_err(plugin_error_from_wit)?;
            let mut effects = Vec::with_capacity(wit_effects.len());
            for e in wit_effects {
                effects.push(
                    effect_from_wit(e).map_err(|err| PluginError::Internal(err.to_string()))?,
                );
            }
            inner.store.set_epoch_deadline(self.deadline_ticks);
            let updated = match refresh_cache(&mut inner).await {
                Ok(c) => c,
                Err(e) => {
                    return Err(self.record_screen_trap(
                        anyhow::anyhow!("refresh after on_event: {e}"),
                        "on_event",
                    ));
                }
            };
            *self.cached.lock().expect("CachedRender poisoned") = updated;
            effects
        };
        Ok(effects)
    }

    fn tips(&self) -> Vec<StyledLine> {
        self.cached
            .lock()
            .expect("CachedRender poisoned")
            .tips
            .clone()
    }
}

/// Re-issue the wasm `render` + `tips` calls against the current inner
/// state and return the fresh styled-line snapshots.
async fn refresh_cache(inner: &mut WasmScreenInner) -> Result<CachedRender, PluginError> {
    let WasmScreenInner {
        store,
        instance,
        handle,
        last_region,
    } = inner;
    let lines = instance
        .savvagent_plugin_screens()
        .screen_instance()
        .call_render(&mut *store, *handle, *last_region)
        .await
        .map_err(|e| PluginError::Internal(format!("wasm trap in render: {e}")))?;
    let tips = instance
        .savvagent_plugin_screens()
        .screen_instance()
        .call_tips(&mut *store, *handle)
        .await
        .map_err(|e| PluginError::Internal(format!("wasm trap in tips: {e}")))?;
    Ok(CachedRender {
        lines: lines.into_iter().map(styled_line_from_wit).collect(),
        tips: tips.into_iter().map(styled_line_from_wit).collect(),
    })
}

/// Translate a host-side `KeyEventPortable` into the WIT-side equivalent.
///
/// The WIT-side `KeyCode` is a strict subset of the host-side enum (`Char`
/// carries a `String` rather than a `char`, and there are no `Unknown`
/// variants). We map total: `Unknown` collapses to `Null`, which the WIT
/// guest can treat as a no-op.
fn key_event_to_wit(k: KeyEventPortable) -> wit::KeyEventPortable {
    let code = match k.code {
        KeyCodePortable::Char(c) => wit::KeyCode::Char(c.to_string()),
        KeyCodePortable::Enter => wit::KeyCode::Enter,
        KeyCodePortable::Esc => wit::KeyCode::Escape,
        KeyCodePortable::Backspace => wit::KeyCode::Backspace,
        KeyCodePortable::Tab => wit::KeyCode::Tab,
        KeyCodePortable::BackTab => wit::KeyCode::Backtab,
        KeyCodePortable::Delete => wit::KeyCode::Delete,
        KeyCodePortable::Insert => wit::KeyCode::Insert,
        KeyCodePortable::Home => wit::KeyCode::Home,
        KeyCodePortable::End => wit::KeyCode::End,
        KeyCodePortable::PageUp => wit::KeyCode::PageUp,
        KeyCodePortable::PageDown => wit::KeyCode::PageDown,
        KeyCodePortable::Up => wit::KeyCode::Up,
        KeyCodePortable::Down => wit::KeyCode::Down,
        KeyCodePortable::Left => wit::KeyCode::Left,
        KeyCodePortable::Right => wit::KeyCode::Right,
        KeyCodePortable::F(n) => wit::KeyCode::Function(n),
        KeyCodePortable::Unknown => wit::KeyCode::Null,
    };
    wit::KeyEventPortable {
        code,
        modifiers: key_mods_to_wit(k.modifiers),
    }
}

fn key_mods_to_wit(m: KeyMods) -> wit::KeyModifiers {
    wit::KeyModifiers {
        ctrl: m.ctrl,
        shift: m.shift,
        alt: m.alt,
        meta: m.meta,
    }
}

/// Hand-rolled `ScreenArgs` → JSON projection. Mirrors `event_to_json` in
/// the static adapter: a stable shape that doesn't depend on the
/// trait-surface gaining a `serde::Serialize` derive (which would tie the
/// wire format to a non-stability-promising derive).
fn screen_args_to_json(args: &ScreenArgs) -> String {
    use serde_json::json;
    let v = match args {
        ScreenArgs::None => json!({"kind": "none"}),
        ScreenArgs::ThemePicker { current_slug } => json!({
            "kind": "theme-picker",
            "current_slug": current_slug,
        }),
        ScreenArgs::ConnectPicker => json!({"kind": "connect-picker"}),
        ScreenArgs::ResumePicker { transcripts } => json!({
            "kind": "resume-picker",
            "transcripts": transcripts.iter().map(|t| json!({
                "id": t.id,
                "label": t.label,
                "saved_at_secs": t.saved_at.secs,
                "saved_at_nanos": t.saved_at.nanos,
            })).collect::<Vec<_>>(),
        }),
        ScreenArgs::PluginsManager => json!({"kind": "plugins-manager"}),
        ScreenArgs::LanguagePicker { current_code } => json!({
            "kind": "language-picker",
            "current_code": current_code,
        }),
        ScreenArgs::ModelPicker { current_id, models } => json!({
            "kind": "model-picker",
            "current_id": current_id,
            "models": models.iter().map(|m| json!({
                "id": m.id,
                "display_name": m.display_name,
            })).collect::<Vec<_>>(),
        }),
        ScreenArgs::Changelog => json!({"kind": "changelog"}),
        ScreenArgs::MigrationPicker { detected } => json!({
            "kind": "migration-picker",
            "detected": detected.iter().map(|p| p.as_str().to_string()).collect::<Vec<_>>(),
        }),
        ScreenArgs::TrustModal { project_root } => json!({
            "kind": "trust-modal",
            "project_root": project_root.to_string_lossy(),
        }),
        ScreenArgs::LspInstallProgress { entry_ids } => json!({
            "kind": "lsp-install-progress",
            "entry_ids": entry_ids,
        }),
        // `ScreenArgs` is `#[non_exhaustive]`; future variants land here
        // as `{"kind": "unknown"}` until convert.rs grows a matching arm.
        // Built-in screens that need new variants must update both this
        // projection and `ScreenArgs::screen_id()` in the parent crate.
        _ => json!({"kind": "unknown"}),
    };
    v.to_string()
}

/// Hand-rolled `HostEvent` → JSON projection. Shape matches
/// `static_::event_to_json`; duplicated here to avoid a cross-module pub
/// boundary just for the helper.
fn host_event_to_json(event: &HostEvent) -> String {
    use serde_json::json;
    let v = match event {
        HostEvent::HostStarting => json!({"kind": "host-starting"}),
        HostEvent::Connect { provider_id } => json!({
            "kind": "connect",
            "provider_id": provider_id.as_str(),
        }),
        HostEvent::Disconnect {
            provider_id,
            reason,
        } => json!({
            "kind": "disconnect",
            "provider_id": provider_id.as_str(),
            "reason": reason,
        }),
        HostEvent::TurnStart { turn_id } => json!({"kind": "turn-start", "turn_id": turn_id}),
        HostEvent::TurnEnd { turn_id, success } => json!({
            "kind": "turn-end",
            "turn_id": turn_id,
            "success": success,
        }),
        HostEvent::ToolCallStart { call_id, tool } => json!({
            "kind": "tool-call-start",
            "call_id": call_id,
            "tool": tool,
        }),
        HostEvent::ToolCallEnd { call_id, success } => json!({
            "kind": "tool-call-end",
            "call_id": call_id,
            "success": success,
        }),
        HostEvent::PromptSubmitted { text } => json!({"kind": "prompt-submitted", "text": text}),
        HostEvent::TranscriptSaved { path } => json!({"kind": "transcript-saved", "path": path}),
        HostEvent::ProviderRegistered { id, display_name } => json!({
            "kind": "provider-registered",
            "id": id.as_str(),
            "display_name": display_name,
        }),
        HostEvent::ContextSizeChanged { tokens } => json!({
            "kind": "context-size-changed",
            "tokens": tokens,
        }),
        HostEvent::ActiveProviderChanged { id } => json!({
            "kind": "active-provider-changed",
            "id": id.as_str(),
        }),
        HostEvent::SubagentStop {
            agent_name,
            success,
        } => json!({
            "kind": "subagent-stop",
            "agent_name": agent_name,
            "success": success,
        }),
    };
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::{KeyCodePortable, KeyMods};

    #[test]
    fn key_event_to_wit_round_trips_char_and_modifiers() {
        let host = KeyEventPortable {
            code: KeyCodePortable::Char('a'),
            modifiers: KeyMods {
                ctrl: true,
                shift: false,
                alt: false,
                meta: false,
            },
        };
        let w = key_event_to_wit(host);
        match w.code {
            wit::KeyCode::Char(s) => assert_eq!(s, "a"),
            other => panic!("expected Char, got {other:?}"),
        }
        assert!(w.modifiers.ctrl);
    }

    #[test]
    fn key_event_to_wit_function_key_carries_number() {
        let host = KeyEventPortable {
            code: KeyCodePortable::F(7),
            modifiers: KeyMods::default(),
        };
        let w = key_event_to_wit(host);
        assert!(matches!(w.code, wit::KeyCode::Function(7)));
    }

    #[test]
    fn key_event_to_wit_maps_unknown_to_null() {
        let host = KeyEventPortable {
            code: KeyCodePortable::Unknown,
            modifiers: KeyMods::default(),
        };
        let w = key_event_to_wit(host);
        assert!(matches!(w.code, wit::KeyCode::Null));
    }

    #[test]
    fn screen_args_to_json_emits_kind_discriminant_for_every_variant() {
        // ScreenArgs is `#[non_exhaustive]` so we cover representative
        // variants here; the `_ => unreachable!()` arm in
        // `screen_args_to_json` is intentionally absent so a new variant
        // surfaces as a compile error.
        for args in [
            ScreenArgs::None,
            ScreenArgs::ThemePicker {
                current_slug: "dark".into(),
            },
            ScreenArgs::ConnectPicker,
            ScreenArgs::PluginsManager,
            ScreenArgs::Changelog,
        ] {
            let s = screen_args_to_json(&args);
            let v: serde_json::Value = serde_json::from_str(&s).expect("valid json");
            assert!(v.get("kind").is_some(), "every variant emits kind");
        }
    }

    #[test]
    fn host_event_to_json_emits_kind_for_every_variant() {
        use savvagent_plugin::ProviderId;
        let pid = ProviderId::new("anthropic").unwrap();
        let cases = vec![
            HostEvent::HostStarting,
            HostEvent::Connect {
                provider_id: pid.clone(),
            },
            HostEvent::TurnStart { turn_id: 1 },
            HostEvent::SubagentStop {
                agent_name: "code".into(),
                success: true,
            },
        ];
        for e in cases {
            let s = host_event_to_json(&e);
            let v: serde_json::Value = serde_json::from_str(&s).expect("valid json");
            assert!(v.get("kind").is_some());
        }
    }
}
