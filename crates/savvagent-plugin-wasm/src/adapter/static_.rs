//! `plugin-static` world adapter.
//!
//! Loads a `.wasm` component, wires the two host imports (`log`,
//! `current-theme`), instantiates the component into a long-lived store,
//! caches the manifest and theme catalog at construction time, and exposes
//! a `Box<dyn savvagent_plugin::Plugin>` to the runtime.
//!
//! ## Concurrency model
//!
//! Per-store the wasm export calls require `&mut Store`, and `Store` is not
//! `Send`-safe to clone, so a single `tokio::sync::Mutex<StoreAndInstance>`
//! serializes every export call. The `Plugin` trait methods `&mut self`
//! (handle_slash, on_event) are themselves serial in the runtime, so this
//! mutex never contends in practice — it exists so the `&self`-only methods
//! (`manifest`, `render_slot`, `themes`) can read the cached snapshots that
//! were populated under the mutex at construction time.
//!
//! ## Cached values
//!
//! `manifest()` and `themes()` are `&self` + sync in the trait, but the
//! wasm exports are `&mut store` + async. Resolve at construction time and
//! cache the conversion result; subsequent reads are zero-cost.
//!
//! ## Trap recovery + three-strikes (Task 8)
//!
//! When a wasm call traps, the `StoreAndInstance` is destroyed and a fresh
//! one is built from the cached `Component` + `Linker`. The trap is also
//! recorded in [`StrikeCounter`]; if three or more land inside the rolling
//! 10-minute window the adapter flips `disabled` and short-circuits every
//! subsequent call with `PluginError::Internal("plugin disabled by
//! strikes")`. The disable signal stays local to the adapter in v0.18.0;
//! a v0.18.1 follow-up will hook it into the registry so the
//! `internal:plugins-manager` plugin can persist `disabled_reason` to
//! the trust ledger.
//!
//! ## Epoch interruption
//!
//! Each per-call code path calls [`Store::set_epoch_deadline`] from the
//! manifest's `call_timeout_ms`. The shared engine's background bumper
//! advances the epoch every [`crate::engine::EPOCH_TICK`]; runaway wasm
//! traps with a `Trap::Interrupt`, which surfaces through the adapter as
//! a `PluginError::Internal("wasm trap in handle_slash: ...interrupt...")`
//! that the strike counter then attributes.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use tokio::sync::Mutex;
use wasmtime::Store;
use wasmtime::component::{Component, HasSelf, Linker};

use savvagent_plugin::manifest::Manifest;
use savvagent_plugin::{Effect, HostEvent, Plugin, PluginError, Region, StyledLine, ThemeEntry};

use crate::convert::{
    effect_from_wit, manifest_from_wit, plugin_error_from_wit, theme_color_to_wit,
    theme_entry_from_wit,
};
use crate::engine::{EPOCH_TICK, shared_engine};
use crate::error::WasmPluginError;
use crate::host_imports::{log as log_host, theme};
use crate::manifest::PluginManifest as DiskManifest;
use crate::static_world::{
    self, PluginStatic, PluginStaticImports, savvagent::plugin::types as wit,
};
use crate::strikes::{StrikeCounter, StrikeOutcome};

/// Per-store state that lives inside the wasmtime [`Store`]. The host
/// imports project from `&mut StaticHostState` via [`HasSelf`].
pub(crate) struct StaticHostState {
    /// Stable id of this plugin, attached to every host-side log event.
    plugin_id: String,
    /// Shared, live theme snapshot. Read under `theme.read().await`.
    theme: theme::ThemeProvider,
}

