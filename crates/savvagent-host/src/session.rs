//! Conversation state and the tool-use loop.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    BlockDelta, CompleteRequest, ContentBlock, Message, ProviderError, ProviderId, Role,
    StopReason, StreamEvent, ToolDef,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};

use crate::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
use crate::config::{HostConfig, ProviderEndpoint, ProviderRegistration, StartupConnectPolicy};
use crate::permissions::{
    BashNetworkChoice, BashNetworkPolicy, PermissionDecision, PermissionPolicy, Verdict,
};
use crate::pool::{DisconnectMode, PoolEntry, PoolError, ProviderLease};
use crate::project;
use crate::provider::RmcpProviderClient;
use crate::sandbox::SandboxConfig;
use crate::tools::{
    BashNetContext, BashNetResolver, BashNetResolverHandle, NetOverride, ToolRegistry,
};

/// Current transcript file schema version.
///
/// Increment when the on-disk shape changes incompatibly. The loader rejects
/// files whose `schema_version` doesn't match this constant with
/// [`TranscriptError::SchemaMismatch`], which lets callers surface a clear
/// error rather than silently misinterpreting old data.
///
/// **Pre-resume files** (written before this version field was introduced)
/// lack the `schema_version` field entirely. They are accepted as v1
/// transcripts — the only field they carry is the raw `Vec<Message>` array,
/// which is identical in shape to `TranscriptFile::messages` in v1.
pub const TRANSCRIPT_SCHEMA_VERSION: u32 = 1;

/// On-disk transcript format.
///
/// The file is pretty-printed JSON. Older files written by
/// `Host::save_transcript` before session-resume was added lack the wrapper
/// object; they are a bare `[...]` array of [`Message`]s. The loader handles
/// both shapes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptFile {
    /// Schema version; used to detect incompatible on-disk formats.
    pub schema_version: u32,
    /// Recorded model identifier. May differ from the active connection.
    pub model: String,
    /// Unix timestamp (seconds) of when the transcript was saved.
    pub saved_at: u64,
    /// Conversation messages in chronological order.
    pub messages: Vec<Message>,
}

/// Errors produced by transcript load / save operations.
#[derive(Debug, Error)]
pub enum TranscriptError {
    /// The file could not be read.
    #[error("io error reading transcript: {0}")]
    Io(#[from] std::io::Error),
    /// JSON was malformed or didn't match the expected shape.
    #[error("malformed transcript JSON: {0}")]
    Malformed(String),
    /// The on-disk schema version differs from the version this binary expects.
    #[error("transcript schema v{found}, expected v{expected}")]
    SchemaMismatch {
        /// Version found in the file.
        found: u32,
        /// Version this build expects.
        expected: u32,
    },
}

/// Reason a turn was cancelled before reaching `end_turn`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CancellationReason {
    /// The provider supplying this turn was force-disconnected mid-flight.
    ProviderDisconnected(ProviderId),
}

impl std::fmt::Display for CancellationReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CancellationReason::ProviderDisconnected(id) => {
                write!(f, "provider {} disconnected", id.as_str())
            }
        }
    }
}

/// Top-level error surfaced from [`Host`] operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HostError {
    /// Connecting to a provider or tool MCP server failed at startup.
    #[error("{0}")]
    Startup(#[from] anyhow::Error),
    /// The provider returned an SPP-level error.
    #[error("provider error: {kind:?}: {message}", kind = .0.kind, message = .0.message)]
    Provider(ProviderError),
    /// Loop ran past [`HostConfig::max_iterations`] without reaching `end_turn`.
    #[error("tool-use loop exceeded {0} iterations")]
    LoopLimit(u32),
    /// The pool has no active provider selected (e.g. pool is empty or the
    /// active provider was drained while a turn was starting).
    #[error("no active provider in pool")]
    NoActiveProvider,
    /// The turn was cancelled cooperatively (stage 1 cancel signal received).
    #[error("turn cancelled: {0}")]
    Cancelled(CancellationReason),
    /// Generic internal error not covered by the above variants.
    #[error("internal error: {0}")]
    Other(String),
    /// Tool routing produced a malformed `tool_use` block. No longer
    /// constructed — kept in the public API so external `match` arms still
    /// compile. Slated for removal in the next minor (0.15.0) version.
    #[deprecated(
        since = "0.14.2",
        note = "tool_use blocks are now authoritative; this variant is never produced and will be removed in 0.15.0"
    )]
    #[error("malformed assistant response: {0}")]
    MalformedResponse(String),
}

/// Status of one tool call inside a [`TurnOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallStatus {
    /// The tool returned successfully.
    Ok,
    /// The tool returned `is_error: true` or a transport-level error.
    Errored,
}

/// Trace of one tool call performed during a turn.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Tool name as chosen by the model.
    pub name: String,
    /// Arguments the model produced.
    pub arguments: Value,
    /// Outcome status.
    pub status: ToolCallStatus,
    /// String payload returned to the model in `tool_result`.
    pub result: String,
}

/// What [`Host::run_turn`] returns when the loop reaches `end_turn`.
#[derive(Debug, Clone)]
pub struct TurnOutcome {
    /// Concatenated assistant text from the final response.
    pub text: String,
    /// Tool calls executed during this turn, in order.
    pub tool_calls: Vec<ToolCall>,
    /// Number of provider round-trips this turn took (including the final one).
    pub iterations: u32,
}

/// Streaming event the host emits while running a turn. The TUI consumes
/// these to render incremental output.
#[derive(Debug, Clone)]
pub enum TurnEvent {
    /// The router picked a `(provider, model)` for this turn. Emitted
    /// once, before any `IterationStarted`, so the TUI can render a
    /// per-turn routing badge above the assistant's response.
    RouteSelected {
        /// The chosen provider for this turn.
        provider_id: savvagent_protocol::ProviderId,
        /// The chosen model for this turn.
        model_id: String,
        /// Why the router picked it (rendered as "Override" / "Default" today).
        reason: crate::router::RoutingReason,
    },
    /// One iteration of the loop began. `iteration` is 1-based.
    IterationStarted {
        /// 1-based iteration index.
        iteration: u32,
    },
    /// A token-delta arrived for the assistant's in-progress response.
    TextDelta {
        /// Text fragment to append to the live buffer.
        text: String,
    },
    /// The model decided to invoke a tool. Emitted *before* the tool runs.
    ToolCallStarted {
        /// Tool name as chosen by the model.
        name: String,
        /// Arguments the model produced.
        arguments: Value,
    },
    /// A tool call finished and its result was appended to history.
    ToolCallFinished {
        /// Tool name (echoed for matching with the `Started` event).
        name: String,
        /// Outcome status.
        status: ToolCallStatus,
        /// String payload that was returned to the model.
        result: String,
    },
    /// Policy returned [`Verdict::Ask`] for a tool call. The turn is paused
    /// until the embedder calls [`Host::resolve_permission`] with the matching
    /// `id`. Emitted before any `ToolCallStarted` for this call.
    PermissionRequested {
        /// Opaque request id; pass back to [`Host::resolve_permission`].
        id: u64,
        /// Tool the model wants to invoke.
        name: String,
        /// Short, human-readable summary for the modal.
        summary: String,
        /// Full argument JSON, in case the UI wants to render it expanded.
        args: Value,
    },
    /// Tool-bash is about to be spawned and the configured
    /// [`BashNetworkPolicy`] is `Ask` with no cached decision for this
    /// session. The host pauses the spawn until the embedder calls
    /// [`Host::resolve_bash_network_decision`] with `id`.
    BashNetworkRequested {
        /// Opaque request id; pass back to
        /// [`Host::resolve_bash_network_decision`].
        id: u64,
        /// Human-readable summary of the bash invocation, suitable for
        /// display in a modal (e.g., the first ~80 chars of the command).
        summary: String,
    },
    /// A tool call was refused (by policy or by the user). No `ToolCallStarted`
    /// or `ToolCallFinished` is emitted for this call — only this event plus a
    /// synthetic error `tool_result` appended to the conversation.
    ToolCallDenied {
        /// Tool name.
        name: String,
        /// Reason that's also embedded in the synthetic `tool_result`.
        reason: String,
    },
    /// The whole turn finished.
    TurnComplete {
        /// Final outcome — same value `run_turn_streaming` returns.
        outcome: TurnOutcome,
    },
    /// A cooperative cancel signal was received and acted upon. The turn
    /// did not complete normally; the in-flight `complete` future was
    /// dropped. Emitted before returning [`HostError::Cancelled`].
    Cancelled {
        /// Why the turn was cancelled.
        reason: CancellationReason,
    },
    /// The grace period expired and the task was hard-aborted.
    /// Emitted by [`Host::remove_provider`] after calling
    /// `AbortHandle::abort()` on all in-flight turn tasks for the
    /// disconnected provider.
    AbortedAfterGrace {
        /// Why the abort was triggered.
        reason: CancellationReason,
    },
}

/// The agent host. Connects once, then handles turns. `Host` is `Send + Sync`
/// behind shared state so the TUI can hand it to background tasks.
pub struct Host {
    config: HostConfig,
    /// Provider pool: keyed by [`ProviderId`], holds a [`PoolEntry`] per
    /// registered provider. Guarded by a `tokio::sync::RwLock`; guards
    /// **must never be held across an `.await` on the provider client**.
    pool: tokio::sync::RwLock<HashMap<savvagent_protocol::ProviderId, PoolEntry>>,
    /// The currently-active provider id. Turns are routed to this entry.
    active_provider: tokio::sync::RwLock<savvagent_protocol::ProviderId>,
    /// The model id forwarded in every `CompleteRequest`. Initialized from
    /// `config.model`; updated via [`Host::set_model`] when the user runs
    /// `/model <id>`. A separate lock (not a mutable `config`) so that
    /// concurrent readers of `config` remain unaffected.
    current_model: tokio::sync::RwLock<String>,
    tools: Mutex<Option<ToolRegistry>>,
    state: Mutex<SessionState>,
    system_prompt: Option<String>,
    /// Layered permission policy: sensitive-path floor + SAVVAGENT.md
    /// front-matter rules + persisted Always/Never rules from
    /// `~/.savvagent/permissions.toml` + built-in defaults. See the
    /// `permissions` module docs.
    policy: PermissionPolicy,
    /// Active Layer-3 sandbox configuration. Resolved from
    /// `HostConfig::sandbox` at startup (or loaded from disk when unset).
    sandbox: SandboxConfig,
    /// Outstanding permission requests. Inserted when the loop emits
    /// `PermissionRequested`; the matching `oneshot` is consumed by
    /// [`Host::resolve_permission`].
    pending: Mutex<HashMap<u64, oneshot::Sender<PermissionDecision>>>,
    /// Outstanding `BashNetworkRequested` prompts, keyed by event id. The
    /// matching `oneshot` is consumed by
    /// [`Host::resolve_bash_network_decision`]. `Arc`-shared so the
    /// lazy bash-net resolver closure can hold a reference too.
    pending_bash_network: Arc<Mutex<HashMap<u64, oneshot::Sender<BashNetworkChoice>>>>,
    /// Current turn's event channel, exposed so the lazy `tool-bash`
    /// spawn resolver (which is invoked from inside `run_turn_inner` via
    /// `ToolRegistry::call_with_bash_net_override`) can emit
    /// [`TurnEvent::BashNetworkRequested`] without having to take the
    /// channel through every call layer. Set at the start of each
    /// streaming turn and cleared at the end; the non-streaming code
    /// path leaves it `None`. Held behind `Arc<std::sync::Mutex<_>>`
    /// so the resolver closure can clone its handle.
    current_turn_events: Arc<std::sync::Mutex<Option<mpsc::Sender<TurnEvent>>>>,
    /// Monotonic source for permission-request ids. `Arc`-shared so the
    /// resolver closure can mint ids.
    next_request_id: Arc<AtomicU64>,
    /// Per-provider broadcast channel for cooperative cancel signals.
    /// Created lazily on first turn start for a given provider. Sending
    /// on this channel races the in-flight `complete` future in
    /// `run_turn_inner` and causes it to return
    /// [`HostError::Cancelled`] early.
    cancel_signal: tokio::sync::Mutex<HashMap<ProviderId, broadcast::Sender<CancellationReason>>>,
    /// Per-provider abort handles for currently in-flight turns. Each
    /// handle refers to the `tokio::task` spawned inside
    /// `run_turn_inner` for one provider-round-trip. Used by
    /// [`Host::remove_provider`] in the hard-abort stage.
    turn_handles: tokio::sync::Mutex<HashMap<ProviderId, Vec<tokio::task::AbortHandle>>>,
}

