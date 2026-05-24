//! Tool MCP server registry.
//!
//! At startup, [`ToolRegistry::connect`] sets up each configured stdio tool
//! server. Most tools are spawned eagerly: the registry connects to them,
//! fetches their `tools/list`, and routes calls to the cached process. The
//! `tool-bash` server is special — its OS sandbox needs to bake in an
//! `allow_net` decision that we can't make until the host's event loop is
//! running and (for `BashNetworkPolicy::Ask`) the user has answered a prompt.
//! For that one tool we still do a *one-shot probe spawn* at connect time
//! to scrape the tool list (with `allow_net = false` — the spawn never
//! actually runs any shell command, just speaks MCP). After the probe we
//! shut the process down and lazy-spawn it on the first call.
//!
//! During the tool-use loop, the host calls
//! [`ToolRegistry::call_with_bash_net_override`] with the model's chosen
//! tool name, JSON arguments, and an optional per-call `net_override`. The
//! registry dispatches the call and returns a normalized
//! [`ToolCallOutcome`]. The bash slot caches a single active server keyed
//! on the `allow_net` it was spawned with: subsequent calls reuse it, and
//! a per-call override that disagrees with the active server's
//! `allow_net` kills the old child and respawns. Per-call overrides do
//! **not** mutate the session-cached bash network decision.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use async_trait::async_trait;
use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{CallToolRequestParams, ResourceUpdatedNotificationParam},
    service::{NotificationContext, RunningService, ServiceError},
    transport::TokioChildProcess,
};
use savvagent_protocol::ToolDef;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::config::ToolEndpoint;
use crate::logging::tool_stderr_log_file;
use crate::sandbox::{SandboxConfig, SandboxWrapper, apply_sandbox};

/// The substring we use to identify a `tool-bash` binary path. Mirrors the
/// detection scheme used in `sandbox.rs::net_allowed_for`.
const TOOL_BASH_MARKER: &str = "tool-bash";

/// Name of the host-built-in tool that reads MCP resources by URI.
/// Always present in `ToolRegistry::defs`, regardless of whether any
/// connected tool publishes resources today.
pub(crate) const READ_RESOURCE_TOOL_NAME: &str = "read_resource";

/// Per-call override of `tool-bash`'s network access.
///
/// | Variant      | Meaning                                                 |
/// |--------------|---------------------------------------------------------|
/// | `Inherit`    | Defer to the resolver's policy (may park on a prompt).  |
/// | `ForceAllow` | Grant network access regardless of policy or cache.     |
/// | `ForceDeny`  | Deny network access regardless of policy or cache.      |
///
/// Explicit overrides never touch the resolver cache — see
/// [`BashNetResolver::resolve`] for the short-circuit guarantee.
///
/// # `Default`
///
/// [`NetOverride::Inherit`] is the `Default`. It models the "no flag" /
/// "no override" case. Do NOT rely on `..Default::default()` in a context
/// that requires an explicit user choice — the silent default would let
/// the policy/prompt path run when the call site expected an explicit
/// pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetOverride {
    /// Defer to the resolver's policy. The resolver may emit a
    /// [`crate::session::TurnEvent::BashNetworkRequested`] prompt.
    #[default]
    Inherit,
    /// Bypass the resolver and grant network access for this call's spawn.
    ForceAllow,
    /// Bypass the resolver and deny network access for this call's spawn.
    ForceDeny,
}

/// Per-call context passed to [`BashNetResolver`]. Today the only field
/// is the bash command itself, which the host uses to populate the prompt
/// summary so the user can see *what* they're being asked about (rather
/// than the static "tool-bash spawn requests network access" line). The
/// struct is non-exhaustive so we can grow it (e.g. tool name, working
/// directory) without breaking implementors.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct BashNetContext<'a> {
    /// The shell command the spawn will execute, or `None` if the
    /// caller can't supply it (e.g. test fixtures). Truncated by the
    /// resolver before display.
    pub command: Option<&'a str>,
}

impl<'a> BashNetContext<'a> {
    /// Construct a context referencing the given command string.
    pub fn with_command(command: &'a str) -> Self {
        Self {
            command: Some(command),
        }
    }
}

/// Per-subagent context passed through the in-process tool path.
/// Carried inside [`ToolCallContext::subagent`] as an `Option`: `None`
/// means the call originates from the parent's main turn loop.
#[derive(Debug, Clone)]
pub struct SubagentContext {
    /// Nesting depth. Parent's first `task` call → depth 1; that
    /// subagent's `task` call → depth 2; and so on. Capped at
    /// `SAVVAGENT_AGENT_MAX_DEPTH` (default 3).
    pub depth: u8,
    /// The agent name (slug) currently executing.
    pub agent_name: String,
    /// The parent (top-level) session ID. Stable across subagent
    /// nesting so hooks can correlate across levels.
    pub parent_session_id: String,
}

/// Concrete context passed to `InProcessToolHandler::call`. Handlers
/// downcast `Arc<dyn Any>` to `Arc<ToolCallContext>` to obtain this.
pub struct ToolCallContext {
    /// The parent `Host`. In-process handlers use this to clone the
    /// provider client, tool registry, gate, permissions, etc.
    pub host: Arc<crate::Host>,
    /// `Some` iff this call originates from a Sub-Host.
    pub subagent: Option<SubagentContext>,
    /// Cancellation token; child of the parent turn's token.
    pub cancellation: CancellationToken,
}

