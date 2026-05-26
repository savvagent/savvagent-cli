//! `plugin-provider` world adapter.
//!
//! Bridges a wasm component implementing the `plugin-provider` world to
//! [`savvagent_mcp::ProviderClient`] — the host-facing trait every provider
//! impl (in-process, MCP-over-HTTP, or now wasm) presents.
//!
//! ## Concurrency model
//!
//! Each `ProviderClient` method (`complete`, `list_models`) constructs a
//! **fresh** `Store<ProviderHostState>` and instantiates the component from
//! a cached `InstancePre`. This is the simplest correct design:
//!
//! - No store reuse across calls means no per-store state can leak between
//!   turns (api keys read into a global, partial streaming state from a
//!   crashed turn, …).
//! - Per-call `Store` ownership means each call's `mpsc::Sender` for
//!   streaming events lives only for the duration of that call.
//! - `InstancePre` does the export-shape typecheck once at construction;
//!   per-call instantiation only pays the wasm-module-instantiation cost,
//!   not the typecheck.
//!
//! The plan sketched a per-store pool to reduce instantiation latency.
//! v0.18.0 ships without one — measurement first, optimize later. The
//! pool slot is reserved in [`WasmProviderClient`] as a `Mutex<Vec<...>>`
//! that's never populated; a future revision can pop from it when
//! non-empty, falling back to fresh construction otherwise.
//!
//! ## `count_tokens`
//!
//! [`ProviderClient`] does **not** declare `count_tokens` — that method
//! lives only on the wasm side. We expose it here as an inherent method on
//! `WasmProviderClient` so callers that need it can dispatch through the
//! adapter; it is not part of the dyn-trait surface and therefore won't
//! flow into the runtime's `PROVIDERS` slot.
//!
//! ## Error mapping
//!
//! Three layers of failure can surface from this adapter:
//!
//! 1. **Plugin returned `provider-error`** → mapped via
//!    `From<wit::ProviderError> for spp::ProviderError` (defined in
//!    `spp_convert.rs`). The plugin owns the error taxonomy here.
//! 2. **Wasm trap / instantiation failure** → wrapped as a synthetic
//!    `ProviderError { kind: Internal, message: "wasmtime: ..." }`.
//!    `Internal` (not `Transport`) so the host's retry/fallback layer —
//!    which special-cases `Transport` for "try a different endpoint" —
//!    doesn't try to retry a permanently-broken plugin. See
//!    `wasm_error_to_provider_error` and `disabled_provider_error`.
//! 3. **`fetch-stream` or unimplemented capability** → not reachable from
//!    this adapter directly; the plugin would get the corresponding
//!    `HttpError`/`KeyringError` and surface it as its own
//!    `ProviderError`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc};
use wasmtime::Store;
use wasmtime::component::{Component, HasSelf, Linker};

use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ErrorKind, ListModelsResponse, ProviderError, StreamEvent,
};

use crate::engine::{EPOCH_TICK, shared_engine};
use crate::error::WasmPluginError;
use crate::host_imports::{
    http::HttpState, keyring::KeyringState, log as log_host, progress::ProgressState,
};
use crate::manifest::PluginManifest as DiskManifest;
use crate::provider_world::{
    PluginProvider, PluginProviderImports, PluginProviderPre,
    savvagent::plugin::{
        http_capability as http_wit, keyring_capability as keyring_wit,
        progress_capability as progress_wit, spp as spp_wit, types as wit,
    },
};
use crate::strikes::{StrikeCounter, StrikeOutcome};