// `PluginStaticImports` is the auto-generated trait the bindgen world emits
// for the `import log` + `import current-theme` functions. We implement it
// on `StaticHostState`; the bindgen-emitted blanket `impl<_T> for &mut _T`
// then satisfies the `for<'a> D::Data<'a>: PluginStaticImports` bound that
// `add_to_linker` requires when paired with `HasSelf<StaticHostState>`.
impl PluginStaticImports for StaticHostState {
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

// The bindgen also requires a `Host` impl for the `types` interface. The
// generated trait is empty; this satisfies the `add_to_linker` bound.
impl static_world::savvagent::plugin::types::Host for StaticHostState {}

/// Holds the wasm `Store` + the loaded `PluginStatic` together so calls
/// that need both can borrow them under one lock.
struct StoreAndInstance {
    store: Store<StaticHostState>,
    instance: PluginStatic,
}

/// Adapter that wraps a `plugin-static` wasm component as a
/// `Box<dyn Plugin>`.
pub struct StaticAdapter {
    cached_manifest: Manifest,
    cached_themes: Vec<ThemeEntry>,
    inner: Arc<Mutex<StoreAndInstance>>,
    /// Held purely for trap-recovery in Task 8; the field is read indirectly
    /// in v0.18.0 only through [`StaticAdapter::disk_manifest`].
    disk_manifest: Arc<DiskManifest>,
    /// Cached `Component` so trap-recovery can re-instantiate without
    /// re-reading the wasm bytes from disk.
    component: Component,
    /// Cached `Linker` so trap-recovery can re-instantiate without
    /// re-wiring host imports.
    linker: Linker<StaticHostState>,
    /// Rolling-window trap counter (Task 8). Three traps inside the
    /// window flip [`Self::disabled`].
    strikes: Arc<StrikeCounter>,
    /// Set to `true` once [`StrikeCounter::record`] returns
    /// [`StrikeOutcome::Disable`]. Every subsequent call short-circuits
    /// with `PluginError::Internal("plugin disabled by strikes")` — the
    /// adapter never re-issues a wasm call after this flips.
    disabled: Arc<AtomicBool>,
}

impl StaticAdapter {
    /// Borrow the parsed `plugin.toml` this adapter was constructed from.
    /// Held purely so Task 8's recovery path can re-derive the wasm path
    /// + identity without re-walking the four-path discovery.
    pub fn disk_manifest(&self) -> &Arc<DiskManifest> {
        &self.disk_manifest
    }

    /// Borrow the rolling-window strike counter. Exposed for tests + the
    /// future Task 9 registry wiring (which needs to observe disable
    /// transitions to update the trust ledger).
    pub fn strikes(&self) -> &Arc<StrikeCounter> {
        &self.strikes
    }

    /// `true` once the rolling-window strike counter has flipped this
    /// adapter to disabled. Read once per call as a fast-path check.
    pub fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::SeqCst)
    }
}

impl std::fmt::Debug for StaticAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticAdapter")
            .field("id", &self.cached_manifest.id.as_str())
            .field("name", &self.cached_manifest.name)
            .field("version", &self.cached_manifest.version)
            .field("disabled", &self.is_disabled())
            .finish()
    }
}