/// Resolver invoked by [`ToolRegistry::call_with_bash_net_override`] when a
/// bash dispatch needs to know what `allow_net` to spawn with.
///
/// The trait's required method, [`resolve_policy`], is the no-override case
/// — implementations consult their permission state and may park on a user
/// prompt. The default [`resolve`] method short-circuits explicit
/// [`NetOverride::ForceAllow`] / [`NetOverride::ForceDeny`] without
/// touching the session cache; only [`NetOverride::Inherit`] reaches
/// [`resolve_policy`]. Callers should always invoke [`resolve`] — the
/// short-circuit logic stays in one place.
///
/// [`resolve_policy`]: BashNetResolver::resolve_policy
/// [`resolve`]: BashNetResolver::resolve
#[async_trait]
pub trait BashNetResolver: Send + Sync + 'static {
    /// Resolve the policy-level `allow_net`. May park on a user prompt.
    /// `context` carries per-call info (e.g. the bash command being
    /// spawned) used to render a meaningful prompt.
    async fn resolve_policy(&self, context: BashNetContext<'_>) -> bool;

    /// Resolve with override consideration. The default implementation
    /// short-circuits explicit overrides; `Inherit` defers to
    /// [`resolve_policy`].
    ///
    /// **Do not override this method.** The dispatcher in
    /// [`LazyBash::dispatch`] relies on the exact "explicit overrides
    /// never touch `resolve_policy`, never touch the cache" behavior
    /// expressed below. Overriding `resolve` is permitted by Rust but
    /// breaks an invariant pinned only by the
    /// `force_allow_short_circuits_policy` /
    /// `force_deny_short_circuits_policy` tests, neither of which run
    /// against a custom override. If you need different policy
    /// behavior, change [`resolve_policy`].
    ///
    /// [`resolve_policy`]: BashNetResolver::resolve_policy
    async fn resolve(&self, over: NetOverride, context: BashNetContext<'_>) -> bool {
        match over {
            NetOverride::ForceAllow => true,
            NetOverride::ForceDeny => false,
            NetOverride::Inherit => self.resolve_policy(context).await,
        }
    }
}

/// Shorthand for the trait-object handle the registry stores. Held behind
/// an `RwLock` in [`LazyBash`] so the host can swap in the real resolver
/// after construction (see [`crate::session::Host::wire_self_into_resolver`]).
pub(crate) type BashNetResolverHandle = Arc<dyn BashNetResolver>;

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
    pub(crate) async fn forward_updated_for_test(&self, params: ResourceUpdatedNotificationParam) {
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
    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<rmcp::RoleClient>,
    ) {
        self.send_updated(params.uri.to_string()).await;
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<rmcp::RoleClient>) {
        self.send_list_changed().await;
    }
}

/// Aggregate view of all connected tool servers.
pub struct ToolRegistry {
    /// Eager (always-spawned) tool servers. Indices in `routes` for
    /// non-bash tools point into this vector.
    eager_servers: Vec<ToolServer>,
    /// Tool name → index into `eager_servers`. Bash tools (currently `run`)
    /// are NOT in this map; they hit the lazy path below instead.
    routes: HashMap<String, usize>,
    /// Aggregated tool definitions, in the order they were discovered.
    /// Includes bash's `run` (scraped via a probe spawn at connect time).
    pub(crate) defs: Vec<ToolDef>,
    /// Optional lazy slot for the configured `tool-bash` endpoint. `None`
    /// when no bash endpoint was supplied (e.g. tests).
    lazy_bash: Option<LazyBash>,
    /// In-process tool handlers, registered by built-in plugins via
    /// `Effect::RegisterInProcessTool`. Looked up before the stdio
    /// children map.
    in_process: tokio::sync::RwLock<
        std::collections::HashMap<String, savvagent_plugin::InProcessToolHandlerArc>,
    >,
    /// Tool definitions for in-process tools, exposed via [`Self::tool_defs`].
    in_process_defs: tokio::sync::RwLock<std::collections::HashMap<String, ToolDef>>,
}

struct ToolServer {
    label: String,
    service: RunningService<RoleClient, ResourceCapturingHandler>,
}

/// Lazy-spawn slot for `tool-bash`. The server is spawned on demand by
/// [`ToolRegistry::call_with_bash_net_override`] and cached until either
/// the registry shuts down or a call arrives with an `allow_net` that
/// disagrees with the cached spawn (in which case the cached process is
/// killed and a fresh one is spawned).
struct LazyBash {
    /// Set of tool names this lazy slot is responsible for dispatching.
    /// Populated by the probe spawn at connect time. In practice this is
    /// `{"run"}` today but storing the full set keeps the registry
    /// honest if tool-bash ever advertises more tools.
    tool_names: std::collections::HashSet<String>,
    /// Static spawn parameters captured at `connect` time. Cloned and
    /// mutated (per-spawn `allow_net` injected into `sandbox_template`)
    /// each time we (re)spawn the child.
    config: BashSpawnConfig,
    /// Resolves the runtime `allow_net` for a given per-call override.
    /// Invoked once per call before we look at the cached active server.
    /// Held behind `RwLock` so the host can install the real resolver
    /// (one that calls back into the host's permission state) after
    /// `Host` construction — at `connect` time we don't yet have an
    /// `Arc<Host>` to give the resolver, so we install a deny-by-default
    /// placeholder and swap it during `wire_self_into_resolver`.
    resolver: Arc<RwLock<BashNetResolverHandle>>,
    /// Currently-active spawned server, if any.
    ///
    /// The lock guards only the (reuse-or-respawn → dispatch) sequence
    /// — the resolver runs unlocked (it may park on a user prompt, and
    /// we don't want to serialize that across all bash dispatches).
    ///
    /// Today's flow is serial-by-construction at the TUI layer
    /// (`app.is_loading` gate prevents concurrent `/bash` invocations
    /// and model-driven calls run sequentially through the turn loop),
    /// so concurrent dispatches don't race in practice. If a future
    /// caller breaks that, both could park on the same prompt's
    /// `oneshot`.
    active: Mutex<Option<ActiveBashServer>>,
}