/// Per-store state for the provider-world wasm Store.
///
/// One fresh value per call — never shared. The four sub-fields are the
/// host implementations of the four declared imports (`log`, `http`,
/// `keyring`, `progress`).
pub(crate) struct ProviderHostState {
    /// Plugin id (`<vendor>:<rest>`) attached to every host-side log event.
    plugin_id: String,
    /// HTTP capability state. Holds the reqwest client + manifest-derived
    /// allow-list.
    http: HttpState,
    /// Keyring capability state. Holds the manifest-derived account
    /// allow-list.
    keyring: KeyringState,
    /// Progress capability state. Holds an optional `mpsc::Sender` for
    /// streaming-event forwarding.
    progress: ProgressState,
}

// ---- Host trait impls -----------------------------------------------
//
// The bindgen emits one trait per declared import (`log` is inline on
// the world; `http-capability`, `keyring-capability`, `progress-capability`
// each get a trait `Host` per interface). We implement each on
// `ProviderHostState` so `add_to_linker::<_, HasSelf<ProviderHostState>>`
// picks up all four in one shot.

// Inline `log` export on the world.
impl PluginProviderImports for ProviderHostState {
    async fn log(&mut self, level: wit::LogLevel, msg: String) {
        log_host::emit(&self.plugin_id, level, &msg);
    }
}

// `savvagent:plugin/types` is the shared (and now `with:`-aliased)
// types interface. Its bindgen trait surface is empty; the impl is a
// formality the linker needs.
impl wit::Host for ProviderHostState {}

// `savvagent:plugin/spp` — bindgen-generated empty Host trait. The spp
// interface is type-only (no functions imported by the world), so this
// is also a no-op marker.
impl spp_wit::Host for ProviderHostState {}

// `savvagent:plugin/http-capability`
impl http_wit::Host for ProviderHostState {
    async fn fetch(
        &mut self,
        req: http_wit::HttpRequest,
    ) -> Result<http_wit::HttpResponse, http_wit::HttpError> {
        self.http.fetch(req).await
    }

    async fn fetch_stream(
        &mut self,
        _req: http_wit::HttpRequest,
    ) -> Result<wasmtime::component::Resource<http_wit::HttpStream>, http_wit::HttpError> {
        // Reserved for v0.19.0. Returning a Transport error keeps the
        // failure path total without invoking unimplemented!/panic; see
        // the module docs for the rationale.
        Err(http_wit::HttpError::Transport(
            "fetch-stream is not supported by this host (savvagent v0.18.0)".to_string(),
        ))
    }
}

// `savvagent:plugin/http-capability/http-stream` — resource host impl.
// Required by the bindgen even though we never construct one: every
// method on the resource must be wired so the linker knows what to do
// if a plugin somehow obtains a handle. We return Transport for all of
// them, matching the `fetch-stream` denial above.
impl http_wit::HostHttpStream for ProviderHostState {
    async fn status(&mut self, _rep: wasmtime::component::Resource<http_wit::HttpStream>) -> u16 {
        0
    }

    async fn headers(
        &mut self,
        _rep: wasmtime::component::Resource<http_wit::HttpStream>,
    ) -> Vec<(String, String)> {
        Vec::new()
    }

    async fn next_chunk(
        &mut self,
        _rep: wasmtime::component::Resource<http_wit::HttpStream>,
    ) -> Result<Option<Vec<u8>>, http_wit::HttpError> {
        Err(http_wit::HttpError::Transport(
            "fetch-stream is not supported by this host (savvagent v0.18.0)".to_string(),
        ))
    }

    async fn drop(
        &mut self,
        _rep: wasmtime::component::Resource<http_wit::HttpStream>,
    ) -> wasmtime::Result<()> {
        Ok(())
    }
}

// `savvagent:plugin/keyring-capability`
impl keyring_wit::Host for ProviderHostState {
    async fn get(&mut self, account: String) -> Result<String, keyring_wit::KeyringError> {
        self.keyring.get(&account)
    }
}

// `savvagent:plugin/progress-capability`
impl progress_wit::Host for ProviderHostState {
    async fn emit_stream_event(&mut self, event: progress_wit::StreamEvent) {
        self.progress.emit(event).await;
    }
}