struct SessionState {
    messages: Vec<Message>,
}

impl Host {
    /// Connect to the configured provider and tool servers, perform any MCP
    /// handshakes, and load the project context file.
    pub async fn start(config: HostConfig) -> Result<Self, HostError> {
        // Build the pool. When `config.providers` is non-empty, populate it
        // according to `config.startup_connect`. Otherwise fall back to the
        // legacy single-provider path using the rmcp HTTP transport.
        let (pool_map, active_id) = if !config.providers.is_empty() {
            let mut map: HashMap<savvagent_protocol::ProviderId, PoolEntry> = HashMap::new();
            let should_connect: Box<dyn Fn(&savvagent_protocol::ProviderId) -> bool> =
                match &config.startup_connect {
                    StartupConnectPolicy::All => Box::new(|_| true),
                    StartupConnectPolicy::None => Box::new(|_| false),
                    StartupConnectPolicy::OptIn(allow) | StartupConnectPolicy::LastUsed(allow) => {
                        let set: std::collections::HashSet<_> = allow.iter().cloned().collect();
                        Box::new(move |id| set.contains(id))
                    }
                };
            for reg in &config.providers {
                if should_connect(&reg.id) {
                    map.insert(
                        reg.id.clone(),
                        PoolEntry::new(
                            Arc::clone(&reg.client),
                            reg.capabilities.clone(),
                            reg.aliases.clone(),
                            reg.display_name.clone(),
                        ),
                    );
                }
            }
            // Active provider = first entry that was connected; if none were
            // connected, fall back to the first registered provider id.
            let active = config
                .providers
                .iter()
                .find(|r| map.contains_key(&r.id))
                .map(|r| r.id.clone())
                .unwrap_or_else(|| config.providers[0].id.clone());
            (map, active)
        } else {
            // Legacy fallback: rmcp HTTP transport, single "default" entry.
            let provider_arc: Arc<dyn ProviderClient + Send + Sync> = match &config.provider {
                ProviderEndpoint::StreamableHttp { url } => {
                    Arc::new(RmcpProviderClient::connect(url).await?)
                }
            };
            let id =
                savvagent_protocol::ProviderId::new("default").expect("\"default\" is a valid id");
            let caps = ProviderCapabilities::new(
                vec![ModelCapabilities {
                    id: config.model.clone(),
                    display_name: config.model.clone(),
                    supports_vision: false,
                    supports_audio: false,
                    context_window: 0,
                    cost_tier: CostTier::Standard,
                }],
                config.model.clone(),
            )
            .expect("single-model caps with matching default are always valid");
            let entry = PoolEntry::new(provider_arc, caps, Vec::new(), "Default".into());
            let mut map = HashMap::new();
            map.insert(id.clone(), entry);
            (map, id)
        };

        let sandbox = config.sandbox.clone().unwrap_or_else(SandboxConfig::load);
        // The real resolver — which calls back into the host's
        // permission state — captures `&self` and so can
        // only be installed once we own the `Host`. The bootstrap resolver
        // here is a safe default that returns the per-call override (if
        // any) or falls back to `false`. `wire_self_into_resolver` swaps
        // in the real one below.
        let resolver = bootstrap_bash_net_resolver();
        let tools =
            ToolRegistry::connect(&config.tools, &config.project_root, &sandbox, resolver).await?;
        let system_prompt = build_layered_system_prompt(&config, &tools);
        let policy = config
            .policy
            .clone()
            .unwrap_or_else(|| PermissionPolicy::default_for(&config.project_root));
        // Pre-create one cancel broadcast sender per connected provider so
        // run_turn_inner can subscribe before acquiring the lease, eliminating
        // the TOCTOU window where a Force disconnect could miss the receiver.
        let cancel_signal_map: HashMap<
            savvagent_protocol::ProviderId,
            broadcast::Sender<CancellationReason>,
        > = pool_map
            .keys()
            .map(|id| (id.clone(), broadcast::channel(8).0))
            .collect();

        let initial_model = config.model.clone();
        let host = Self {
            config,
            pool: tokio::sync::RwLock::new(pool_map),
            active_provider: tokio::sync::RwLock::new(active_id),
            current_model: tokio::sync::RwLock::new(initial_model),
            tools: Mutex::new(Some(tools)),
            state: Mutex::new(SessionState {
                messages: Vec::new(),
            }),
            system_prompt,
            policy,
            sandbox,
            pending: Mutex::new(HashMap::new()),
            pending_bash_network: Arc::new(Mutex::new(HashMap::new())),
            current_turn_events: Arc::new(std::sync::Mutex::new(None)),
            next_request_id: Arc::new(AtomicU64::new(1)),
            cancel_signal: tokio::sync::Mutex::new(cancel_signal_map),
            turn_handles: tokio::sync::Mutex::new(HashMap::new()),
        };
        host.wire_self_into_resolver().await;
        Ok(host)
    }

    /// Construct a host directly from a (possibly mock) [`ProviderClient`] and
    /// a pre-connected tool registry. Used by tests and embedders that want to
    /// bypass the standard transport layer.
    ///
    /// The supplied `provider` is stored as the sole pool entry under the
    /// synthetic id `"default"`, which also becomes the active provider.
    #[doc(hidden)]
    pub async fn with_components(
        config: HostConfig,
        provider: Box<dyn ProviderClient + Send + Sync>,
    ) -> Result<Self, HostError> {
        let sandbox = config.sandbox.clone().unwrap_or_else(SandboxConfig::load);
        let resolver = bootstrap_bash_net_resolver();
        let tools =
            ToolRegistry::connect(&config.tools, &config.project_root, &sandbox, resolver).await?;
        let system_prompt = build_layered_system_prompt(&config, &tools);
        let policy = config
            .policy
            .clone()
            .unwrap_or_else(|| PermissionPolicy::default_for(&config.project_root));

        // Wrap the boxed client into an Arc-backed pool entry.
        let provider_arc: Arc<dyn ProviderClient + Send + Sync> = Arc::from(provider);
        let default_id =
            savvagent_protocol::ProviderId::new("default").expect("\"default\" is a valid id");
        let caps = ProviderCapabilities::new(
            vec![ModelCapabilities {
                id: config.model.clone(),
                display_name: config.model.clone(),
                supports_vision: false,
                supports_audio: false,
                context_window: 0,
                cost_tier: CostTier::Standard,
            }],
            config.model.clone(),
        )
        .expect("single-model caps with matching default are always valid");
        let entry = PoolEntry::new(provider_arc, caps, Vec::new(), "Default".into());
        let mut pool_map: HashMap<savvagent_protocol::ProviderId, PoolEntry> = HashMap::new();
        pool_map.insert(default_id.clone(), entry);

        // Pre-create the cancel broadcast sender for the single default
        // provider, matching the invariant established in Host::start.
        let mut cancel_signal_map: HashMap<ProviderId, broadcast::Sender<CancellationReason>> =
            HashMap::new();
        cancel_signal_map.insert(default_id.clone(), broadcast::channel(8).0);

        let initial_model = config.model.clone();
        let host = Self {
            config,
            pool: tokio::sync::RwLock::new(pool_map),
            active_provider: tokio::sync::RwLock::new(default_id),
            current_model: tokio::sync::RwLock::new(initial_model),
            tools: Mutex::new(Some(tools)),
            state: Mutex::new(SessionState {
                messages: Vec::new(),
            }),
            system_prompt,
            policy,
            sandbox,
            pending: Mutex::new(HashMap::new()),
            pending_bash_network: Arc::new(Mutex::new(HashMap::new())),
            current_turn_events: Arc::new(std::sync::Mutex::new(None)),
            next_request_id: Arc::new(AtomicU64::new(1)),
            cancel_signal: tokio::sync::Mutex::new(cancel_signal_map),
            turn_handles: tokio::sync::Mutex::new(HashMap::new()),
        };
        host.wire_self_into_resolver().await;
        Ok(host)
    }

    /// Send `user_input` as a user turn and run the tool-use loop until the
    /// model emits `end_turn` (or some other terminal stop reason). No
    /// streaming events are emitted; use [`Self::run_turn_streaming`] for the
    /// TUI's incremental-render path.
    pub async fn run_turn(&self, user_input: impl Into<String>) -> Result<TurnOutcome, HostError> {
        self.run_turn_inner(user_input.into(), None).await
    }

    /// Run a turn while emitting [`TurnEvent`]s onto `events`. Token-level
    /// `TextDelta`s are forwarded as the provider streams them. The final
    /// [`TurnOutcome`] is also returned for callers that want both.
    pub async fn run_turn_streaming(
        &self,
        user_input: impl Into<String>,
        events: mpsc::Sender<TurnEvent>,
    ) -> Result<TurnOutcome, HostError> {
        self.run_turn_inner(user_input.into(), Some(events)).await
    }

    /// Ask the active provider for its model list.
    ///
    /// Returns the provider's default error when `list_models` is not
    /// advertised; callers should treat that case as "fall through to
    /// optimistic `/model` selection".
    pub async fn list_models(
        &self,
    ) -> Result<savvagent_protocol::ListModelsResponse, savvagent_protocol::ProviderError> {
        let lease = {
            let active = self.active_provider.read().await.clone();
            let pool = self.pool.read().await;
            let Some(entry) = pool.get(&active) else {
                return Err(savvagent_protocol::ProviderError {
                    kind: savvagent_protocol::ErrorKind::Internal,
                    message: "no active provider in pool".into(),
                    retry_after_ms: None,
                    provider_code: None,
                });
            };
            entry.lease()
        };
        // Pool read guard dropped here; now safe to await.
        lease.client().list_models().await
    }