/// Captured at `ToolRegistry::connect` time and reused for every (re)spawn
/// of the bash child. The only field that varies per spawn is the
/// `allow_net` injected into `sandbox_template.tool_overrides["tool-bash"]`.
#[derive(Clone)]
struct BashSpawnConfig {
    command: PathBuf,
    args: Vec<String>,
    project_root: PathBuf,
    /// Base sandbox config; the per-spawn `allow_net` is injected as a
    /// `tool_overrides[TOOL_BASH_MARKER]` entry before calling
    /// `apply_sandbox`. Cloned per spawn so we never permanently mutate
    /// the host's `SandboxConfig`.
    sandbox_template: SandboxConfig,
}

/// Spawn-determining parameters of an active `tool-bash` child. Two
/// `BashSpawnKey`s compare equal iff the cached server is suitable for
/// the new call without a respawn. Today the only spawn-determining
/// parameter is `allow_net`; a future domain-allowlist extension would
/// add an `allowed_domains` field, at which point every existing site
/// that asks "does the cache still satisfy this call?" already routes
/// through the key's `==` and gets the new field for free (provided
/// `PartialEq` derive — or a manual impl — includes it).
#[derive(Debug, Clone, PartialEq, Eq)]
struct BashSpawnKey {
    allow_net: bool,
}

/// An active, lazily-spawned `tool-bash` child plus the [`BashSpawnKey`]
/// it was spawned with. Killed (via `service.cancel()`) before a respawn
/// or at registry shutdown.
struct ActiveBashServer {
    label: String,
    service: RunningService<RoleClient, ()>,
    spawn_key: BashSpawnKey,
}