// ---- Adapter --------------------------------------------------------

/// Adapter that wraps a `plugin-provider` wasm component as a
/// `Box<dyn ProviderClient>`.
///
/// Construction does the bindgen typecheck and caches an `InstancePre`;
/// every `complete` / `list_models` / `count_tokens` call mints a fresh
/// Store, instantiates, calls, and drops the Store.
pub struct WasmProviderClient {
    /// Long-lived pre-instantiated component. Cloning is `Arc`-cheap.
    instance_pre: Arc<PluginProviderPre<ProviderHostState>>,
    /// Parsed `plugin.toml`. Needed at every call to construct the
    /// per-store `HttpState`/`KeyringState` (their allow-lists come from
    /// `[security]`).
    disk_manifest: Arc<DiskManifest>,
    /// Reserved for the per-call store pool (see module docs). Always
    /// empty in v0.18.0; the field is held so a future revision can wire
    /// a `try_pop`-or-new path without an ABI break.
    _store_pool: Mutex<Vec<Store<ProviderHostState>>>,
    /// Rolling-window trap counter (Task 8). Provider calls always
    /// build a fresh store, so there's no "rebuild instance" step — but
    /// we still want a runaway plugin to stop getting called.
    strikes: Arc<StrikeCounter>,
    /// `true` once the strike counter has flipped this client to
    /// disabled. Every subsequent call short-circuits with a Transport
    /// `ProviderError` so the host's retry/fallback layer can route
    /// around it.
    disabled: Arc<AtomicBool>,
}

impl std::fmt::Debug for WasmProviderClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmProviderClient")
            .field("plugin_id", &self.disk_manifest.plugin.id.as_str())
            .field(
                "provider_id",
                &self.disk_manifest.exports.provider_id.as_deref(),
            )
            .finish()
    }
}