    async fn run_turn_inner(
        &self,
        user_input: String,
        events: Option<mpsc::Sender<TurnEvent>>,
    ) -> Result<TurnOutcome, HostError> {
        // Publish the events channel (if any) for the lazy bash-net
        // resolver. Clear it via a guard on exit so an early-return path
        // doesn't leak a stale Sender that outlives the turn.
        let _events_guard = CurrentTurnEventsGuard::install(&self.current_turn_events, &events);
        // Snapshot existing history and append the user message. We keep a
        // local working copy and only commit it back to `state` once the loop
        // succeeds — that way a failed turn doesn't corrupt the conversation.
        let mut messages = {
            let s = self.state.lock().await;
            s.messages.clone()
        };
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: user_input }],
        });

        let tool_defs = {
            let guard = self.tools.lock().await;
            guard.as_ref().map(|t| t.defs.clone()).unwrap_or_default()
        };

        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut iterations: u32 = 0;
        let want_stream = events.is_some();

        loop {
            if iterations >= self.config.max_iterations {
                return Err(HostError::LoopLimit(self.config.max_iterations));
            }
            iterations += 1;

            if let Some(tx) = &events {
                let _ = tx
                    .send(TurnEvent::IterationStarted {
                        iteration: iterations,
                    })
                    .await;
            }

            let req = CompleteRequest {
                model: self.current_model.read().await.clone(),
                messages: messages.clone(),
                system: self.system_prompt.clone(),
                tools: tool_defs.clone(),
                temperature: None,
                top_p: None,
                max_tokens: self.config.max_tokens,
                stop_sequences: Vec::new(),
                stream: want_stream,
                thinking: None,
                metadata: None,
            };

            // Wire token-delta forwarding only when the caller asked to
            // stream; otherwise skip the channel + task entirely.
            let (provider_tx, forwarder) = if let Some(events_tx) = events.clone() {
                let (tx, rx) = mpsc::channel::<StreamEvent>(64);
                let task = tokio::spawn(forward_text_deltas(rx, events_tx));
                (Some(tx), Some(task))
            } else {
                (None, None)
            };

            tracing::debug!(
                iteration = iterations,
                stream = want_stream,
                msg_count = messages.len(),
                "dispatching provider.complete"
            );
            // Subscribe to the cancel broadcast for the active provider BEFORE
            // acquiring a lease. This closes the TOCTOU window where a
            // concurrent remove_provider(Force) could send the cancel signal
            // between "I have a lease" and "I'm listening for cancel".
            //
            // Order:
            //   a. Snapshot active_id (brief read lock on active_provider).
            //   b. Subscribe to cancel_signal[active_id] — the Sender was
            //      pre-created by add_provider / Host::start.
            //   c. Acquire the lease (brief pool read lock, then dropped).
            //
            // If Force runs after (b), the Receiver will observe the signal
            // and cancel_rx.recv() will fire. If Force removes the pool entry
            // between (b) and (c), pool.get returns None and we return
            // NoActiveProvider — either path is correct.
            let active_id: ProviderId = self.active_provider.read().await.clone();

            let mut cancel_rx: broadcast::Receiver<CancellationReason> = {
                let map = self.cancel_signal.lock().await;
                match map.get(&active_id) {
                    Some(tx) => tx.subscribe(),
                    None => {
                        // Should not happen: add_provider always pre-creates
                        // the sender. Create a fresh one as a safety net so
                        // the turn can proceed.
                        tracing::warn!(
                            provider = %active_id.as_str(),
                            "cancel_signal entry missing at turn start; \
                             creating fallback sender"
                        );
                        broadcast::channel(8).0.subscribe()
                    }
                }
            };

            // Acquire a lease and drop the pool guard before awaiting.
            // The RwLock guard must never be held across an `.await` on
            // the provider client.
            let lease: ProviderLease = {
                let pool = self.pool.read().await;
                let Some(entry) = pool.get(&active_id) else {
                    return Err(HostError::NoActiveProvider);
                };
                entry.lease()
                // `pool` guard dropped here
            };

            // Spawn the provider call so we can race it against a cancel signal.
            let client = Arc::clone(lease.client());
            let work_handle = tokio::spawn(async move { client.complete(req, provider_tx).await });
            let abort = work_handle.abort_handle();
            {
                let mut handles = self.turn_handles.lock().await;
                handles.entry(active_id.clone()).or_default().push(abort);
            }

            // Race: provider completes normally vs. cancel signal arrives.
            // `lease` is held here so `active_turn_count` stays > 0 for the
            // duration. It drops after this block.
            let resp_result = tokio::select! {
                join_res = work_handle => {
                    match join_res {
                        Ok(r) => r,
                        Err(join_err) => {
                            if join_err.is_cancelled() {
                                // Aborted by hard-abort stage; propagate as Cancelled.
                                drop(lease);
                                return Err(HostError::Cancelled(
                                    CancellationReason::ProviderDisconnected(active_id),
                                ));
                            }
                            drop(lease);
                            return Err(HostError::Other(format!(
                                "turn task panicked: {join_err}"
                            )));
                        }
                    }
                }
                cancel_res = cancel_rx.recv() => {
                    let reason = match cancel_res {
                        Ok(r) => r,
                        // Lagged or sender dropped — treat as provider disconnected.
                        Err(_) => CancellationReason::ProviderDisconnected(active_id.clone()),
                    };
                    if let Some(tx) = &events {
                        let _ = tx
                            .send(TurnEvent::Cancelled { reason: reason.clone() })
                            .await;
                    }
                    drop(lease);
                    return Err(HostError::Cancelled(reason));
                }
            };

            // Drop the lease (decrementing active_turn_count) and clean up the
            // abort handle we registered for this iteration.
            drop(lease);
            {
                let mut handles = self.turn_handles.lock().await;
                if let Some(vec) = handles.get_mut(&active_id) {
                    vec.retain(|h| !h.is_finished());
                    if vec.is_empty() {
                        handles.remove(&active_id);
                    }
                }
            }

            // Drop the sender side (if any) so the forwarder drains and exits.
            if let Some(task) = forwarder {
                let _ = task.await;
            }
            let resp = resp_result.map_err(HostError::Provider)?;
            tracing::debug!(
                iteration = iterations,
                stop_reason = ?resp.stop_reason,
                blocks = resp.content.len(),
                "provider.complete returned"
            );

            // Append the assistant turn verbatim — the provider's content
            // blocks (including tool_use ids) round-trip back unchanged.
            messages.push(Message {
                role: Role::Assistant,
                content: resp.content.clone(),
            });

            let mut tool_uses: Vec<(String, String, Value)> = Vec::new();
            let mut text_buf = String::new();
            for block in &resp.content {
                match block {
                    ContentBlock::Text { text } => {
                        if !text_buf.is_empty() {
                            text_buf.push('\n');
                        }
                        text_buf.push_str(text);
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_uses.push((id.clone(), name.clone(), input.clone()));
                    }
                    _ => {}
                }
            }

            // Tool-use blocks drive continuation. Providers disagree on how
            // they signal tool use through `stop_reason` (Anthropic emits
            // `tool_use`; Gemini happily emits `end_turn` alongside a
            // functionCall part), so the actual content blocks are the only
            // reliable signal. If any `ToolUse` blocks are present, run them
            // and loop again, regardless of `stop_reason`.
            if tool_uses.is_empty() {
                if !matches!(resp.stop_reason, StopReason::EndTurn) {
                    // Anomalous terminations (MaxTokens cut-off, Refusal,
                    // StopSequence, Other) currently collapse into the same
                    // success path as EndTurn — but they aren't noise: a
                    // MaxTokens truncation can corrupt the assistant turn
                    // we are about to commit to state. Surface it as a warn
                    // so it shows up at default log levels.
                    tracing::warn!(
                        stop_reason = ?resp.stop_reason,
                        "terminating turn with non-end_turn stop_reason and no tool_use blocks"
                    );
                }
                let outcome = TurnOutcome {
                    text: text_buf,
                    tool_calls,
                    iterations,
                };
                {
                    let mut state = self.state.lock().await;
                    state.messages = messages;
                }
                if let Some(tx) = events {
                    let _ = tx
                        .send(TurnEvent::TurnComplete {
                            outcome: outcome.clone(),
                        })
                        .await;
                }
                return Ok(outcome);
            }

            // Execute every requested tool call and append a single user
            // turn of tool_result blocks (Anthropic's expected shape).
            let mut tool_results: Vec<ContentBlock> = Vec::with_capacity(tool_uses.len());
            for (tool_use_id, name, input) in tool_uses {
                let gate = self.gate_tool_call(&name, &input, events.as_ref()).await;
                match gate {
                    Ok(()) => {
                        if let Some(tx) = &events {
                            let _ = tx
                                .send(TurnEvent::ToolCallStarted {
                                    name: name.clone(),
                                    arguments: input.clone(),
                                })
                                .await;
                        }
                        let outcome = {
                            let guard = self.tools.lock().await;
                            let registry = guard.as_ref().expect("tools registry present");
                            registry
                                .call_with_bash_net_override(
                                    &name,
                                    input.clone(),
                                    NetOverride::Inherit,
                                )
                                .await
                        };
                        let status = if outcome.is_error {
                            ToolCallStatus::Errored
                        } else {
                            ToolCallStatus::Ok
                        };
                        if let Some(tx) = &events {
                            let _ = tx
                                .send(TurnEvent::ToolCallFinished {
                                    name: name.clone(),
                                    status,
                                    result: outcome.payload.clone(),
                                })
                                .await;
                        }
                        tool_results.push(ContentBlock::ToolResult {
                            tool_use_id,
                            content: vec![ContentBlock::Text {
                                text: outcome.payload.clone(),
                            }],
                            is_error: outcome.is_error,
                        });
                        tool_calls.push(ToolCall {
                            name,
                            arguments: input,
                            status,
                            result: outcome.payload,
                        });
                    }
                    Err(reason) => {
                        if let Some(tx) = &events {
                            let _ = tx
                                .send(TurnEvent::ToolCallDenied {
                                    name: name.clone(),
                                    reason: reason.clone(),
                                })
                                .await;
                        }
                        let payload = format!("denied by policy: {reason}");
                        tool_results.push(ContentBlock::ToolResult {
                            tool_use_id,
                            content: vec![ContentBlock::Text {
                                text: payload.clone(),
                            }],
                            is_error: true,
                        });
                        tool_calls.push(ToolCall {
                            name,
                            arguments: input,
                            status: ToolCallStatus::Errored,
                            result: payload,
                        });
                    }
                }
            }
            messages.push(Message {
                role: Role::User,
                content: tool_results,
            });
        }
    }

    /// Read-only snapshot of the conversation history.
    pub async fn messages(&self) -> Vec<Message> {
        self.state.lock().await.messages.clone()
    }

    /// Persist the current message history as pretty-printed JSON to `path`.
    ///
    /// Creates parent directories as needed. The file is written in the
    /// [`TranscriptFile`] versioned format so [`Self::load_transcript`] can
    /// round-trip it unambiguously.
    pub async fn save_transcript(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let messages = self.messages().await;
        let saved_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let record = TranscriptFile {
            schema_version: TRANSCRIPT_SCHEMA_VERSION,
            model: self.current_model.read().await.clone(),
            saved_at,
            messages,
        };
        let json = serde_json::to_vec_pretty(&record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        tokio::fs::write(path, json).await
    }

    /// Re-hydrate the message history from a previously-saved transcript file.
    ///
    /// Accepts two on-disk shapes:
    ///
    /// - **Versioned** (`TranscriptFile` object with a `schema_version` field):
    ///   the schema version must equal [`TRANSCRIPT_SCHEMA_VERSION`]; a mismatch
    ///   returns [`TranscriptError::SchemaMismatch`].
    /// - **Legacy bare array** (`[...]`): files written before session-resume
    ///   was introduced. They are interpreted as v1 transcripts; the messages
    ///   are loaded as-is.
    ///
    /// Existing conversation history is **replaced** by the loaded messages.
    /// The caller is expected to have verified that a provider is connected
    /// before calling this; the host does not re-connect automatically.
    ///
    /// Returns the loaded [`TranscriptFile`] so callers can surface metadata
    /// (model, saved_at) in the UI.
    ///
    /// # Invariant: must not be called during an in-flight turn
    ///
    /// [`Self::run_turn_inner`] snapshots `state.messages` into a local `Vec`
    /// at turn start and commits that local clone back to `state.messages`
    /// when the turn completes. Calling `load_transcript` while a turn is
    /// running therefore appears to succeed, but the in-flight turn will
    /// silently overwrite the resumed history at the moment it finishes.
    /// The TUI enforces this by gating `/resume` on its `is_loading` flag.
    pub async fn load_transcript(&self, path: &Path) -> Result<TranscriptFile, TranscriptError> {
        let bytes = tokio::fs::read(path).await?;
        let root: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| TranscriptError::Malformed(e.to_string()))?;

        let transcript = match &root {
            // Versioned format: top-level object with schema_version.
            serde_json::Value::Object(map) if map.contains_key("schema_version") => {
                let record: TranscriptFile = serde_json::from_value(root)
                    .map_err(|e| TranscriptError::Malformed(e.to_string()))?;
                if record.schema_version != TRANSCRIPT_SCHEMA_VERSION {
                    return Err(TranscriptError::SchemaMismatch {
                        found: record.schema_version,
                        expected: TRANSCRIPT_SCHEMA_VERSION,
                    });
                }
                record
            }
            // Legacy bare array: treat as v1, synthesize metadata.
            serde_json::Value::Array(_) => {
                let messages: Vec<Message> = serde_json::from_value(root)
                    .map_err(|e| TranscriptError::Malformed(e.to_string()))?;
                TranscriptFile {
                    schema_version: TRANSCRIPT_SCHEMA_VERSION,
                    model: self.current_model.read().await.clone(),
                    saved_at: 0,
                    messages,
                }
            }
            _ => {
                return Err(TranscriptError::Malformed(
                    "expected a JSON object or array at the root".into(),
                ));
            }
        };

        {
            let mut state = self.state.lock().await;
            state.messages = transcript.messages.clone();
        }
        Ok(transcript)
    }

    /// Drop the per-turn message history without touching connections.
    pub async fn clear_history(&self) {
        self.state.lock().await.messages.clear();
    }

    /// Borrow the active config.
    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    /// Borrow the active sandbox configuration.
    pub fn sandbox_config(&self) -> &SandboxConfig {
        &self.sandbox
    }

    /// Resolve a previously-emitted [`TurnEvent::PermissionRequested`].
    ///
    /// Called by the embedder (TUI) once the user picks Allow / Deny in the
    /// modal. A no-op if `id` is unknown — that handles double-resolves and
    /// races where the turn was cancelled before the user answered.
    pub async fn resolve_permission(&self, id: u64, decision: PermissionDecision) {
        let sender = self.pending.lock().await.remove(&id);
        if let Some(tx) = sender {
            let _ = tx.send(decision);
        }
    }

    /// Resolve a previously-emitted
    /// [`TurnEvent::BashNetworkRequested`]. The embedder (TUI) calls this
    /// after the user picks Once / AlwaysThisSession / DenyOnce /
    /// DenyAlways from the modal. The corresponding spawn resumes.
    ///
    /// A no-op if `id` is unknown — that handles double-resolves and
    /// races where the spawn was cancelled before the user answered.
    pub async fn resolve_bash_network_decision(&self, id: u64, choice: BashNetworkChoice) {
        let tx = self.pending_bash_network.lock().await.remove(&id);
        if let Some(tx) = tx {
            let _ = tx.send(choice);
        }
    }

    /// Run a single shell command via `tool-bash`. Used by the TUI's
    /// `/bash` slash command so a user can run a shell command without
    /// round-tripping through the provider.
    ///
    /// Returns `Err("tool registry unavailable")` if the host has been
    /// shut down. If `tool-bash` is not configured on this host, returns
    /// `Ok((true, "unknown tool: run"))` from the tool dispatch layer.
    ///
    /// `net_override` — per-call sandbox network preference. See
    /// [`NetOverride`] for the 3-state semantics.
    ///
    /// - [`NetOverride::Inherit`] — defer to the configured bash-network
    ///   policy (default: `Ask`). May park on a user prompt.
    /// - [`NetOverride::ForceAllow`] — force `allow_net = true` for this
    ///   call only.
    /// - [`NetOverride::ForceDeny`] — force `allow_net = false` for this
    ///   call only.
    ///
    /// Per-call overrides do not mutate the session decision cache.
    ///
    /// `events` — channel to receive
    /// [`TurnEvent::BashNetworkRequested`] events during the call.
    /// Required when the policy is `Ask` and no decision is cached,
    /// otherwise the resolver collapses to deny.
    pub async fn run_bash_command(
        &self,
        command: &str,
        net_override: NetOverride,
        events: Option<mpsc::Sender<TurnEvent>>,
    ) -> Result<(bool, String), String> {
        let _events_guard = CurrentTurnEventsGuard::install(&self.current_turn_events, &events);
        let input = serde_json::json!({ "command": command });
        let guard = self.tools.lock().await;
        let registry = guard
            .as_ref()
            .ok_or_else(|| "tool registry unavailable".to_string())?;
        let outcome = registry
            .call_with_bash_net_override("run", input, net_override)
            .await;
        Ok((outcome.is_error, outcome.payload))
    }

    /// Install the real bash-net resolver into the tool registry. The
    /// resolver captures `Arc`-shared handles to the permission policy,
    /// pending-prompt map, current-turn events, and request-id counter
    /// — exactly the state [`resolve_bash_network_with_state`] needs
    /// — so the closure can emit `BashNetworkRequested` and await the
    /// user's answer without holding a reference to `Host` itself.
    ///
    /// Idempotent — replacing the resolver multiple times is fine; the
    /// `ToolRegistry`'s lazy-bash slot reads it under a lock.
    async fn wire_self_into_resolver(&self) {
        let resolver: BashNetResolverHandle = Arc::new(HostBashNetResolver {
            policy: self.policy.clone(),
            pending: self.pending_bash_network.clone(),
            next_id: self.next_request_id.clone(),
            current_events: self.current_turn_events.clone(),
        });
        let guard = self.tools.lock().await;
        if let Some(reg) = guard.as_ref() {
            reg.install_bash_net_resolver(resolver);
        }
    }

    /// Persist a user-recorded Always/Never decision via the policy.
    ///
    /// Builds an [`crate::permissions::ArgPattern`] from `(tool_name, args)`
    /// and writes through to `~/.savvagent/permissions.toml`. Subsequent
    /// matching calls — in this session and future ones — short-circuit
    /// the modal. I/O errors are logged but not surfaced; the in-memory
    /// rule update still wins immediately.
    pub async fn add_session_rule(
        &self,
        tool_name: &str,
        args: &Value,
        decision: PermissionDecision,
    ) {
        if let Err(e) = self.policy.add_rule(tool_name, args, decision).await {
            tracing::warn!(
                tool = tool_name,
                error = %e,
                "failed to persist permission rule to ~/.savvagent/permissions.toml",
            );
        }
    }

    /// Snapshot of every tool advertised by the connected tool servers, in
    /// the order they were registered. Used by the TUI's `/tools` command.
    pub async fn tool_defs(&self) -> Vec<ToolDef> {
        let guard = self.tools.lock().await;
        guard.as_ref().map(|t| t.defs.clone()).unwrap_or_default()
    }

    /// What the policy would return for `tool_name` with empty arguments —
    /// a coarse "verdict at a glance" used by the TUI's `/tools` listing.
    /// Path-conditional verdicts (e.g. `write_file`) collapse to the
    /// no-path branch, which is intentionally the more conservative choice.
    pub fn default_verdict_for(&self, tool_name: &str) -> Verdict {
        self.policy
            .evaluate(tool_name, &Value::Object(serde_json::Map::new()))
    }

    /// Run policy against `(name, input)` and either return `Ok(())` (caller
    /// proceeds with the call) or `Err(reason)` (caller synthesizes a denied
    /// `tool_result`). For [`Verdict::Ask`], emits a
    /// [`TurnEvent::PermissionRequested`] and awaits the matching
    /// [`Self::resolve_permission`] via a oneshot.
    async fn gate_tool_call(
        &self,
        name: &str,
        input: &Value,
        events: Option<&mpsc::Sender<TurnEvent>>,
    ) -> Result<(), String> {
        match self.policy.evaluate(name, input) {
            Verdict::Allow => Ok(()),
            Verdict::Deny { reason } => Err(reason),
            Verdict::Ask { summary } => {
                let Some(tx) = events else {
                    // No interactive surface — Ask collapses to Deny so the
                    // model gets a clear "non-interactive turn" tool_result
                    // instead of hanging on a oneshot that nobody resolves.
                    return Err("non-interactive turn".into());
                };
                let req_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
                let (resolve_tx, resolve_rx) = oneshot::channel();
                self.pending.lock().await.insert(req_id, resolve_tx);
                let send_result = tx
                    .send(TurnEvent::PermissionRequested {
                        id: req_id,
                        name: name.to_string(),
                        summary,
                        args: input.clone(),
                    })
                    .await;
                if send_result.is_err() {
                    self.pending.lock().await.remove(&req_id);
                    return Err("event channel closed before permission could be requested".into());
                }
                match resolve_rx.await {
                    Ok(PermissionDecision::Allow) => Ok(()),
                    Ok(PermissionDecision::Deny) => Err("denied by user".into()),
                    Err(_) => Err("permission channel dropped".into()),
                }
            }
        }
    }

    /// Cleanly shut down tool-server children. The provider session is
    /// dropped along with `self`. Idempotent — calling twice is a no-op for
    /// the second call.
    pub async fn shutdown(&self) {
        let registry = {
            let mut guard = self.tools.lock().await;
            guard.take()
        };
        if let Some(r) = registry {
            r.shutdown().await;
        }
    }

    /// The currently-active provider id. Turns are routed to this entry.
    pub async fn active_provider(&self) -> savvagent_protocol::ProviderId {
        self.active_provider.read().await.clone()
    }

    /// Snapshot of the active provider's capabilities. Returns `None` if
    /// the pool is empty or the active provider isn't in the pool (which
    /// would be a bug). Clones the [`ProviderCapabilities`] so the caller
    /// doesn't hold a pool lock.
    pub async fn active_capabilities(&self) -> Option<ProviderCapabilities> {
        let active = self.active_provider.read().await.clone();
        let pool = self.pool.read().await;
        pool.get(&active).map(|entry| entry.capabilities().clone())
    }

    /// Update the model id forwarded in every subsequent `CompleteRequest`.
    ///
    /// This is the pool-safe alternative to rebuilding the host: the pool
    /// itself is untouched; only the model field sent to the provider changes.
    /// The caller is responsible for persisting the choice to
    /// `~/.savvagent/models.toml` via `models_pref::save_for_provider`.
    pub async fn set_model(&self, model: String) {
        *self.current_model.write().await = model;
    }

    /// Whether `id` is currently registered (and connected) in the pool.
    pub async fn is_connected(&self, id: &str) -> bool {
        let Ok(pid) = savvagent_protocol::ProviderId::new(id) else {
            return false;
        };
        self.pool.read().await.contains_key(&pid)
    }

    /// Register a new provider in the pool.
    ///
    /// Returns [`PoolError::AlreadyRegistered`] if a provider with the same
    /// id is already present.
    pub async fn add_provider(&self, reg: ProviderRegistration) -> Result<(), PoolError> {
        let mut pool = self.pool.write().await;
        if pool.contains_key(&reg.id) {
            return Err(PoolError::AlreadyRegistered(reg.id));
        }
        pool.insert(
            reg.id.clone(),
            PoolEntry::new(reg.client, reg.capabilities, reg.aliases, reg.display_name),
        );
        // Pre-create the cancel broadcast sender so run_turn_inner can
        // subscribe before acquiring a lease, closing the TOCTOU window.
        // The pool write lock is still held here; release it first so the
        // cancel_signal lock order stays consistent (cancel_signal is always
        // locked after any pool lock drops).
        drop(pool);
        self.cancel_signal
            .lock()
            .await
            .entry(reg.id)
            .or_insert_with(|| broadcast::channel(8).0);
        Ok(())
    }

    /// Remove a provider from the pool.
    ///
    /// `DisconnectMode::Drain` — removes the entry from the eligibility set
    /// immediately, then waits for all outstanding [`ProviderLease`]s to drop
    /// before returning. This lets in-flight turns finish before the entry is
    /// discarded.
    ///
    /// `DisconnectMode::Force` — 3-stage cancellation:
    /// 1. Sends a cooperative cancel signal to all in-flight turns on this
    ///    provider; each turn's `select!` will observe it and return
    ///    [`HostError::Cancelled`].
    /// 2. Waits up to `HostConfig::force_disconnect_grace_ms` for
    ///    `active_turn_count` to reach zero.
    /// 3. If time expires, calls `AbortHandle::abort()` on every registered
    ///    in-flight task and emits [`TurnEvent::AbortedAfterGrace`] on the
    ///    current turn's event channel.
    ///
    /// Returns [`PoolError::NotRegistered`] if `id` is not in the pool.
    pub async fn remove_provider(
        &self,
        id: &savvagent_protocol::ProviderId,
        mode: DisconnectMode,
    ) -> Result<(), PoolError> {
        // Remove the entry from the pool immediately so new turns can't
        // acquire it, then handle outstanding leases according to `mode`.
        let entry = {
            let mut pool = self.pool.write().await;
            pool.remove(id)
                .ok_or_else(|| PoolError::NotRegistered(id.clone()))?
            // Write guard dropped here.
        };

        match mode {
            DisconnectMode::Drain => {
                // Poll until all leases are released. Each drop() on a
                // ProviderLease decrements the counter; we spin with a short
                // sleep to avoid busy-waiting while holding no locks.
                while entry.active_turn_count() > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            }
            DisconnectMode::Force => {
                let reason = CancellationReason::ProviderDisconnected(id.clone());

                // Stage 1: cooperative cancel — broadcast the signal so any
                // in-flight `run_turn_inner` `select!` can observe it and
                // return early without waiting for the provider response.
                {
                    let map = self.cancel_signal.lock().await;
                    if let Some(tx) = map.get(id) {
                        let _ = tx.send(reason.clone());
                    }
                }

                // Stage 2: bounded grace — wait for active_turn_count to hit
                // zero or for the deadline to expire.
                let grace = std::time::Duration::from_millis(self.config.force_disconnect_grace_ms);
                let deadline = tokio::time::Instant::now() + grace;
                loop {
                    if entry.active_turn_count() == 0 {
                        break;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        // Stage 3: hard abort — abort every registered task
                        // for this provider.
                        let mut handles = self.turn_handles.lock().await;
                        if let Some(hs) = handles.remove(id) {
                            for h in hs {
                                h.abort();
                            }
                        }
                        drop(handles);

                        // Emit AbortedAfterGrace on the current turn's event
                        // channel so the TUI can surface it. The channel is
                        // held behind a std::sync::Mutex. If the mutex is
                        // poisoned (a prior panic held the lock) we skip the
                        // emit and log a warning — the hard abort still
                        // proceeds; only the event surface is lost.
                        match self.current_turn_events.lock() {
                            Ok(guard) => {
                                if let Some(tx) = guard.as_ref() {
                                    let _ = tx.try_send(TurnEvent::AbortedAfterGrace {
                                        reason: reason.clone(),
                                    });
                                }
                            }
                            Err(_) => {
                                tracing::warn!(
                                    "current_turn_events mutex poisoned; \
                                     skipping AbortedAfterGrace emit"
                                );
                            }
                        }
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        }

        // Remove the cancel_signal entry for this provider. This must come
        // after any Stage-1 send() (Force mode) so the broadcast has already
        // been dispatched before we drop the Sender. Both Drain and Force
        // paths converge here, so the map is cleaned up in both cases and
        // does not grow unboundedly across connect/disconnect cycles.
        self.cancel_signal.lock().await.remove(id);

        drop(entry);
        Ok(())
    }

    /// Switch the active provider. Clears conversation history first so the
    /// new provider starts on a clean session.
    ///
    /// Returns [`PoolError::NotRegistered`] if `id` is not in the pool.
    pub async fn set_active_provider(&self, id: &ProviderId) -> Result<(), PoolError> {
        // Validate first, before mutating any state.
        {
            let pool = self.pool.read().await;
            if !pool.contains_key(id) {
                return Err(PoolError::NotRegistered(id.clone()));
            }
        }
        // Order matters: clear history first (so the new provider sees a
        // clean session), then swap active.
        self.clear_history().await;
        *self.active_provider.write().await = id.clone();
        Ok(())
    }

    /// Acquire a lease on the named provider without going through
    /// `run_turn`. Used by integration tests to simulate an in-flight turn
    /// and verify drain-mode semantics.
    #[doc(hidden)]
    pub async fn acquire_lease_for_test(
        &self,
        id: &savvagent_protocol::ProviderId,
    ) -> Result<ProviderLease, PoolError> {
        let pool = self.pool.read().await;
        let entry = pool
            .get(id)
            .ok_or_else(|| PoolError::NotRegistered(id.clone()))?;
        Ok(entry.lease())
    }
}

// The pool and active_provider are wrapped in `tokio::sync::RwLock` which
// is `Send + Sync`. `PoolEntry` holds `Arc<dyn ProviderClient + Send + Sync>`
// which is also `Send + Sync`. The rest of `Host` is too.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Host>();
};

/// RAII guard that publishes the per-turn events `Sender` to
/// `Host::current_turn_events` for the lifetime of a turn, and clears
/// it on drop. Lets the lazy bash-net resolver closure pick up the
/// channel without having to thread it through every call layer.
struct CurrentTurnEventsGuard<'a> {
    slot: &'a std::sync::Mutex<Option<mpsc::Sender<TurnEvent>>>,
}