impl ToolRegistry {
    /// Spawn each non-bash tool server eagerly, probe-spawn the bash
    /// tool (if configured) just long enough to scrape its tool list,
    /// then aggregate everything.
    ///
    /// `project_root` is forwarded to every spawned child via three parallel
    /// env vars — `SAVVAGENT_TOOL_FS_ROOT`, `SAVVAGENT_TOOL_BASH_ROOT`, and
    /// `SAVVAGENT_TOOL_GREP_ROOT` — so the bundled tool binaries confine
    /// themselves to the host's project root by default. Setting all three
    /// on every tool is harmless: each tool reads only the var it cares about.
    ///
    /// `sandbox` is applied to each spawn when [`SandboxConfig::enabled`] is
    /// `true` and the platform wrapper binary (`bwrap` / `sandbox-exec`) is
    /// found on `$PATH`. If it's missing, the tool runs unwrapped with a
    /// warning — sandboxing is never a hard prerequisite.
    ///
    /// `bash_net_resolver` is invoked on every `tool-bash` call to resolve
    /// the `allow_net` for that call's spawn. See [`BashNetResolver`].
    pub(crate) async fn connect(
        endpoints: &[ToolEndpoint],
        project_root: &Path,
        sandbox: &SandboxConfig,
        bash_net_resolver: BashNetResolverHandle,
        resource_tx: tokio::sync::mpsc::Sender<ResourceEvent>,
    ) -> Result<Self> {
        let mut eager_servers = Vec::new();
        let mut routes: HashMap<String, usize> = HashMap::new();
        let mut defs = Vec::new();
        let mut lazy_bash: Option<LazyBash> = None;

        for ep in endpoints {
            match ep {
                ToolEndpoint::Stdio { command, args } => {
                    let is_bash = command.to_string_lossy().contains(TOOL_BASH_MARKER);
                    if is_bash {
                        // Probe spawn: spawn bash with allow_net=false just
                        // long enough to scrape its tool list, then drop it
                        // before any user command can run. The session's
                        // first real call lazily respawns with the runtime
                        // allow_net decision.
                        let probe_cmd = build_bash_command(
                            command,
                            args,
                            project_root,
                            sandbox,
                            /* allow_net = */ false,
                        );
                        let label = command.display().to_string();
                        let transport = TokioChildProcess::new(probe_cmd)
                            .with_context(|| format!("spawn tool-bash probe: {label}"))?;
                        let handler =
                            ResourceCapturingHandler::new(label.clone(), resource_tx.clone());
                        let service = handler
                            .serve(transport)
                            .await
                            .with_context(|| format!("init MCP session with {label}"))?;
                        let tools = service
                            .list_all_tools()
                            .await
                            .with_context(|| format!("list_tools on {label}"))?;
                        let mut tool_names = std::collections::HashSet::new();
                        for t in tools {
                            let name = t.name.to_string();
                            if routes.contains_key(&name) || tool_names.contains(&name) {
                                anyhow::bail!("duplicate tool `{name}` advertised by {label}");
                            }
                            tool_names.insert(name.clone());
                            defs.push(ToolDef {
                                name,
                                description: t.description.as_deref().unwrap_or("").to_string(),
                                input_schema: Value::Object(input_schema_value(t.input_schema)),
                            });
                        }
                        // Probe done — shut it down so the lazy spawn gets
                        // a clean slate at first call. A failure here
                        // leaks the probe child until process exit, so
                        // log loudly even though we proceed: the lazy
                        // dispatch path can still spawn a fresh server
                        // and serve calls.
                        if let Err(e) = service.cancel().await {
                            tracing::error!(
                                "tool-bash probe shutdown failed for {label}: {e} \
                                 (probe child may linger until process exit)"
                            );
                        }
                        if lazy_bash.is_some() {
                            anyhow::bail!(
                                "multiple tool-bash endpoints configured; only one is supported"
                            );
                        }
                        lazy_bash = Some(LazyBash {
                            tool_names,
                            config: BashSpawnConfig {
                                command: command.clone(),
                                args: args.clone(),
                                project_root: project_root.to_path_buf(),
                                sandbox_template: sandbox.clone(),
                            },
                            resolver: Arc::new(RwLock::new(bash_net_resolver.clone())),
                            active: Mutex::new(None),
                        });
                    } else {
                        let label = command.display().to_string();
                        let mut cmd = tokio::process::Command::new(command);
                        cmd.args(args);
                        cmd.env("SAVVAGENT_TOOL_FS_ROOT", project_root);
                        cmd.env("SAVVAGENT_TOOL_BASH_ROOT", project_root);
                        cmd.env("SAVVAGENT_TOOL_GREP_ROOT", project_root);

                        let wrapper = apply_sandbox(&mut cmd, command, project_root, sandbox);
                        let allow_net = sandbox.net_allowed_for(command);
                        log_sandbox_wrapper(&label, &wrapper, allow_net, sandbox.is_enabled());
                        redirect_tool_stderr(&mut cmd, command);

                        let transport = TokioChildProcess::new(cmd)
                            .with_context(|| format!("spawn tool server: {label}"))?;
                        let handler =
                            ResourceCapturingHandler::new(label.clone(), resource_tx.clone());
                        let service = handler
                            .serve(transport)
                            .await
                            .with_context(|| format!("init MCP session with {label}"))?;
                        let tools = service
                            .list_all_tools()
                            .await
                            .with_context(|| format!("list_tools on {label}"))?;
                        let idx = eager_servers.len();
                        for t in tools {
                            let name = t.name.to_string();
                            if routes.insert(name.clone(), idx).is_some() {
                                anyhow::bail!("duplicate tool `{name}` advertised by {label}");
                            }
                            defs.push(ToolDef {
                                name,
                                description: t.description.as_deref().unwrap_or("").to_string(),
                                input_schema: Value::Object(input_schema_value(t.input_schema)),
                            });
                        }
                        eager_servers.push(ToolServer { label, service });
                    }
                }
            }
        }

        tracing::debug!(
            "connected to {} eager tool server(s){}, {} tool(s) total",
            eager_servers.len(),
            if lazy_bash.is_some() {
                " + lazy tool-bash"
            } else {
                ""
            },
            defs.len()
        );

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
            }),
        });

        Ok(Self {
            eager_servers,
            routes,
            defs,
            lazy_bash,
            in_process: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            in_process_defs: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        })
    }

    /// Trusted shell-availability flag for the default-prompt builder.
    /// True iff the embedder wired a `tool-bash`-marker endpoint in
    /// `HostConfig::tools`. Cannot be flipped by tool-server-supplied
    /// data (e.g. a third-party tool advertising `name == "run"`).
    pub(crate) fn bash_available(&self) -> bool {
        self.lazy_bash.is_some()
    }

    /// Test-only constructor for an empty registry. Mirrors what
    /// `connect()` produces when given a `HostConfig` with no tools.
    #[cfg(test)]
    pub(crate) fn empty_for_test() -> Arc<Self> {
        Arc::new(Self {
            eager_servers: Vec::new(),
            routes: HashMap::new(),
            defs: Vec::new(),
            lazy_bash: None,
            in_process: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            in_process_defs: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        })
    }

    /// Register an in-process tool handler under `spec.name`. Subsequent
    /// calls to [`Self::call_in_process`] with that name dispatch to
    /// `handler`; `spec` is exposed via [`Self::tool_defs`] so the
    /// provider sees the tool in the model-facing tool list.
    ///
    /// If a handler is already registered for `spec.name`, it is replaced.
    ///
    /// Called from the TUI by `apply_pending_in_process_tools` in
    /// response to an `Effect::RegisterInProcessTool`.
    pub async fn register_in_process_tool(
        &self,
        spec: ToolDef,
        handler: savvagent_plugin::InProcessToolHandlerArc,
    ) {
        let name = spec.name.clone();
        let mut handlers = self.in_process.write().await;
        handlers.insert(name.clone(), handler);
        drop(handlers);
        let mut defs = self.in_process_defs.write().await;
        defs.insert(name, spec);
    }

    /// Snapshot of all tool definitions known to this registry: the
    /// stdio-served ones (eager + lazy bash, plus the synthetic
    /// `read_resource`) followed by any in-process tools registered via
    /// [`Self::register_in_process_tool`]. Used by the host when
    /// building the provider's tool list and by
    /// [`crate::SubHost`] callers (e.g. `TaskToolHandler`) that need
    /// the full parent-tool view to compute a per-subagent allowlist.
    pub async fn tool_defs(&self) -> Vec<ToolDef> {
        let mut out: Vec<ToolDef> = self.defs.clone();
        let in_proc = self.in_process_defs.read().await;
        for def in in_proc.values() {
            out.push(def.clone());
        }
        out
    }

    /// Dispatch an in-process tool call. The handler receives the
    /// caller-supplied `ctx` as-is (typically downcast to
    /// `Arc<ToolCallContext>`).
    ///
    /// Currently unused outside tests; the SubHost dispatch path
    /// (Task 7+) will route through here when the tool name resolves
    /// to an in-process handler.
    #[allow(dead_code)]
    pub(crate) async fn call_in_process(
        &self,
        name: &str,
        input: serde_json::Value,
        ctx: std::sync::Arc<dyn std::any::Any + Send + Sync>,
    ) -> Result<serde_json::Value, String> {
        let handler = {
            let guard = self.in_process.read().await;
            let Some(arc) = guard.get(name) else {
                return Err(format!("unknown in-process tool: {name}"));
            };
            arc.clone()
        };
        handler.as_arc().call(input, ctx).await
    }

    /// True iff `name` is registered as an in-process tool. Used by
    /// callers that want to choose between [`Self::call_in_process`]
    /// and the stdio-path [`Self::call_with_bash_net_override`].
    ///
    /// Currently unused outside tests; the SubHost dispatch path
    /// (Task 7+) will consult this to choose the right entry point.
    #[allow(dead_code)]
    pub(crate) async fn in_process_has(&self, name: &str) -> bool {
        self.in_process.read().await.contains_key(name)
    }

    /// Call `name` with a per-call bash network override.
    ///
    /// For non-bash tools, `net_override` is ignored. For bash tools, the
    /// override is passed to the resolver and to the spawn logic; see
    /// [`LazyBash`] for the spawn-vs-reuse decision.
    pub(crate) async fn call_with_bash_net_override(
        &self,
        name: &str,
        input: Value,
        net_override: NetOverride,
    ) -> ToolCallOutcome {
        // In-process tools take precedence over stdio routes. Callers
        // who hit this path lack a `ToolCallContext`, so this is a
        // guardrail to surface the bug rather than fail open.
        if self.in_process.read().await.contains_key(name) {
            return ToolCallOutcome::error(format!(
                "in-process tool `{name}` must be dispatched via call_in_process"
            ));
        }

        // Validate args shape up-front; both paths need it as an object.
        let args = match input {
            Value::Object(m) => m,
            other => {
                return ToolCallOutcome::error(format!(
                    "tool `{name}` arguments must be a JSON object, got {}",
                    discriminant(&other)
                ));
            }
        };

        // Lazy bash path first — it owns the `run` tool name.
        if let Some(lazy) = self.lazy_bash.as_ref()
            && lazy.tool_names.contains(name)
        {
            return lazy.dispatch(name, args, net_override).await;
        }

        // Eager path for everything else.
        let Some(&idx) = self.routes.get(name) else {
            return ToolCallOutcome::error(format!("unknown tool: {name}"));
        };
        let server = &self.eager_servers[idx];
        let params = CallToolRequestParams::new(name.to_string()).with_arguments(args);
        ToolCallOutcome::from_call_result(name, server.service.call_tool(params).await)
    }

    /// Replace the bash network resolver. Used by [`crate::session::Host`]
    /// after construction so the resolver can capture `Arc`-shared
    /// handles to the host's permission state and emit
    /// [`crate::session::TurnEvent::BashNetworkRequested`].
    ///
    /// No-op when no `tool-bash` endpoint is configured. Logged at
    /// `debug!` so a misconfigured host (caller expects bash but no
    /// endpoint was wired) leaves a trail rather than a silent drop.
    pub(crate) fn install_bash_net_resolver(&self, resolver: BashNetResolverHandle) {
        match self.lazy_bash.as_ref() {
            Some(lazy) => {
                *lazy.resolver.write().expect("resolver lock poisoned") = resolver;
            }
            None => {
                tracing::debug!(
                    "install_bash_net_resolver called on a host without a configured \
                     tool-bash endpoint — no-op; bash-network prompts will not fire"
                );
            }
        }
    }

    /// Cancel each tool server session, draining its child process.
    pub(crate) async fn shutdown(self) {
        for s in self.eager_servers {
            if let Err(e) = s.service.cancel().await {
                tracing::warn!("error closing tool server {}: {e}", s.label);
            }
        }
        if let Some(lazy) = self.lazy_bash {
            let mut guard = lazy.active.lock().await;
            if let Some(active) = guard.take()
                && let Err(e) = active.service.cancel().await
            {
                tracing::warn!("error closing lazy tool-bash {}: {e}", active.label);
            }
        }
    }

    /// Dispatch the synthetic `read_resource` tool. Looks up the owning
    /// tool server in `eager_servers` (resources can't come from bash —
    /// bash is request/response only — so we don't consult the lazy slot).
    pub(crate) async fn dispatch_read_resource(&self, uri: &str, owner: &str) -> ToolCallOutcome {
        let server = self.eager_servers.iter().find(|s| s.label == owner);
        let Some(server) = server else {
            return ToolCallOutcome::error(format!(
                "unknown resource owner: {owner}; no tool advertises this URI ({uri})"
            ));
        };
        // rmcp's RunningService exposes read_resource via peer().
        // `ReadResourceRequestParams` is `#[non_exhaustive]` outside its
        // defining crate — use the `::new` constructor.
        let req = rmcp::model::ReadResourceRequestParams::new(uri.to_string());
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
                ToolCallOutcome::error(format!("read_resource failed for {uri} on {owner}: {err}"))
            }
        }
    }
}