impl WasmProviderClient {
    /// Construct a `WasmProviderClient` by loading `plugin.wasm` from
    /// `plugin_dir`, wiring all four host imports, and pre-instantiating
    /// the component.
    ///
    /// The wasm file is loaded once; per-call instantiation reuses the
    /// `Component` indirectly via the `InstancePre`.
    pub async fn new(
        disk_manifest: Arc<DiskManifest>,
        plugin_dir: &Path,
    ) -> Result<Self, WasmPluginError> {
        let engine = shared_engine()?;
        let wasm_path = plugin_dir.join("plugin.wasm");
        let component =
            Component::from_file(&engine, &wasm_path).map_err(WasmPluginError::Wasmtime)?;

        let mut linker: Linker<ProviderHostState> = Linker::new(&engine);
        PluginProvider::add_to_linker::<_, HasSelf<ProviderHostState>>(&mut linker, |s| s)
            .map_err(WasmPluginError::Wasmtime)?;

        let pre = linker
            .instantiate_pre(&component)
            .map_err(WasmPluginError::Wasmtime)?;
        let plugin_pre = PluginProviderPre::new(pre).map_err(WasmPluginError::Wasmtime)?;

        Ok(Self {
            instance_pre: Arc::new(plugin_pre),
            disk_manifest,
            _store_pool: Mutex::new(Vec::new()),
            strikes: Arc::new(StrikeCounter::default()),
            disabled: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Borrow the rolling-window strike counter. Exposed for tests + the
    /// future Task 9 registry wiring.
    pub fn strikes(&self) -> &Arc<StrikeCounter> {
        &self.strikes
    }

    /// `true` once the rolling-window strike counter has flipped this
    /// client to disabled.
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

    /// Common pre-call short-circuit. Returns a Transport-class
    /// `ProviderError` to match the existing error-mapping convention
    /// for wasm/instantiation failures.
    fn check_disabled(&self) -> Result<(), ProviderError> {
        if self.is_disabled() {
            Err(disabled_provider_error())
        } else {
            Ok(())
        }
    }

    /// Record one wasm trap and (if we crossed the limit) flip
    /// `disabled`. Returns the trap-wrapped `ProviderError` the caller
    /// should surface.
    fn record_trap(&self, msg: String, op: &'static str) -> ProviderError {
        match self.strikes.record() {
            StrikeOutcome::Continue { count, window } => {
                tracing::warn!(
                    plugin = %self.disk_manifest.plugin.id,
                    op,
                    count,
                    window_secs = window.as_secs(),
                    "wasm provider trap recorded ({count}/{} in last {}s)",
                    crate::strikes::LIMIT,
                    window.as_secs(),
                );
                wasm_error_to_provider_error(&msg)
            }
            StrikeOutcome::Disable => {
                self.disabled.store(true, Ordering::SeqCst);
                tracing::error!(
                    plugin = %self.disk_manifest.plugin.id,
                    op,
                    "wasm provider disabled by strikes after repeated traps",
                );
                ProviderError {
                    kind: ErrorKind::Internal,
                    message: format!(
                        "wasmtime: plugin disabled by strikes (repeated wasm traps); last trap in {op}: {msg}"
                    ),
                    retry_after_ms: None,
                    provider_code: None,
                }
            }
        }
    }

    /// Borrow the parsed `plugin.toml` this adapter was constructed from.
    /// Used by tests and Task 9's `PROVIDERS` extender to read
    /// `[exports] provider-id` without re-parsing.
    pub fn disk_manifest(&self) -> &Arc<DiskManifest> {
        &self.disk_manifest
    }

    /// Build a fresh `ProviderHostState` for one call.
    ///
    /// `events` is `Some(sender)` for `complete` (when the caller asked
    /// for streaming) and `None` for `list_models` / `count_tokens`.
    /// The allow-lists are pulled out of the cached manifest every call;
    /// they're cheap `Vec<String>` clones (the manifest's `[security]`
    /// table is small).
    fn new_host_state(&self, events: Option<mpsc::Sender<StreamEvent>>) -> ProviderHostState {
        // `[security]` is provider-world-only (enforced by
        // `manifest.rs`), but the field is still `Option<...>` — an
        // absent section means empty allow-lists, which is the most
        // restrictive setting we can derive automatically.
        let (allowed_hosts, keyring_accounts) = match &self.disk_manifest.security {
            Some(s) => (s.allowed_hosts.clone(), s.keyring_accounts.clone()),
            None => (Vec::new(), Vec::new()),
        };
        let progress = match events {
            Some(tx) => ProgressState::enabled(tx),
            None => ProgressState::disabled(),
        };
        ProviderHostState {
            plugin_id: self.disk_manifest.plugin.id.clone(),
            http: HttpState::new(allowed_hosts),
            keyring: KeyringState::new(keyring_accounts),
            progress,
        }
    }

    /// Call `count-tokens` against the plugin. Not part of the
    /// `ProviderClient` trait surface — exposed here as an inherent
    /// method so callers that need it can dispatch through the adapter.
    ///
    /// `model` and `messages` are passed verbatim into the WIT-side
    /// `count-tokens-request`; this mirrors the request shape declared
    /// in `spp.wit`.
    pub async fn count_tokens(
        &self,
        req: CountTokensRequest,
    ) -> Result<CountTokensResponse, ProviderError> {
        self.check_disabled()?;
        let engine = shared_engine().map_err(|e| wasm_error_to_provider_error(&e.to_string()))?;
        let state = self.new_host_state(None);
        let mut store = Store::new(&engine, state);
        store.set_epoch_deadline(self.call_deadline_ticks());
        let instance = self
            .instance_pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| self.record_trap(format!("instantiate: {e}"), "count_tokens"))?;
        // Reset the deadline before the wasm call — instantiation may
        // have consumed some of the budget; each guest call gets its
        // own fresh ticks-from-now budget.
        store.set_epoch_deadline(self.call_deadline_ticks());
        let wit_req = spp_wit::CountTokensRequest {
            model: req.model,
            messages: req.messages.into_iter().map(Into::into).collect(),
        };
        let result = instance
            .call_count_tokens(&mut store, &wit_req)
            .await
            .map_err(|e| self.record_trap(format!("count_tokens trap: {e}"), "count_tokens"))?;
        match result {
            Ok(resp) => Ok(CountTokensResponse {
                input_tokens: resp.input_tokens,
            }),
            Err(e) => Err(e.into()),
        }
    }
}

#[async_trait]
impl ProviderClient for WasmProviderClient {
    async fn complete(
        &self,
        req: CompleteRequest,
        events: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        self.check_disabled()?;
        let engine = shared_engine().map_err(|e| wasm_error_to_provider_error(&e.to_string()))?;
        let state = self.new_host_state(events);
        let mut store = Store::new(&engine, state);
        store.set_epoch_deadline(self.call_deadline_ticks());
        let instance = self
            .instance_pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| self.record_trap(format!("instantiate: {e}"), "complete"))?;
        store.set_epoch_deadline(self.call_deadline_ticks());
        let wit_req: spp_wit::CompleteRequest = req.into();
        let result = instance
            .call_complete(&mut store, &wit_req)
            .await
            .map_err(|e| self.record_trap(format!("complete trap: {e}"), "complete"))?;
        result.map(Into::into).map_err(Into::into)
    }

    async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
        self.check_disabled()?;
        let engine = shared_engine().map_err(|e| wasm_error_to_provider_error(&e.to_string()))?;
        let state = self.new_host_state(None);
        let mut store = Store::new(&engine, state);
        store.set_epoch_deadline(self.call_deadline_ticks());
        let instance = self
            .instance_pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| self.record_trap(format!("instantiate: {e}"), "list_models"))?;
        store.set_epoch_deadline(self.call_deadline_ticks());
        let result = instance
            .call_list_models(&mut store)
            .await
            .map_err(|e| self.record_trap(format!("list_models trap: {e}"), "list_models"))?;
        result.map(Into::into).map_err(Into::into)
    }
}