impl<'a> CurrentTurnEventsGuard<'a> {
    fn install(
        slot: &'a std::sync::Mutex<Option<mpsc::Sender<TurnEvent>>>,
        events: &Option<mpsc::Sender<TurnEvent>>,
    ) -> Self {
        match slot.lock() {
            Ok(mut guard) => *guard = events.clone(),
            Err(_) => {
                tracing::warn!(
                    "current_turn_events mutex poisoned; \
                     skipping per-turn event channel install"
                );
            }
        }
        Self { slot }
    }
}

impl Drop for CurrentTurnEventsGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut g) = self.slot.lock() {
            *g = None;
        }
    }
}

/// Summary text shown to the user when a `tool-bash` spawn requests
/// network access. Surfaced via [`TurnEvent::BashNetworkRequested`];
/// consumed by the TUI's bash-network modal. Public so test fixtures and
/// the production path share a single string.
pub const BASH_NETWORK_PROMPT_SUMMARY: &str = "tool-bash spawn requests network access";

/// Bootstrap resolver used while the registry is being constructed —
/// before we have an `Arc<Host>` we can install a resolver that calls
/// back into the host's permission state. Its `resolve_policy` returns
/// `false` (deny). The real resolver is installed in
/// [`Host::wire_self_into_resolver`] right after `Host` construction —
/// at which point the trait's default `resolve` takes over, so explicit
/// overrides short-circuit normally.
struct BootstrapBashNetResolver;