impl LazyBash {
    /// Resolve `allow_net` for this call, (re)spawn the bash server if
    /// needed, and dispatch the tool call to it.
    async fn dispatch(
        &self,
        name: &str,
        args: serde_json::Map<String, Value>,
        net_override: NetOverride,
    ) -> ToolCallOutcome {
        // Step 1: resolve the per-call allow_net via the host-supplied
        // resolver. This may emit a prompt and block until the user
        // answers — hence why we run it before taking the active-server
        // lock. We snapshot the current resolver under a brief read lock,
        // then drop the lock before awaiting so the host can swap the
        // resolver freely. The trait's default `resolve` impl handles the
        // explicit-override short-circuit, so we don't need to repeat that
        // logic here.
        let resolver = self
            .resolver
            .read()
            .expect("resolver lock poisoned")
            .clone();
        // Extract the bash `command` arg (if present) so the resolver
        // can put it in the prompt summary. Non-bash tools don't reach
        // this dispatch path; bash's `run` tool spec defines `command`
        // as a string. Other shapes (missing key, non-string) fall back
        // to None and the static summary line.
        let command_for_prompt = args.get("command").and_then(Value::as_str);
        let context = BashNetContext {
            command: command_for_prompt,
        };
        let allow_net = resolver.resolve(net_override, context).await;
        let new_key = BashSpawnKey { allow_net };

        // Step 2: lock the active server slot so we get a single
        // ordering for the (reuse-or-respawn → dispatch) sequence.
        let mut guard = self.active.lock().await;
        let cached_key = guard.as_ref().map(|a| a.spawn_key.clone());
        let must_respawn = cached_key.as_ref() != Some(&new_key);

        if must_respawn {
            if let Some(prev) = &cached_key {
                tracing::info!(
                    "tool-bash: spawn key changed ({:?} -> {new_key:?}); respawning",
                    prev
                );
            }

            // Blue-green respawn: spawn the new server FIRST. If it
            // fails we keep the old one — the alternative (kill first,
            // then fail to spawn) would leave the slot empty and force
            // every subsequent call to attempt a fresh spawn from cold.
            let cmd = build_bash_command(
                &self.config.command,
                &self.config.args,
                &self.config.project_root,
                &self.config.sandbox_template,
                allow_net,
            );
            let label = self.config.command.display().to_string();
            let transport = match TokioChildProcess::new(cmd) {
                Ok(t) => t,
                Err(e) => {
                    return ToolCallOutcome::error(format!(
                        "spawn tool-bash ({label}, allow_net={allow_net}): {e}"
                    ));
                }
            };
            let new_service = match ().serve(transport).await {
                Ok(s) => s,
                Err(e) => {
                    return ToolCallOutcome::error(format!(
                        "init MCP session with tool-bash ({label}, allow_net={allow_net}): {e}"
                    ));
                }
            };

            // New server is up. Now we can safely retire the old one.
            if let Some(prev) = guard.take()
                && let Err(e) = prev.service.cancel().await
            {
                tracing::warn!(
                    "lazy tool-bash respawn: error cancelling previous server {} \
                     ({:?}): {e} (ignored — new server already up)",
                    prev.label,
                    prev.spawn_key,
                );
            }

            *guard = Some(ActiveBashServer {
                label,
                service: new_service,
                spawn_key: new_key,
            });
            tracing::debug!("lazy tool-bash: (re)spawned with allow_net={allow_net}");
        }

        // Step 3: dispatch the call. `guard.as_ref().unwrap()` is safe — we
        // just populated it above (or confirmed an existing entry).
        let active = guard.as_ref().expect("active bash server present");
        let params = CallToolRequestParams::new(name.to_string()).with_arguments(args);
        ToolCallOutcome::from_call_result(name, active.service.call_tool(params).await)
    }
}