/// Free-form `count-tokens` request used by [`WasmProviderClient::count_tokens`].
///
/// `count-tokens` has no [`savvagent_protocol`] counterpart, so we
/// declare a small local type rather than dragging the WIT-level type
/// out into the public surface.
#[derive(Debug, Clone)]
pub struct CountTokensRequest {
    /// Model id the count is being computed against.
    pub model: String,
    /// Messages whose token count the plugin should compute.
    pub messages: Vec<savvagent_protocol::Message>,
}

/// Free-form `count-tokens` response. Mirrors the WIT-side record
/// field-for-field.
#[derive(Debug, Clone)]
pub struct CountTokensResponse {
    /// Total input-side token count.
    pub input_tokens: u32,
}

/// Wrap a wasmtime / instantiation / trap error string into the
/// `ProviderError` shape host code expects. We always use
/// `ErrorKind::Internal` — these failures aren't transport-layer or
/// vendor-side, they're host-side. Plugin authors see them in logs
/// regardless.
fn wasm_error_to_provider_error(msg: &str) -> ProviderError {
    ProviderError {
        kind: ErrorKind::Internal,
        message: format!("wasmtime: {msg}"),
        retry_after_ms: None,
        provider_code: None,
    }
}

/// `ProviderError` shape returned when the client is already disabled by
/// strikes. `ErrorKind::Internal` so the host's retry/fallback layer
/// (which already special-cases `Transport`) doesn't try to retry a
/// permanently-dead plugin.
fn disabled_provider_error() -> ProviderError {
    ProviderError {
        kind: ErrorKind::Internal,
        message: "wasmtime: plugin disabled by strikes (repeated wasm traps)".to_string(),
        retry_after_ms: None,
        provider_code: None,
    }
}