#[async_trait::async_trait]
impl BashNetResolver for BootstrapBashNetResolver {
    async fn resolve_policy(&self, _context: BashNetContext<'_>) -> bool {
        false
    }
}

fn bootstrap_bash_net_resolver() -> BashNetResolverHandle {
    std::sync::Arc::new(BootstrapBashNetResolver)
}

/// Build the three-layer system prompt for a freshly-connected host:
/// default-prompt (optional) → embedder override (optional) →
/// `SAVVAGENT.md` body (optional). The default-prompt layer reads
/// `tools.bash_available()` so the rendered shell-capability paragraph
/// reflects what the host actually wired.
fn build_layered_system_prompt(config: &HostConfig, tools: &ToolRegistry) -> Option<String> {
    let default_prompt_text = if config.default_prompt_enabled {
        let app_version = match config.app_version.as_deref() {
            Some(v) => crate::default_prompt::AppVersion::App(v),
            None => crate::default_prompt::AppVersion::HostCrateFallback,
        };
        let env = crate::default_prompt::PromptEnv::probe(
            &config.project_root,
            std::env::consts::OS,
            std::env::consts::ARCH,
            tools.bash_available(),
            app_version,
        );
        Some(crate::default_prompt::build(&env, &tools.defs))
    } else {
        None
    };
    let savvagent_md_body = project::parse_savvagent_md(&config.project_root).body;
    project::layered_prompt(
        default_prompt_text.as_deref(),
        config.system_prompt.as_deref(),
        savvagent_md_body.as_deref(),
    )
}

/// Production resolver installed by [`Host::wire_self_into_resolver`]
/// after `Host` construction. Holds `Arc`-shared handles into the host's
/// permission state so `resolve_policy` can emit a
/// [`TurnEvent::BashNetworkRequested`] event and await the user's answer.
struct HostBashNetResolver {
    policy: PermissionPolicy,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<BashNetworkChoice>>>>,
    next_id: Arc<AtomicU64>,
    current_events: Arc<std::sync::Mutex<Option<mpsc::Sender<TurnEvent>>>>,
}

#[async_trait::async_trait]
impl BashNetResolver for HostBashNetResolver {
    async fn resolve_policy(&self, context: BashNetContext<'_>) -> bool {
        let events = match self.current_events.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => {
                tracing::warn!(
                    "current_turn_events mutex poisoned; \
                     bash-net resolver returning None for events sender"
                );
                None
            }
        };
        let summary = format_bash_network_prompt_summary(context.command);
        match resolve_bash_network_with_state(
            &self.policy,
            &self.pending,
            &self.next_id,
            events.as_ref(),
            summary,
        )
        .await
        {
            Ok(v) => v,
            // `tracing::error!` — not `warn!`. A resolver failure silently
            // downgrades the user's bash-network preference to deny, and
            // the TUI surface for that today is "bash spawn fails with no
            // visible reason". Future work: surface this on the per-turn
            // events channel so the TUI can show it as a notice.
            Err(e) => {
                tracing::error!("tool-bash net resolver failed: {e}. Defaulting to deny.");
                false
            }
        }
    }
}

/// Maximum length of a bash command included in the network-prompt
/// summary. Commands longer than this are truncated with an ellipsis so
/// the modal stays readable on narrow terminals.
const BASH_PROMPT_COMMAND_TRUNCATE: usize = 80;

/// Compose the prompt summary text shown in the bash-network modal.
/// When the call provided a command, embed a truncated copy on a second
/// line so the user sees *what* is being asked about rather than only
/// the generic [`BASH_NETWORK_PROMPT_SUMMARY`] line.
fn format_bash_network_prompt_summary(command: Option<&str>) -> String {
    match command {
        Some(cmd) => {
            let truncated = if cmd.chars().count() > BASH_PROMPT_COMMAND_TRUNCATE {
                let head: String = cmd.chars().take(BASH_PROMPT_COMMAND_TRUNCATE - 1).collect();
                format!("{head}…")
            } else {
                cmd.to_string()
            };
            format!("{BASH_NETWORK_PROMPT_SUMMARY}\n  $ {truncated}")
        }
        None => BASH_NETWORK_PROMPT_SUMMARY.to_string(),
    }
}

/// Reasons the lazy bash-network resolver can fail to produce a
/// decision. Each variant short-circuits the lazy spawn to "deny"
/// (via the resolver closure installed on the tool registry) and is
/// logged so an operator can trace why the bash spawn never reached
/// the user.
#[derive(Debug, thiserror::Error)]
pub enum BashNetResolveError {
    /// `policy = Ask` and the bash spawn was triggered with no
    /// per-turn events channel installed — there's no surface to
    /// prompt the user on.
    #[error("no event channel — running outside a turn")]
    NoEvents,
    /// The [`TurnEvent::BashNetworkRequested`] send failed because the
    /// receiver was already dropped (turn ended / TUI shut down
    /// between gate decision and send).
    #[error("event channel closed before the prompt could be sent")]
    EventChannelClosed,
    /// The `oneshot::Sender` paired with the pending prompt id was
    /// dropped without sending a choice — typically because the host
    /// was shut down while the modal was up.
    #[error("user prompt cancelled (oneshot dropped)")]
    PromptCancelled,
}

/// Shared resolver-state logic. Pure-function over its inputs so the
/// lazy-bash resolver closure (which can't borrow `&self`) and any
/// future direct callers can both use it. Emits
/// [`TurnEvent::BashNetworkRequested`] on the supplied channel when
/// the policy is `Ask` and no cached decision exists, awaits the
/// matching [`Host::resolve_bash_network_decision`] call, and
/// returns the resolved `allow_net`.
async fn resolve_bash_network_with_state(
    policy: &PermissionPolicy,
    pending: &Mutex<HashMap<u64, oneshot::Sender<BashNetworkChoice>>>,
    next_request_id: &AtomicU64,
    events: Option<&mpsc::Sender<TurnEvent>>,
    summary: String,
) -> Result<bool, BashNetResolveError> {
    match policy.bash_network() {
        BashNetworkPolicy::Always => Ok(true),
        BashNetworkPolicy::Never => Ok(false),
        BashNetworkPolicy::Ask => {
            if let Some(cached) = policy.bash_network_cached() {
                return Ok(cached);
            }
            // No cache — emit a prompt and await.
            let Some(events) = events else {
                // No interactive surface — collapse to deny so the
                // spawn doesn't hang on a oneshot that nobody resolves.
                tracing::warn!(
                    "tool-bash net resolver: no event channel — running outside a turn; \
                     defaulting to deny"
                );
                return Err(BashNetResolveError::NoEvents);
            };
            let id = next_request_id.fetch_add(1, Ordering::Relaxed);
            let (tx, rx) = oneshot::channel();
            pending.lock().await.insert(id, tx);
            if events
                .send(TurnEvent::BashNetworkRequested {
                    id,
                    summary: summary.clone(),
                })
                .await
                .is_err()
            {
                pending.lock().await.remove(&id);
                tracing::warn!(
                    id,
                    summary = %summary,
                    "tool-bash net resolver: event channel closed before prompt could be sent",
                );
                return Err(BashNetResolveError::EventChannelClosed);
            }
            let choice = rx.await.map_err(|_| {
                tracing::warn!(
                    id,
                    summary = %summary,
                    "tool-bash net resolver: prompt cancelled (oneshot dropped)",
                );
                BashNetResolveError::PromptCancelled
            })?;
            // Update cache via the same sync resolver — pass a closure
            // that returns the choice we already have.
            let allow = policy.resolve_bash_network(|| choice);
            Ok(allow)
        }
    }
}