/// Build a sandboxed `tokio::process::Command` for tool-bash with the
/// given `allow_net`. The injected per-spawn `tool_overrides[tool-bash]
/// .allow_net = Some(allow_net)` wins over the built-in default-deny
/// fallback in `SandboxConfig::net_allowed_for`.
fn build_bash_command(
    command: &Path,
    args: &[String],
    project_root: &Path,
    sandbox_template: &SandboxConfig,
    allow_net: bool,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args);
    cmd.env("SAVVAGENT_TOOL_FS_ROOT", project_root);
    cmd.env("SAVVAGENT_TOOL_BASH_ROOT", project_root);
    cmd.env("SAVVAGENT_TOOL_GREP_ROOT", project_root);

    // Clone the template and inject the per-spawn allow_net override. We
    // merge with any user-pinned `[tool_overrides.tool-bash]` entry —
    // user pin wins (matches the "user config overrides host runtime"
    // invariant). We only inject ours when the user did NOT pin one.
    let mut sandbox = sandbox_template.clone();
    let needs_inject = sandbox
        .tool_overrides
        .get(TOOL_BASH_MARKER)
        .map(|ov| ov.allow_net.is_none())
        .unwrap_or(true);
    if needs_inject {
        sandbox
            .tool_overrides
            .entry(TOOL_BASH_MARKER.to_string())
            .or_default()
            .allow_net = Some(allow_net);
    }

    let wrapper = apply_sandbox(&mut cmd, command, project_root, &sandbox);
    let label = command.display().to_string();
    // The bash spawn path uses the merged-with-override `sandbox` config,
    // so its `is_enabled()` reflects the actual state for this spawn.
    log_sandbox_wrapper(&label, &wrapper, allow_net, sandbox.is_enabled());
    redirect_tool_stderr(&mut cmd, command);
    cmd
}

/// Log the resolved sandbox wrapper for a freshly built tool command.
/// Single source of truth for the eager and bash spawn paths so the log
/// format stays consistent.
///
/// `sandbox_enabled` distinguishes the two structurally different reasons
/// a wrapper resolves to [`SandboxWrapper::None`]:
///
/// - Sandboxing is disabled by config — nothing to say.
/// - Sandboxing is enabled but the wrapper binary (`bwrap` / `sandbox-exec`)
///   is unavailable — the tool is about to run **unwrapped despite the
///   user's opt-in**. That's a security-relevant degradation; emit
///   `tracing::error!` for every such spawn so it never goes silent.
///   ([`apply_sandbox`] also `warn!`s once per missing binary, but that
///   warning fires before the user might be paying attention; this one
///   fires on every spawn that's actually running unwrapped.)
fn log_sandbox_wrapper(
    label: &str,
    wrapper: &SandboxWrapper,
    allow_net: bool,
    sandbox_enabled: bool,
) {
    match wrapper {
        SandboxWrapper::None => {
            if sandbox_enabled {
                tracing::error!(
                    "sandbox enabled but tool `{label}` ran unwrapped \
                     (allow_net={allow_net}) — required wrapper binary not found"
                );
            }
        }
        SandboxWrapper::Bwrap => {
            tracing::info!("sandbox[bwrap]: {label} (allow_net={allow_net})");
        }
        SandboxWrapper::SandboxExec => {
            tracing::info!("sandbox[sandbox-exec]: {label} (allow_net={allow_net})");
        }
    }
}

/// Redirect a tool subprocess's stderr to a per-tool append log file under
/// `~/.savvagent/logs/tools/`. Falls back to `Stdio::null()` if the file
/// can't be opened — the invariant is "never inherit the TUI's terminal",
/// not "must log everything".
///
/// Must be called *after* [`apply_sandbox`] (which can replace the whole
/// `Command` with a `bwrap`/`sandbox-exec` wrapper and would otherwise drop
/// our stderr configuration).
fn redirect_tool_stderr(cmd: &mut tokio::process::Command, command: &Path) {
    let stderr = match tool_stderr_log_file(command) {
        Ok(file) => std::process::Stdio::from(file),
        Err(_) => std::process::Stdio::null(),
    };
    cmd.stderr(stderr);
}