impl StaticAdapter {
    /// Construct a `StaticAdapter` by loading `plugin.wasm` from
    /// `plugin_dir`, wiring host imports, instantiating, and caching
    /// manifest + themes.
    ///
    /// `disk_manifest` is the parsed `plugin.toml`; it carries identity and
    /// is consulted by Task 8's recovery path. `theme` is the runtime's
    /// shared theme snapshot the host import `current-theme()` reads from.
    pub async fn new(
        disk_manifest: Arc<DiskManifest>,
        plugin_dir: &Path,
        theme: theme::ThemeProvider,
    ) -> Result<Self, WasmPluginError> {
        let engine = shared_engine()?;
        let wasm_path = plugin_dir.join("plugin.wasm");
        let component =
            Component::from_file(&engine, &wasm_path).map_err(WasmPluginError::Wasmtime)?;

        let mut linker: Linker<StaticHostState> = Linker::new(&engine);
        PluginStatic::add_to_linker::<_, HasSelf<StaticHostState>>(&mut linker, |s| s)
            .map_err(WasmPluginError::Wasmtime)?;

        let plugin_id = disk_manifest.plugin.id.clone();

        // Construct the initial store + instance with a sentinel deadline
        // so the construction-time `manifest`/`themes` calls — which we
        // don't yet have a `call_timeout_ms` budget for — never trap.
        let (mut store, instance) =
            build_store_and_instance(&engine, &component, &linker, &plugin_id, &theme).await?;
        // Construction-time deadline: effectively never. Per-call paths
        // re-set the deadline before issuing the wasm call.
        // Use `u64::MAX / 2` (not `u64::MAX`) as the construction-time
        // sentinel — wasmtime computes the absolute deadline as
        // `current_epoch + delta`, which would overflow if the bumper
        // has already advanced the epoch a few times. Half of u64 still
        // gives the equivalent of ~thousands of years before tripping.
        store.set_epoch_deadline(u64::MAX / 2);

        // Cache manifest at construction.
        let wit_manifest = instance
            .call_manifest(&mut store)
            .await
            .map_err(WasmPluginError::Wasmtime)?
            .map_err(|e| {
                WasmPluginError::Manifest(
                    wasm_path.clone(),
                    format!("plugin returned error: {e:?}"),
                )
            })?;
        let cached_manifest = manifest_from_wit(wit_manifest)?;

        // Cache themes at construction.
        let wit_themes = instance
            .call_themes(&mut store)
            .await
            .map_err(WasmPluginError::Wasmtime)?;
        let cached_themes: Vec<ThemeEntry> =
            wit_themes.into_iter().map(theme_entry_from_wit).collect();

        Ok(Self {
            cached_manifest,
            cached_themes,
            inner: Arc::new(Mutex::new(StoreAndInstance { store, instance })),
            disk_manifest,
            component,
            linker,
            strikes: Arc::new(StrikeCounter::default()),
            disabled: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Per-call epoch deadline in ticks of [`EPOCH_TICK`].
    ///
    /// Reads `runtime.call_timeout_ms` from the manifest and converts to
    /// ticks. A timeout shorter than one tick is rounded up to one so the
    /// wasm always gets at least one quantum to execute.
    fn call_deadline_ticks(&self) -> u64 {
        let ms = u64::from(self.disk_manifest.runtime.call_timeout_ms);
        let tick_ms = EPOCH_TICK.as_millis() as u64;
        ms.div_ceil(tick_ms.max(1)).max(1)
    }

    /// Replace the live `StoreAndInstance` after a wasm trap. Used by the
    /// trap-recovery path: when a wasm call fails, we discard the
    /// post-trap store (it may be in an inconsistent state) and build a
    /// fresh one off the cached `Component` + `Linker`.
    ///
    /// On failure the adapter's `inner` is left holding the (now-dead)
    /// pre-trap instance. The next call will trap again and re-record a
    /// strike; after three the disable short-circuit takes over.
    async fn rebuild_instance(&self) -> Result<(), WasmPluginError> {
        let engine = shared_engine()?;
        let plugin_id = self.disk_manifest.plugin.id.clone();
        // Pull a fresh ThemeProvider clone out of the existing host state
        // by minting a new provider that resolves to the same snapshot.
        // The theme provider is just an Arc-backed handle; the simplest
        // correct path is to keep a clone on `self` — but adding a field
        // would change the public surface. Instead we re-read the cached
        // theme from the running `StaticHostState` under the inner lock.
        let theme = {
            let guard = self.inner.lock().await;
            guard.store.data().theme.clone()
        };
        let (mut store, instance) =
            build_store_and_instance(&engine, &self.component, &self.linker, &plugin_id, &theme)
                .await?;
        // Use `u64::MAX / 2` (not `u64::MAX`) as the construction-time
        // sentinel — wasmtime computes the absolute deadline as
        // `current_epoch + delta`, which would overflow if the bumper
        // has already advanced the epoch a few times. Half of u64 still
        // gives the equivalent of ~thousands of years before tripping.
        store.set_epoch_deadline(u64::MAX / 2);
        let mut guard = self.inner.lock().await;
        *guard = StoreAndInstance { store, instance };
        Ok(())
    }

    /// Common pre-call short-circuit: returns `Err` if the adapter is
    /// already disabled by strikes. Centralizes the disabled-message
    /// wording so tests can assert on one substring.
    fn check_disabled(&self) -> Result<(), PluginError> {
        if self.is_disabled() {
            Err(PluginError::Internal(
                "plugin disabled by strikes (repeated wasm traps)".to_string(),
            ))
        } else {
            Ok(())
        }
    }

    /// Handle a wasm-call result: on Err, record a strike and (if not
    /// disabled) rebuild the instance. Always returns the (possibly
    /// strike-annotated) original Err.
    async fn handle_call_error(&self, err: anyhow::Error, op: &'static str) -> PluginError {
        match self.strikes.record() {
            StrikeOutcome::Continue { count, window } => {
                // Rebuild before returning so the next call gets a fresh
                // store. A rebuild failure means the host can't restore
                // the instance — log it but still surface the original
                // trap to the caller, which is what they were waiting
                // on.
                if let Err(rebuild_err) = self.rebuild_instance().await {
                    tracing::warn!(
                        plugin = %self.disk_manifest.plugin.id,
                        ?rebuild_err,
                        "wasm post-trap rebuild failed",
                    );
                }
                tracing::warn!(
                    plugin = %self.disk_manifest.plugin.id,
                    op,
                    count,
                    window_secs = window.as_secs(),
                    "wasm trap recorded ({count}/{} in last {}s)",
                    crate::strikes::LIMIT,
                    window.as_secs(),
                );
                PluginError::Internal(format!("wasm trap in {op}: {err}"))
            }
            StrikeOutcome::Disable => {
                self.disabled.store(true, Ordering::SeqCst);
                tracing::error!(
                    plugin = %self.disk_manifest.plugin.id,
                    op,
                    "plugin disabled by strikes after wasm trap",
                );
                PluginError::Internal(format!(
                    "plugin disabled by strikes (repeated wasm traps); last trap in {op}: {err}"
                ))
            }
        }
    }
}

/// Build a fresh `Store<StaticHostState>` + `PluginStatic` pair off a
/// cached `Component` + `Linker`. Used at construction time and from the
/// trap-recovery path.
async fn build_store_and_instance(
    engine: &wasmtime::Engine,
    component: &Component,
    linker: &Linker<StaticHostState>,
    plugin_id: &str,
    theme: &theme::ThemeProvider,
) -> Result<(Store<StaticHostState>, PluginStatic), WasmPluginError> {
    let state = StaticHostState {
        plugin_id: plugin_id.to_string(),
        theme: theme.clone(),
    };
    let mut store = Store::new(engine, state);
    let instance = PluginStatic::instantiate_async(&mut store, component, linker)
        .await
        .map_err(WasmPluginError::Wasmtime)?;
    Ok((store, instance))
}

#[async_trait]
impl Plugin for StaticAdapter {
    fn manifest(&self) -> Manifest {
        self.cached_manifest.clone()
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        self.check_disabled()?;
        let deadline = self.call_deadline_ticks();
        let result = {
            let mut guard = self.inner.lock().await;
            let StoreAndInstance { store, instance } = &mut *guard;
            store.set_epoch_deadline(deadline);
            instance.call_handle_slash(&mut *store, name, &args).await
        };
        match result {
            Ok(Ok(wit_effects)) => {
                let mut effects = Vec::with_capacity(wit_effects.len());
                for e in wit_effects {
                    effects.push(
                        effect_from_wit(e).map_err(|err| PluginError::Internal(err.to_string()))?,
                    );
                }
                Ok(effects)
            }
            Ok(Err(plugin_err)) => Err(plugin_error_from_wit(plugin_err)),
            Err(e) => Err(self.handle_call_error(e, "handle_slash").await),
        }
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        self.check_disabled()?;
        let event_json = serde_json::to_string(&event_to_json(&event))
            .map_err(|e| PluginError::Internal(format!("serialize HostEvent: {e}")))?;
        let deadline = self.call_deadline_ticks();
        let result = {
            let mut guard = self.inner.lock().await;
            let StoreAndInstance { store, instance } = &mut *guard;
            store.set_epoch_deadline(deadline);
            instance.call_on_event(&mut *store, &event_json).await
        };
        match result {
            Ok(Ok(wit_effects)) => {
                let mut effects = Vec::with_capacity(wit_effects.len());
                for e in wit_effects {
                    effects.push(
                        effect_from_wit(e).map_err(|err| PluginError::Internal(err.to_string()))?,
                    );
                }
                Ok(effects)
            }
            Ok(Err(plugin_err)) => Err(plugin_error_from_wit(plugin_err)),
            Err(e) => Err(self.handle_call_error(e, "on_event").await),
        }
    }

    fn render_slot(&self, _slot_id: &str, _region: Region) -> Vec<StyledLine> {
        // `render_slot` is `&self` + sync, but the wasm export is `&mut
        // store` + async. Bridging would require either `block_in_place`
        // (deadlock-prone if called from a single-threaded runtime) or a
        // background pumper that pre-renders slots — both meaningful
        // designs for a later release. v0.18.0 punts: external static
        // plugins simply cannot contribute render slots. Built-in
        // plugins, which still have direct `&self` access to their state,
        // continue to render slots as before.
        Vec::new()
    }

    fn themes(&self) -> Vec<ThemeEntry> {
        self.cached_themes.clone()
    }
}

/// Render a `HostEvent` as a JSON-shaped value the wasm guest can parse.
/// We hand-roll the projection here rather than relying on a `serde::Serialize`
/// on `HostEvent` (which the trait-surface crate intentionally does not
/// provide) so the wire shape is stable across savvagent versions.
fn event_to_json(event: &HostEvent) -> serde_json::Value {
    use serde_json::json;
    match event {
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
        HostEvent::TurnStart { turn_id } => json!({
            "kind": "turn-start",
            "turn_id": turn_id,
        }),
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
        HostEvent::PromptSubmitted { text } => json!({
            "kind": "prompt-submitted",
            "text": text,
        }),
        HostEvent::TranscriptSaved { path } => json!({
            "kind": "transcript-saved",
            "path": path,
        }),
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::ProviderId;

    #[test]
    fn event_to_json_covers_every_variant() {
        let pid = ProviderId::new("anthropic").unwrap();
        let cases = vec![
            HostEvent::HostStarting,
            HostEvent::Connect {
                provider_id: pid.clone(),
            },
            HostEvent::Disconnect {
                provider_id: pid.clone(),
                reason: "bye".into(),
            },
            HostEvent::TurnStart { turn_id: 1 },
            HostEvent::TurnEnd {
                turn_id: 2,
                success: true,
            },
            HostEvent::ToolCallStart {
                call_id: "c".into(),
                tool: "read_file".into(),
            },
            HostEvent::ToolCallEnd {
                call_id: "c".into(),
                success: false,
            },
            HostEvent::PromptSubmitted { text: "hi".into() },
            HostEvent::TranscriptSaved {
                path: "/tmp/t.json".into(),
            },
            HostEvent::ProviderRegistered {
                id: pid.clone(),
                display_name: "Anthropic".into(),
            },
            HostEvent::ContextSizeChanged { tokens: 42 },
            HostEvent::ActiveProviderChanged { id: pid },
            HostEvent::SubagentStop {
                agent_name: "code-reviewer".into(),
                success: true,
            },
        ];
        for e in cases {
            let v = event_to_json(&e);
            assert!(v.get("kind").is_some(), "every variant emits a `kind`");
        }
    }
}