/// Convert a stream of provider [`StreamEvent`]s into [`TurnEvent::TextDelta`]s
/// and forward them to the host caller. Non-text events are dropped (they're
/// re-derivable from the final response, which the loop already has).
async fn forward_text_deltas(mut rx: mpsc::Receiver<StreamEvent>, out: mpsc::Sender<TurnEvent>) {
    while let Some(ev) = rx.recv().await {
        if let StreamEvent::ContentBlockDelta {
            delta: BlockDelta::TextDelta { text },
            ..
        } = ev
        {
            if out.send(TurnEvent::TextDelta { text }).await.is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod policy_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use async_trait::async_trait;
    use savvagent_mcp::ProviderClient;
    use savvagent_protocol::{
        CompleteRequest, CompleteResponse, ContentBlock, ProviderError, Role, StopReason,
        StreamEvent, Usage,
    };
    use serde_json::json;
    use tokio::sync::mpsc;

    use super::*;
    use crate::config::{HostConfig, ProviderEndpoint};

    /// Mock provider that on the first `complete` returns one `tool_use` and
    /// on every subsequent call returns `end_turn`. The first response asks
    /// for `tool_name` with the supplied JSON `tool_args`.
    struct ScriptedProvider {
        calls: AtomicUsize,
        tool_name: String,
        tool_args: Value,
    }

    impl ScriptedProvider {
        fn new(tool_name: impl Into<String>, tool_args: Value) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                tool_name: tool_name.into(),
                tool_args,
            }
        }
    }

    #[async_trait]
    impl ProviderClient for ScriptedProvider {
        async fn complete(
            &self,
            req: CompleteRequest,
            _events: Option<mpsc::Sender<StreamEvent>>,
        ) -> Result<CompleteResponse, ProviderError> {
            let n = self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            let (content, stop_reason) = if n == 0 {
                (
                    vec![ContentBlock::ToolUse {
                        id: "tu_1".into(),
                        name: self.tool_name.clone(),
                        input: self.tool_args.clone(),
                    }],
                    StopReason::ToolUse,
                )
            } else {
                (
                    vec![ContentBlock::Text {
                        text: "done".into(),
                    }],
                    StopReason::EndTurn,
                )
            };
            Ok(CompleteResponse {
                id: format!("resp-{n}"),
                model: req.model,
                content,
                stop_reason,
                stop_sequence: None,
                usage: Usage::default(),
            })
        }
    }

    fn config_no_tools() -> HostConfig {
        let project_root = std::env::temp_dir().join("savvagent-policy-test");
        HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "test-model".to_string(),
        )
        .with_project_root(project_root.clone())
        // Use a transient (in-memory only) policy so tests don't touch the
        // real `~/.savvagent/permissions.toml`.
        .with_policy(PermissionPolicy::transient(project_root))
    }

    /// `read_file` against `.env` is a default-Deny — host must synthesize an
    /// error tool_result and emit `ToolCallDenied`, never touching the
    /// registry.
    #[tokio::test]
    async fn deny_path_synthesizes_error_and_emits_event() {
        let provider: Box<dyn ProviderClient + Send + Sync> =
            Box::new(ScriptedProvider::new("read_file", json!({"path": ".env"})));
        let host = Host::with_components(config_no_tools(), provider)
            .await
            .unwrap();

        let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
        let outcome = host.run_turn_streaming("hi", tx).await.unwrap();

        // The model thought a tool ran; we report it as Errored with a
        // policy-denied payload.
        assert_eq!(outcome.tool_calls.len(), 1);
        let call = &outcome.tool_calls[0];
        assert_eq!(call.name, "read_file");
        assert_eq!(call.status, ToolCallStatus::Errored);
        assert!(call.result.contains("denied by policy"), "{}", call.result);

        let mut saw_denied = false;
        let mut saw_started = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::ToolCallDenied { ref name, .. } if name == "read_file" => {
                    saw_denied = true;
                }
                TurnEvent::ToolCallStarted { .. } => {
                    saw_started = true;
                }
                _ => {}
            }
        }
        assert!(saw_denied, "expected a ToolCallDenied event");
        assert!(
            !saw_started,
            "ToolCallStarted should not fire for a denied call"
        );
    }

    /// `run` is default-Ask — host must emit `PermissionRequested` and wait
    /// for `resolve_permission`. After Allow, the registry runs (returns
    /// "unknown tool" since none registered, which is fine — we're testing
    /// the gating, not the call).
    #[tokio::test]
    async fn ask_path_blocks_until_resolved() {
        let provider: Box<dyn ProviderClient + Send + Sync> =
            Box::new(ScriptedProvider::new("run", json!({"command": "echo hi"})));
        let host = Arc::new(
            Host::with_components(config_no_tools(), provider)
                .await
                .unwrap(),
        );

        let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
        let host_for_run = Arc::clone(&host);
        let runner = tokio::spawn(async move { host_for_run.run_turn_streaming("hi", tx).await });

        // Drain events until we see PermissionRequested, then resolve Allow.
        let mut request_id: Option<u64> = None;
        while let Some(ev) = rx.recv().await {
            if let TurnEvent::PermissionRequested { id, ref name, .. } = ev {
                assert_eq!(name, "run");
                request_id = Some(id);
                break;
            }
        }
        let id = request_id.expect("PermissionRequested never arrived");
        host.resolve_permission(id, PermissionDecision::Allow).await;

        // Drain the rest.
        while rx.recv().await.is_some() {}

        let outcome = runner.await.unwrap().unwrap();
        // The Allow let the call reach the (empty) registry, which returns an
        // "unknown tool" error. That's the contract we want — the gate said
        // "go" and the call dispatched.
        assert_eq!(outcome.tool_calls.len(), 1);
        let call = &outcome.tool_calls[0];
        assert_eq!(call.name, "run");
        assert_eq!(call.status, ToolCallStatus::Errored);
        assert!(
            call.result.contains("unknown tool"),
            "expected dispatch to reach registry, got: {}",
            call.result
        );
    }

    /// A pre-registered Always rule short-circuits the policy: the modal
    /// never fires and `gate_tool_call` returns the rule's decision. After
    /// M9 PR 4 the rule is keyed on a normalized [`ArgPattern`] (here:
    /// command first-word `echo`) and stored in the policy's `toml_rules`
    /// layer — disk I/O is suppressed because the test policy is transient.
    #[tokio::test]
    async fn session_rule_short_circuits_policy() {
        let provider: Box<dyn ProviderClient + Send + Sync> =
            Box::new(ScriptedProvider::new("run", json!({"command": "echo hi"})));
        let host = Host::with_components(config_no_tools(), provider)
            .await
            .unwrap();
        host.add_session_rule(
            "run",
            &json!({"command": "echo hi"}),
            PermissionDecision::Allow,
        )
        .await;

        let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
        let outcome = host.run_turn_streaming("hi", tx).await.unwrap();

        let mut saw_permission = false;
        while let Some(ev) = rx.recv().await {
            if matches!(ev, TurnEvent::PermissionRequested { .. }) {
                saw_permission = true;
            }
        }
        assert!(
            !saw_permission,
            "session rule should suppress PermissionRequested"
        );
        // Allow lets the call reach the empty registry → "unknown tool".
        assert!(outcome.tool_calls[0].result.contains("unknown tool"));
    }

    /// Lazy-spawn end-to-end (no real bash binary): build the exact
    /// resolver closure `Host::wire_self_into_resolver` builds, drive it
    /// twice, and confirm only the first call emits a prompt — the
    /// second hits the session cache after `AlwaysThisSession`. This is
    /// the contract the lazy `tool-bash` spawn relies on for "prompt
    /// once per session", and the regression is what made this PR
    /// necessary: with the old eager spawn the question was already
    /// settled at registry-connect time.
    #[tokio::test]
    async fn lazy_bash_resolver_prompts_once_then_caches_session() {
        use std::sync::Arc;

        use crate::permissions::BashNetworkPolicy;
        use crate::tools::{BashNetResolver, NetOverride};

        // Build a transient Ask-policy host and reach into its state to
        // construct the same resolver struct `wire_self_into_resolver`
        // installs. We test through the resolver because the registry's
        // lazy-bash dispatch path runs exactly this code on every call.
        let provider: Box<dyn ProviderClient + Send + Sync> =
            Box::new(ScriptedProvider::new("noop", json!({})));
        let project_root = std::env::temp_dir().join("savvagent-lazy-bash-test");
        let config = HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "test-model".to_string(),
        )
        .with_project_root(project_root.clone())
        .with_policy(
            PermissionPolicy::transient(project_root).with_bash_network(BashNetworkPolicy::Ask),
        );
        let host = Arc::new(Host::with_components(config, provider).await.unwrap());

        let resolver = Arc::new(super::HostBashNetResolver {
            policy: host.policy.clone(),
            pending: host.pending_bash_network.clone(),
            next_id: host.next_request_id.clone(),
            current_events: host.current_turn_events.clone(),
        });

        // Publish a per-turn events channel into the host's slot — same
        // as `CurrentTurnEventsGuard::install` would do at turn start.
        let (events_tx, mut events_rx) = mpsc::channel::<TurnEvent>(8);
        *host.current_turn_events.lock().unwrap() = Some(events_tx);

        // Spawn the first resolve in a task so we can pump events and
        // call resolve_bash_network_decision concurrently.
        let resolver_clone = resolver.clone();
        let first = tokio::spawn(async move {
            resolver_clone
                .resolve(NetOverride::Inherit, BashNetContext::default())
                .await
        });

        // We expect a single BashNetworkRequested. Pluck its id, then
        // answer AlwaysThisSession so the cache populates.
        let id = loop {
            match events_rx.recv().await {
                Some(TurnEvent::BashNetworkRequested { id, .. }) => break id,
                Some(_other) => continue,
                None => panic!("events channel closed before BashNetworkRequested arrived"),
            }
        };
        host.resolve_bash_network_decision(id, BashNetworkChoice::AlwaysThisSession)
            .await;

        let allow_first = first.await.unwrap();
        assert!(allow_first, "AlwaysThisSession must resolve allow_net=true");

        // Second resolve: should NOT emit another prompt (cache hit).
        let allow_second = resolver
            .resolve(NetOverride::Inherit, BashNetContext::default())
            .await;
        assert!(
            allow_second,
            "second resolve must reuse the cached AlwaysThisSession decision"
        );

        // Drain any pending events with a tiny window and assert none
        // are BashNetworkRequested. We use try_recv repeatedly with no
        // sleep — the channel either has the message already or it
        // never will because the resolve_bash_network_with_state path
        // short-circuited via `policy.bash_network_cached()`.
        loop {
            match events_rx.try_recv() {
                Ok(TurnEvent::BashNetworkRequested { .. }) => {
                    panic!("second resolve must NOT emit a BashNetworkRequested");
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }

        // Drop the resolver so the test exits cleanly.
        drop(resolver);
    }

    /// The trait's default `resolve` impl short-circuits explicit
    /// overrides *before* it touches the session decision cache. This
    /// pins that contract: even after running the resolver with both
    /// `ForceAllow` and `ForceDeny` back-to-back, the policy's cached
    /// decision must stay `None`.
    #[tokio::test]
    async fn per_call_override_short_circuits_without_touching_cache() {
        use std::sync::Arc;

        use crate::permissions::BashNetworkPolicy;
        use crate::tools::{BashNetResolver, NetOverride};

        let provider: Box<dyn ProviderClient + Send + Sync> =
            Box::new(ScriptedProvider::new("noop", json!({})));
        let project_root = std::env::temp_dir().join("savvagent-per-call-override-test");
        let config = HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "test-model".to_string(),
        )
        .with_project_root(project_root.clone())
        .with_policy(
            PermissionPolicy::transient(project_root).with_bash_network(BashNetworkPolicy::Ask),
        );
        let host = Arc::new(Host::with_components(config, provider).await.unwrap());

        let resolver = Arc::new(super::HostBashNetResolver {
            policy: host.policy.clone(),
            pending: host.pending_bash_network.clone(),
            next_id: host.next_request_id.clone(),
            current_events: host.current_turn_events.clone(),
        });

        let allow = resolver
            .resolve(NetOverride::ForceAllow, BashNetContext::default())
            .await;
        assert!(allow);
        assert_eq!(
            host.policy.bash_network_cached(),
            None,
            "ForceAllow must NOT update the cache"
        );

        let allow = resolver
            .resolve(NetOverride::ForceDeny, BashNetContext::default())
            .await;
        assert!(!allow);
        assert_eq!(
            host.policy.bash_network_cached(),
            None,
            "ForceDeny must NOT update the cache"
        );
    }

    /// `non-streaming` `run_turn` has no event channel, so any Ask collapses
    /// to a Deny rather than hanging on a oneshot that nobody resolves.
    #[tokio::test]
    async fn ask_with_no_events_collapses_to_deny() {
        let provider: Box<dyn ProviderClient + Send + Sync> =
            Box::new(ScriptedProvider::new("run", json!({"command": "echo hi"})));
        let host = Host::with_components(config_no_tools(), provider)
            .await
            .unwrap();

        let outcome = host.run_turn("hi").await.unwrap();
        assert_eq!(outcome.tool_calls.len(), 1);
        assert_eq!(outcome.tool_calls[0].status, ToolCallStatus::Errored);
        assert!(
            outcome.tool_calls[0]
                .result
                .contains("non-interactive turn"),
            "{}",
            outcome.tool_calls[0].result
        );
    }

    /// Some providers (notably Gemini) emit `stop_reason=end_turn`
    /// alongside `tool_use` content blocks because their wire format has no
    /// distinct "tool_use" finish reason. The host must treat tool_use
    /// blocks as authoritative and dispatch them rather than rejecting the
    /// response.
    #[tokio::test]
    async fn tool_use_blocks_run_even_when_stop_reason_says_end_turn() {
        struct EndTurnWithToolUseProvider {
            calls: AtomicUsize,
        }

        #[async_trait]
        impl ProviderClient for EndTurnWithToolUseProvider {
            async fn complete(
                &self,
                req: CompleteRequest,
                _events: Option<mpsc::Sender<StreamEvent>>,
            ) -> Result<CompleteResponse, ProviderError> {
                let n = self.calls.fetch_add(1, AtomicOrdering::SeqCst);
                let content = if n == 0 {
                    vec![ContentBlock::ToolUse {
                        id: "tu_1".into(),
                        name: "definitely_no_such_tool".into(),
                        input: json!({"arg": "value"}),
                    }]
                } else {
                    vec![ContentBlock::Text {
                        text: "all done".into(),
                    }]
                };
                Ok(CompleteResponse {
                    id: format!("resp-{n}"),
                    model: req.model,
                    content,
                    stop_reason: StopReason::EndTurn,
                    stop_sequence: None,
                    usage: Usage::default(),
                })
            }
        }

        let provider: Box<dyn ProviderClient + Send + Sync> =
            Box::new(EndTurnWithToolUseProvider {
                calls: AtomicUsize::new(0),
            });
        let host = Host::with_components(config_no_tools(), provider)
            .await
            .unwrap();
        host.add_session_rule(
            "definitely_no_such_tool",
            &json!({"arg": "value"}),
            PermissionDecision::Allow,
        )
        .await;

        let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
        let outcome = host.run_turn_streaming("hi", tx).await.unwrap();
        while rx.recv().await.is_some() {}

        assert_eq!(
            outcome.tool_calls.len(),
            1,
            "host must dispatch the tool_use block even when stop_reason=end_turn"
        );
        assert_eq!(outcome.text, "all done");
    }

    /// A response that pairs `tool_use` with `StopReason::MaxTokens` still
    /// runs the tool — the new "content is authoritative" rule applies to
    /// every non-`EndTurn` stop reason, not just `EndTurn`. This pins the
    /// behavior so a future refactor can't accidentally restore a
    /// `stop_reason`-gated short-circuit for `MaxTokens`/`Refusal`/etc.
    #[tokio::test]
    async fn tool_use_blocks_run_even_when_stop_reason_is_max_tokens() {
        struct MaxTokensWithToolUseProvider {
            calls: AtomicUsize,
        }

        #[async_trait]
        impl ProviderClient for MaxTokensWithToolUseProvider {
            async fn complete(
                &self,
                req: CompleteRequest,
                _events: Option<mpsc::Sender<StreamEvent>>,
            ) -> Result<CompleteResponse, ProviderError> {
                let n = self.calls.fetch_add(1, AtomicOrdering::SeqCst);
                let (content, stop_reason) = if n == 0 {
                    (
                        vec![ContentBlock::ToolUse {
                            id: "tu_max".into(),
                            name: "definitely_no_such_tool".into(),
                            input: json!({}),
                        }],
                        StopReason::MaxTokens,
                    )
                } else {
                    (
                        vec![ContentBlock::Text {
                            text: "done".into(),
                        }],
                        StopReason::EndTurn,
                    )
                };
                Ok(CompleteResponse {
                    id: format!("resp-{n}"),
                    model: req.model,
                    content,
                    stop_reason,
                    stop_sequence: None,
                    usage: Usage::default(),
                })
            }
        }

        let provider: Box<dyn ProviderClient + Send + Sync> =
            Box::new(MaxTokensWithToolUseProvider {
                calls: AtomicUsize::new(0),
            });
        let host = Host::with_components(config_no_tools(), provider)
            .await
            .unwrap();
        host.add_session_rule(
            "definitely_no_such_tool",
            &json!({}),
            PermissionDecision::Allow,
        )
        .await;

        let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
        let outcome = host.run_turn_streaming("hi", tx).await.unwrap();
        while rx.recv().await.is_some() {}

        assert_eq!(outcome.tool_calls.len(), 1);
        assert_eq!(outcome.text, "done");
    }

    /// Many real responses (Gemini especially) carry preamble text in the
    /// same turn as a tool_use ("I'll check that for you" + `functionCall`).
    /// The host must still dispatch the tool and continue; the intermediate
    /// turn's text is not exposed via `TurnOutcome` (only the final turn's
    /// text is), so we assert on the assistant turn the host commits to
    /// `state.messages`.
    #[tokio::test]
    async fn mixed_text_and_tool_use_in_one_response_runs_tool() {
        struct MixedProvider {
            calls: AtomicUsize,
        }

        #[async_trait]
        impl ProviderClient for MixedProvider {
            async fn complete(
                &self,
                req: CompleteRequest,
                _events: Option<mpsc::Sender<StreamEvent>>,
            ) -> Result<CompleteResponse, ProviderError> {
                let n = self.calls.fetch_add(1, AtomicOrdering::SeqCst);
                let content = if n == 0 {
                    vec![
                        ContentBlock::Text {
                            text: "I'll check that.".into(),
                        },
                        ContentBlock::ToolUse {
                            id: "tu_mixed".into(),
                            name: "definitely_no_such_tool".into(),
                            input: json!({}),
                        },
                    ]
                } else {
                    vec![ContentBlock::Text {
                        text: "final reply".into(),
                    }]
                };
                Ok(CompleteResponse {
                    id: format!("resp-{n}"),
                    model: req.model,
                    content,
                    stop_reason: StopReason::EndTurn,
                    stop_sequence: None,
                    usage: Usage::default(),
                })
            }
        }

        let provider: Box<dyn ProviderClient + Send + Sync> = Box::new(MixedProvider {
            calls: AtomicUsize::new(0),
        });
        let host = Host::with_components(config_no_tools(), provider)
            .await
            .unwrap();
        host.add_session_rule(
            "definitely_no_such_tool",
            &json!({}),
            PermissionDecision::Allow,
        )
        .await;

        let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
        let outcome = host.run_turn_streaming("hi", tx).await.unwrap();
        while rx.recv().await.is_some() {}

        assert_eq!(outcome.tool_calls.len(), 1);
        assert_eq!(outcome.text, "final reply");

        // The intermediate assistant turn that mixed text + tool_use must
        // round-trip into session state verbatim — both blocks preserved.
        let state = host.state.lock().await;
        let first_assistant = state
            .messages
            .iter()
            .find(|m| matches!(m.role, Role::Assistant))
            .expect("session must record the assistant turn");
        assert_eq!(
            first_assistant.content.len(),
            2,
            "intermediate assistant turn must keep both Text and ToolUse blocks"
        );
        assert!(matches!(
            &first_assistant.content[0],
            ContentBlock::Text { text } if text == "I'll check that."
        ));
        assert!(matches!(
            &first_assistant.content[1],
            ContentBlock::ToolUse { name, .. } if name == "definitely_no_such_tool"
        ));
    }

    #[test]
    fn format_prompt_summary_without_command_is_static() {
        let s = super::format_bash_network_prompt_summary(None);
        assert_eq!(s, super::BASH_NETWORK_PROMPT_SUMMARY);
    }

    #[test]
    fn format_prompt_summary_with_short_command_includes_command_line() {
        let s = super::format_bash_network_prompt_summary(Some("curl https://example.com"));
        assert!(
            s.starts_with(super::BASH_NETWORK_PROMPT_SUMMARY),
            "summary must still lead with the static line: {s}"
        );
        assert!(
            s.contains("$ curl https://example.com"),
            "summary must show the command after a $ prompt: {s}"
        );
        assert!(
            !s.contains("…"),
            "short commands must not be truncated: {s}"
        );
    }

    #[test]
    fn format_prompt_summary_truncates_long_command_with_ellipsis() {
        let long = "x".repeat(super::BASH_PROMPT_COMMAND_TRUNCATE + 50);
        let s = super::format_bash_network_prompt_summary(Some(&long));
        assert!(
            s.contains('…'),
            "commands longer than the truncate limit must end with an ellipsis: {s}"
        );
        // The truncated portion must respect the limit (limit-1 chars + '…' = limit chars total).
        let body_line = s.lines().nth(1).expect("summary should have a 2nd line");
        let visible: String = body_line.chars().skip_while(|c| *c != 'x').collect();
        assert_eq!(
            visible.chars().count(),
            super::BASH_PROMPT_COMMAND_TRUNCATE,
            "truncated body must be exactly the truncate limit in chars: {visible:?}"
        );
    }

    /// Records the `system` field of every `CompleteRequest`, then returns
    /// an `end_turn` response. Used to verify what the host puts in
    /// `req.system` after layering.
    struct CapturingProvider {
        captured: Arc<std::sync::Mutex<Vec<Option<String>>>>,
    }

    impl CapturingProvider {
        fn new() -> (Self, Arc<std::sync::Mutex<Vec<Option<String>>>>) {
            let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
            (
                Self {
                    captured: captured.clone(),
                },
                captured,
            )
        }
    }

    #[async_trait]
    impl ProviderClient for CapturingProvider {
        async fn complete(
            &self,
            req: CompleteRequest,
            _events: Option<mpsc::Sender<StreamEvent>>,
        ) -> Result<CompleteResponse, ProviderError> {
            self.captured.lock().unwrap().push(req.system.clone());
            Ok(CompleteResponse {
                id: "resp-cap".into(),
                model: req.model,
                content: vec![ContentBlock::Text {
                    text: "done".into(),
                }],
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
                usage: Usage::default(),
            })
        }
    }

    #[tokio::test]
    async fn host_start_default_prompt_enabled_attaches_system_message() {
        let d = tempfile::tempdir().unwrap();
        let config = HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "test-model".to_string(),
        )
        .with_project_root(d.path().to_path_buf())
        .with_app_version("9.9.9")
        .with_policy(PermissionPolicy::transient(d.path().to_path_buf()));

        let (provider, captured) = CapturingProvider::new();
        let host = Host::with_components(config, Box::new(provider))
            .await
            .unwrap();
        host.run_turn("hello").await.unwrap();

        let captured = captured.lock().unwrap();
        let system = captured
            .first()
            .expect("at least one completion captured")
            .as_ref()
            .expect("system prompt must be Some when default is enabled");
        assert!(
            system.contains("# Savvagent default prompt"),
            "missing default heading in:\n{system}"
        );
        assert!(system.contains("You are Savvagent"));
        assert!(system.contains("Savvagent version: 9.9.9"));
    }

    #[tokio::test]
    async fn host_start_default_prompt_disabled_omits_default() {
        let d = tempfile::tempdir().unwrap();
        let config = HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "test-model".to_string(),
        )
        .with_project_root(d.path().to_path_buf())
        .with_default_prompt_disabled()
        .with_policy(PermissionPolicy::transient(d.path().to_path_buf()));

        let (provider, captured) = CapturingProvider::new();
        let host = Host::with_components(config, Box::new(provider))
            .await
            .unwrap();
        host.run_turn("hello").await.unwrap();

        let captured = captured.lock().unwrap();
        let system = captured.first().expect("at least one completion captured");
        assert!(
            system.is_none(),
            "expected None system when default disabled with no override + no SAVVAGENT.md, got: {system:?}"
        );
    }

    #[tokio::test]
    async fn host_start_default_plus_savvagent_md_composes_in_order() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("SAVVAGENT.md"), "BODY_TEXT\n").unwrap();
        let config = HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "test-model".to_string(),
        )
        .with_project_root(d.path().to_path_buf())
        .with_policy(PermissionPolicy::transient(d.path().to_path_buf()));

        let (provider, captured) = CapturingProvider::new();
        let host = Host::with_components(config, Box::new(provider))
            .await
            .unwrap();
        host.run_turn("hello").await.unwrap();

        let captured = captured.lock().unwrap();
        let system = captured.first().unwrap().as_ref().unwrap();
        let i_default = system.find("# Savvagent default prompt").expect(system);
        let i_body = system
            .find("# Project context (from SAVVAGENT.md)")
            .expect(system);
        assert!(i_default < i_body, "{system}");
        assert!(system.contains("BODY_TEXT"));
    }

    #[tokio::test]
    async fn host_start_with_system_prompt_attaches_host_override_section() {
        // `HostConfig::system_prompt = Some("...")` should appear in
        // the rendered prompt as the middle layer (between default and
        // SAVVAGENT.md body). Pins the override-layer wiring through
        // the real `build_layered_system_prompt` path.
        let d = tempfile::tempdir().unwrap();
        let mut config = HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "test-model".to_string(),
        )
        .with_project_root(d.path().to_path_buf())
        .with_policy(PermissionPolicy::transient(d.path().to_path_buf()));
        config.system_prompt = Some("OVERRIDE_PAYLOAD".to_string());

        let (provider, captured) = CapturingProvider::new();
        let host = Host::with_components(config, Box::new(provider))
            .await
            .unwrap();
        host.run_turn("hello").await.unwrap();

        let captured = captured.lock().unwrap();
        let system = captured.first().unwrap().as_ref().unwrap();
        let i_default = system.find("# Savvagent default prompt").expect(system);
        let i_override = system.find("# Host override").expect(system);
        assert!(i_default < i_override, "{system}");
        assert!(system.contains("OVERRIDE_PAYLOAD"));
    }

    #[tokio::test]
    async fn host_start_without_app_version_uses_host_crate_label() {
        // When `HostConfig::app_version` is unset, the prompt should
        // render the `AppVersion::HostCrateFallback` label so a
        // library embedder forgetting `with_app_version` is visible.
        let d = tempfile::tempdir().unwrap();
        let config = HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "test-model".to_string(),
        )
        .with_project_root(d.path().to_path_buf())
        .with_policy(PermissionPolicy::transient(d.path().to_path_buf()));
        // Note: no `.with_app_version(...)` call.

        let (provider, captured) = CapturingProvider::new();
        let host = Host::with_components(config, Box::new(provider))
            .await
            .unwrap();
        host.run_turn("hello").await.unwrap();

        let captured = captured.lock().unwrap();
        let system = captured.first().unwrap().as_ref().unwrap();
        assert!(
            system.contains("Savvagent host crate version:"),
            "expected host-crate fallback label, got:\n{system}"
        );
        assert!(
            !system.contains("Savvagent version:"),
            "App-version label leaked when no embedder version was set: {system}"
        );
    }

    /// Direct unit test of the shared `build_layered_system_prompt`
    /// helper that both `Host::start` and `Host::with_components`
    /// call. A regression that changes one constructor's wiring but
    /// not the other wouldn't be caught by the constructor-level
    /// `CapturingProvider` tests alone — this test pins the helper's
    /// behaviour in isolation so any future refactor that bypasses
    /// it from one constructor would surface as a coverage gap, not
    /// a silent regression.
    #[tokio::test]
    async fn build_layered_system_prompt_helper_composes_all_three_layers() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("SAVVAGENT.md"), "PROJECT_BODY\n").unwrap();
        let mut config = HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "test-model".to_string(),
        )
        .with_project_root(d.path().to_path_buf())
        .with_app_version("7.7.7")
        .with_policy(PermissionPolicy::transient(d.path().to_path_buf()));
        config.system_prompt = Some("MIDDLE_LAYER".to_string());

        let sandbox = crate::sandbox::SandboxConfig::default();
        let resolver = bootstrap_bash_net_resolver();
        let tools =
            crate::tools::ToolRegistry::connect(&[], &config.project_root, &sandbox, resolver)
                .await
                .unwrap();

        let prompt = super::build_layered_system_prompt(&config, &tools)
            .expect("default + override + body must produce Some");

        // All three layer headings present.
        for h in &[
            "# Savvagent default prompt",
            "# Host override",
            "# Project context (from SAVVAGENT.md)",
        ] {
            assert!(prompt.contains(h), "missing heading {h} in:\n{prompt}");
        }
        // Override and body content rendered.
        assert!(prompt.contains("MIDDLE_LAYER"));
        assert!(prompt.contains("PROJECT_BODY"));
        // Default-section content reflects the embedder version label.
        assert!(prompt.contains("Savvagent version: 7.7.7"));
        // Bash isn't wired in this fixture; the no-tools branch fires.
        assert!(prompt.contains("No tools are currently connected"));
    }
}