/// Normalize the shape of an MCP tool result into a single `String` payload
/// suitable for embedding in a `tool_result` content block.
fn render_result_payload(result: &rmcp::model::CallToolResult) -> String {
    if let Some(v) = &result.structured_content {
        return serde_json::to_string(v).unwrap_or_else(|_| String::from("<unrenderable JSON>"));
    }
    let mut out = String::new();
    for c in &result.content {
        if let Some(t) = c.as_text() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&t.text);
        }
    }
    out
}

fn input_schema_value(arc: Arc<rmcp::model::JsonObject>) -> rmcp::model::JsonObject {
    Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone())
}

fn discriminant(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Outcome of one tool dispatch.
#[derive(Debug, Clone)]
pub(crate) struct ToolCallOutcome {
    /// True if the tool reported failure or transport error.
    pub is_error: bool,
    /// Text payload to embed in a `tool_result` content block.
    pub payload: String,
}

impl ToolCallOutcome {
    pub(crate) fn success(payload: String) -> Self {
        Self {
            is_error: false,
            payload,
        }
    }
    pub(crate) fn error(payload: String) -> Self {
        Self {
            is_error: true,
            payload,
        }
    }

    /// Normalize the outcome of `RunningService::call_tool` into a
    /// [`ToolCallOutcome`]. `name` is used only to format the
    /// transport-error message; the success path doesn't consult it.
    pub(crate) fn from_call_result(
        name: &str,
        result: std::result::Result<rmcp::model::CallToolResult, ServiceError>,
    ) -> Self {
        match result {
            Ok(r) => {
                let payload = render_result_payload(&r);
                if r.is_error == Some(true) {
                    Self::error(payload)
                } else {
                    Self::success(payload)
                }
            }
            Err(e) => {
                // Preserve the structured `ServiceError` (variant kind,
                // backtrace if any) for operators *before* we flatten
                // it into the LLM-facing payload. The model-facing
                // string is identical to before; this just adds an
                // operator-side trail.
                tracing::error!(tool = name, error = ?e, "tool transport error");
                Self::error(format!("tool transport error on {name}: {e}"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod lazy_bash_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Resolver that returns a fixed `allow_net` policy and counts invocations.
    /// The trait's default `resolve` short-circuits explicit overrides, so
    /// `resolve_policy` only runs when the caller passes `NetOverride::Inherit`.
    struct FixedResolver {
        policy: bool,
        invocations: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl BashNetResolver for FixedResolver {
        async fn resolve_policy(&self, _context: BashNetContext<'_>) -> bool {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            self.policy
        }
    }

    fn fixed_resolver(policy: bool, counter: Arc<AtomicUsize>) -> BashNetResolverHandle {
        Arc::new(FixedResolver {
            policy,
            invocations: counter,
        })
    }

    /// A degenerate `LazyBash` whose `dispatch` never actually spawns —
    /// it intercepts the spawn step so we can verify the spawn/respawn
    /// counting logic without requiring a real tool-bash binary on disk.
    ///
    /// We do this by hand-rolling a thin wrapper around the spawn
    /// decision. The real `LazyBash::dispatch` is exercised end-to-end
    /// in the integration test in `session.rs`.
    struct CountingBash {
        resolver: BashNetResolverHandle,
        active: Mutex<Option<BashSpawnKey>>,
        spawn_count: AtomicUsize,
    }

    impl CountingBash {
        async fn dispatch(&self, net_override: NetOverride) -> bool {
            let resolver = self.resolver.clone();
            let allow_net = resolver
                .resolve(net_override, BashNetContext::default())
                .await;
            let new_key = BashSpawnKey { allow_net };
            let mut guard = self.active.lock().await;
            let must_respawn = guard.as_ref() != Some(&new_key);
            if must_respawn {
                self.spawn_count.fetch_add(1, Ordering::SeqCst);
                *guard = Some(new_key);
            }
            allow_net
        }
    }

    #[tokio::test]
    async fn two_calls_same_allow_net_spawn_once() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bash = CountingBash {
            resolver: fixed_resolver(true, counter.clone()),
            active: Mutex::new(None),
            spawn_count: AtomicUsize::new(0),
        };

        assert!(bash.dispatch(NetOverride::Inherit).await);
        assert!(bash.dispatch(NetOverride::Inherit).await);

        assert_eq!(
            bash.spawn_count.load(Ordering::SeqCst),
            1,
            "two calls with the same resolved allow_net must reuse the cached spawn"
        );
    }

    #[tokio::test]
    async fn override_flip_respawns() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bash = CountingBash {
            // Resolver policy is `true`; per-call ForceDeny flips it.
            resolver: fixed_resolver(true, counter.clone()),
            active: Mutex::new(None),
            spawn_count: AtomicUsize::new(0),
        };

        // Call 1: ForceDeny → spawn with allow_net=false.
        assert!(!bash.dispatch(NetOverride::ForceDeny).await);
        // Call 2: Inherit → resolver returns true → respawn.
        assert!(bash.dispatch(NetOverride::Inherit).await);

        assert_eq!(
            bash.spawn_count.load(Ordering::SeqCst),
            2,
            "flipping allow_net between calls must force a respawn"
        );
    }

    #[tokio::test]
    async fn force_allow_short_circuits_policy() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bash = CountingBash {
            // Resolver policy is `false` — but ForceAllow never asks.
            resolver: fixed_resolver(false, counter.clone()),
            active: Mutex::new(None),
            spawn_count: AtomicUsize::new(0),
        };

        assert!(bash.dispatch(NetOverride::ForceAllow).await);

        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "ForceAllow must NOT call resolve_policy — the override short-circuits"
        );
    }

    #[tokio::test]
    async fn force_deny_short_circuits_policy() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bash = CountingBash {
            resolver: fixed_resolver(true, counter.clone()),
            active: Mutex::new(None),
            spawn_count: AtomicUsize::new(0),
        };

        assert!(!bash.dispatch(NetOverride::ForceDeny).await);

        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "ForceDeny must NOT call resolve_policy — the override short-circuits"
        );
    }

    #[tokio::test]
    async fn override_matches_cached_no_respawn() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bash = CountingBash {
            resolver: fixed_resolver(true, counter.clone()),
            active: Mutex::new(None),
            spawn_count: AtomicUsize::new(0),
        };

        // Call 1: Inherit → resolver true → spawn(true).
        assert!(bash.dispatch(NetOverride::Inherit).await);
        // Call 2: ForceAllow → matches active → reuse.
        assert!(bash.dispatch(NetOverride::ForceAllow).await);

        assert_eq!(
            bash.spawn_count.load(Ordering::SeqCst),
            1,
            "matching override should reuse the cached spawn"
        );
    }

    #[test]
    fn bash_spawn_key_equality_discriminates_allow_net() {
        // Pin the contract `LazyBash::dispatch` relies on: two keys
        // compare equal iff they describe an interchangeable spawn.
        // When this struct grows new fields (e.g. v0.9 domain allowlist),
        // a forgotten `PartialEq` derive line — or a hand-rolled `Eq`
        // impl that misses the new field — would make the new variant
        // collapse into an existing cache slot. This test catches that.
        assert_eq!(
            BashSpawnKey { allow_net: true },
            BashSpawnKey { allow_net: true }
        );
        assert_eq!(
            BashSpawnKey { allow_net: false },
            BashSpawnKey { allow_net: false }
        );
        assert_ne!(
            BashSpawnKey { allow_net: true },
            BashSpawnKey { allow_net: false }
        );
    }

    #[test]
    fn bash_available_false_when_no_lazy_bash_slot() {
        // Constructed directly from public-in-crate fields. `lazy_bash: None`
        // is the trusted-source signal that no `tool-bash` endpoint was wired
        // by the embedder; `bash_available()` must reflect that.
        let registry = ToolRegistry {
            eager_servers: Vec::new(),
            routes: HashMap::new(),
            defs: Vec::new(),
            lazy_bash: None,
            in_process: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            in_process_defs: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        };
        assert!(!registry.bash_available());
    }
}