#[cfg(test)]
mod list_models_tests {
    use async_trait::async_trait;
    use savvagent_mcp::{InProcessProviderClient, ProviderClient, ProviderHandler, StreamEmitter};
    use savvagent_protocol::{
        CompleteRequest, CompleteResponse, ErrorKind, ListModelsResponse, ModelInfo, ProviderError,
    };

    use super::*;
    use crate::config::{HostConfig, ProviderEndpoint};

    /// Handler that advertises a tiny curated list. Used to exercise the
    /// `Host::list_models` facade end-to-end via the in-process bridge.
    struct CuratedHandler;

    #[async_trait]
    impl ProviderHandler for CuratedHandler {
        async fn complete(
            &self,
            _req: CompleteRequest,
            _emit: Option<&dyn StreamEmitter>,
        ) -> Result<CompleteResponse, ProviderError> {
            unreachable!("complete is not exercised in list_models tests")
        }
        async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
            Ok(ListModelsResponse {
                models: vec![
                    ModelInfo {
                        id: "alpha".into(),
                        display_name: Some("Alpha".into()),
                        context_window: Some(8192),
                    },
                    ModelInfo {
                        id: "beta".into(),
                        display_name: None,
                        context_window: None,
                    },
                ],
                default_model_id: Some("alpha".into()),
            })
        }
    }

    /// Handler that inherits the default `list_models` trait impl, signaling
    /// "not advertised" to host callers.
    struct SilentHandler;

    #[async_trait]
    impl ProviderHandler for SilentHandler {
        async fn complete(
            &self,
            _req: CompleteRequest,
            _emit: Option<&dyn StreamEmitter>,
        ) -> Result<CompleteResponse, ProviderError> {
            unreachable!("complete is not exercised in list_models tests")
        }
    }

    fn config() -> HostConfig {
        let project_root = std::env::temp_dir().join("savvagent-list-models-test");
        HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "alpha".to_string(),
        )
        .with_project_root(project_root.clone())
        .with_policy(PermissionPolicy::transient(project_root))
    }

    #[tokio::test]
    async fn host_list_models_returns_provider_list() {
        let provider: Box<dyn ProviderClient + Send + Sync> = Box::new(
            InProcessProviderClient::new(std::sync::Arc::new(CuratedHandler)),
        );
        let host = Host::with_components(config(), provider).await.unwrap();
        let resp = host
            .list_models()
            .await
            .expect("list_models should succeed");
        let ids: Vec<_> = resp.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "beta"]);
        assert_eq!(resp.default_model_id, Some("alpha".into()));
    }

    #[tokio::test]
    async fn host_list_models_surfaces_not_advertised_error() {
        let provider: Box<dyn ProviderClient + Send + Sync> = Box::new(
            InProcessProviderClient::new(std::sync::Arc::new(SilentHandler)),
        );
        let host = Host::with_components(config(), provider).await.unwrap();
        let err = host
            .list_models()
            .await
            .expect_err("default impl must error");
        assert!(matches!(err.kind, ErrorKind::NotImplemented));
        assert!(err.message.contains("list_models"), "msg: {}", err.message);
    }
}