#[cfg(test)]
mod resource_handler_tests {
    use super::*;
    use rmcp::model::ResourceUpdatedNotificationParam;
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

#[cfg(test)]
mod tool_call_outcome_tests {
    use super::*;
    use rmcp::model::{CallToolResult, Content};

    #[test]
    fn from_call_result_success_produces_non_error_outcome() {
        // CallToolResult::success sets is_error = Some(false). The
        // `is_error == None` branch in `from_call_result` is unreachable
        // via this public constructor but folds into the same arm — its
        // behavior matches Some(false) by construction.
        let result = CallToolResult::success(vec![Content::text("hello".to_string())]);
        let outcome = ToolCallOutcome::from_call_result("greet", Ok(result));
        assert!(!outcome.is_error);
        assert_eq!(outcome.payload, "hello");
    }

    #[test]
    fn from_call_result_error_flag_produces_error_outcome() {
        let result = CallToolResult::error(vec![Content::text("tool blew up".to_string())]);
        let outcome = ToolCallOutcome::from_call_result("greet", Ok(result));
        assert!(
            outcome.is_error,
            "MCP-side `is_error: true` must flow through to ToolCallOutcome::is_error"
        );
        assert_eq!(outcome.payload, "tool blew up");
    }

    #[test]
    fn from_call_result_transport_err_carries_tool_name_in_payload() {
        // TransportClosed is the simplest ServiceError variant to
        // construct in a test (unit variant, no payload). The branch
        // under test is "any Err -> Self::error with tool name", which
        // doesn't depend on which ServiceError variant is passed.
        let err = ServiceError::TransportClosed;
        let outcome = ToolCallOutcome::from_call_result("greet", Err(err));
        assert!(outcome.is_error);
        assert!(
            outcome.payload.contains("greet"),
            "transport-error payload must name the tool for operator triage: {}",
            outcome.payload
        );
        assert!(
            outcome.payload.contains("transport error"),
            "transport-error payload must self-identify: {}",
            outcome.payload
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subagent_context_carries_depth_and_name() {
        let ctx = SubagentContext {
            depth: 1,
            agent_name: "code-reviewer".into(),
            parent_session_id: "abc-123".into(),
        };
        assert_eq!(ctx.depth, 1);
        assert_eq!(ctx.agent_name, "code-reviewer");
    }

    #[test]
    fn tool_call_context_struct_compiles() {
        fn _accepts_ctx(_ctx: ToolCallContext) {}
        // Type-only smoke; no construction (requires Arc<Host> which is impractical here).
    }

    #[tokio::test]
    async fn registry_routes_in_process_tool() {
        use async_trait::async_trait;
        use savvagent_plugin::{InProcessToolHandler, InProcessToolHandlerArc};
        use serde_json::{Value, json};
        use std::any::Any;
        use std::sync::Arc;

        struct Echo;

        #[async_trait]
        impl InProcessToolHandler for Echo {
            async fn call(
                &self,
                input: Value,
                _ctx: Arc<dyn Any + Send + Sync>,
            ) -> Result<Value, String> {
                Ok(input)
            }
        }

        let registry = ToolRegistry::empty_for_test();
        let spec = ToolDef {
            name: "echo".into(),
            description: "echo input".into(),
            input_schema: json!({"type": "object"}),
        };
        registry
            .register_in_process_tool(spec, InProcessToolHandlerArc::new(Echo))
            .await;

        let ctx: Arc<dyn Any + Send + Sync> = Arc::new(()) as Arc<dyn Any + Send + Sync>;
        let outcome = registry
            .call_in_process("echo", json!({"hi": 1}), ctx)
            .await;
        assert!(outcome.is_ok(), "call_in_process returned: {outcome:?}");
        assert_eq!(outcome.unwrap(), json!({"hi": 1}));
    }
}