#[cfg(test)]
mod transcript_tests {
    use async_trait::async_trait;
    use savvagent_mcp::ProviderClient;
    use savvagent_protocol::{
        CompleteRequest, CompleteResponse, ContentBlock, Message, ProviderError, Role, StopReason,
        Usage,
    };
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::config::{HostConfig, ProviderEndpoint};
    use crate::permissions::PermissionPolicy;

    /// Minimal provider that immediately returns end_turn with no content.
    struct NoopProvider;

    #[async_trait]
    impl ProviderClient for NoopProvider {
        async fn complete(
            &self,
            req: CompleteRequest,
            _events: Option<mpsc::Sender<StreamEvent>>,
        ) -> Result<CompleteResponse, ProviderError> {
            Ok(CompleteResponse {
                id: "noop".into(),
                model: req.model,
                content: vec![ContentBlock::Text { text: "ok".into() }],
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
                usage: Usage::default(),
            })
        }
    }

    fn tmp_config(dir: &std::path::Path) -> HostConfig {
        HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "inproc://test".into(),
            },
            "test-model".to_string(),
        )
        .with_project_root(dir.to_path_buf())
        .with_policy(PermissionPolicy::transient(dir.to_path_buf()))
    }

    /// Round-trip: save then load recovers the same message history.
    #[tokio::test]
    async fn round_trip_recovers_messages() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.json");

        // Build a host with some history.
        let host1 = Host::with_components(
            tmp_config(dir.path()),
            Box::new(NoopProvider) as Box<dyn ProviderClient + Send + Sync>,
        )
        .await
        .unwrap();
        host1.run_turn("hello").await.unwrap();
        host1.save_transcript(&path).await.unwrap();
        let saved = host1.messages().await;

        // Load into a fresh host.
        let host2 = Host::with_components(
            tmp_config(dir.path()),
            Box::new(NoopProvider) as Box<dyn ProviderClient + Send + Sync>,
        )
        .await
        .unwrap();
        let record = host2.load_transcript(&path).await.unwrap();
        let loaded = host2.messages().await;

        assert_eq!(saved, loaded, "message history must survive round-trip");
        assert_eq!(record.schema_version, TRANSCRIPT_SCHEMA_VERSION);
        assert_eq!(record.model, "test-model");
    }

    /// Schema version mismatch yields a typed error, not a panic.
    #[tokio::test]
    async fn schema_mismatch_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("future.json");

        let future_file = serde_json::json!({
            "schema_version": TRANSCRIPT_SCHEMA_VERSION + 1,
            "model": "some-model",
            "saved_at": 0,
            "messages": []
        });
        tokio::fs::write(&path, serde_json::to_vec(&future_file).unwrap())
            .await
            .unwrap();

        let host = Host::with_components(
            tmp_config(dir.path()),
            Box::new(NoopProvider) as Box<dyn ProviderClient + Send + Sync>,
        )
        .await
        .unwrap();
        let err = host.load_transcript(&path).await.unwrap_err();
        assert!(
            matches!(err, TranscriptError::SchemaMismatch { .. }),
            "expected SchemaMismatch, got {err:?}"
        );
    }

    /// Malformed JSON returns an error without panicking.
    #[tokio::test]
    async fn malformed_json_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.json");
        tokio::fs::write(&path, b"{ not valid json !!!")
            .await
            .unwrap();

        let host = Host::with_components(
            tmp_config(dir.path()),
            Box::new(NoopProvider) as Box<dyn ProviderClient + Send + Sync>,
        )
        .await
        .unwrap();
        let err = host.load_transcript(&path).await.unwrap_err();
        assert!(
            matches!(err, TranscriptError::Malformed(_)),
            "expected Malformed, got {err:?}"
        );
    }

    /// Legacy bare-array transcripts (pre-resume) are accepted transparently.
    #[tokio::test]
    async fn legacy_bare_array_loads_ok() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy.json");

        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "legacy message".into(),
            }],
        }];
        tokio::fs::write(&path, serde_json::to_vec_pretty(&messages).unwrap())
            .await
            .unwrap();

        let host = Host::with_components(
            tmp_config(dir.path()),
            Box::new(NoopProvider) as Box<dyn ProviderClient + Send + Sync>,
        )
        .await
        .unwrap();
        let record = host.load_transcript(&path).await.unwrap();
        assert_eq!(record.messages, messages);
        assert_eq!(record.schema_version, TRANSCRIPT_SCHEMA_VERSION);
    }
}
