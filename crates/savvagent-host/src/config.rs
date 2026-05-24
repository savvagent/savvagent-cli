//! Host configuration types.

use std::path::PathBuf;
use std::sync::Arc;

use savvagent_mcp::ProviderClient;
use savvagent_protocol::ProviderId;

use crate::capabilities::{ModelAlias, ProviderCapabilities};
use crate::permissions::PermissionPolicy;
use crate::sandbox::SandboxConfig;

/// Where the host should reach the LLM provider.
#[derive(Debug, Clone)]
pub enum ProviderEndpoint {
    /// An SPP provider running as an MCP Streamable HTTP server (e.g.
    /// `provider-anthropic`'s `savvagent-anthropic` binary listening on a
    /// loopback port).
    StreamableHttp {
        /// Full URL ending in the MCP path, e.g. `http://127.0.0.1:8787/mcp`.
        url: String,
    },
}

/// How to launch a tool MCP server.
#[derive(Debug, Clone)]
pub enum ToolEndpoint {
    /// Spawn `command` with `args` as a child process and speak MCP over its
    /// stdin/stdout pipes.
    Stdio {
        /// Path to the binary (anything `tokio::process::Command::new` accepts).
        command: PathBuf,
        /// Arguments forwarded verbatim.
        args: Vec<String>,
    },
}

/// A connected provider handed to the host at construction time (or
/// later, via `Host::add_provider`). The plugin builds the `Arc<dyn
/// ProviderClient>` once and the host stores that same Arc — no Box→Arc
/// conversion exists anywhere in the system.
pub struct ProviderRegistration {
    /// Stable identifier for this provider (e.g. `"anthropic"`).
    pub id: ProviderId,
    /// Human-readable name for display in the UI.
    pub display_name: String,
    /// Pre-built client. Stored as `Arc` so the host and callers can
    /// share the same allocation without a Box→Arc round-trip.
    pub client: Arc<dyn ProviderClient + Send + Sync>,
    /// Capability metadata for this provider's models.
    pub capabilities: ProviderCapabilities,
    /// Short aliases that map bare names to specific models on this
    /// provider (e.g. `"opus"` → `"claude-opus-4-7"`).
    pub aliases: Vec<ModelAlias>,
}

impl ProviderRegistration {
    /// Construct a registration with no aliases. Use [`Self::with_aliases`]
    /// to attach model aliases afterwards.
    pub fn new(
        id: ProviderId,
        display_name: impl Into<String>,
        client: Arc<dyn ProviderClient + Send + Sync>,
        capabilities: ProviderCapabilities,
    ) -> Self {
        Self {
            id,
            display_name: display_name.into(),
            client,
            capabilities,
            aliases: Vec::new(),
        }
    }

    /// Attach model aliases to this registration.
    pub fn with_aliases(mut self, aliases: Vec<ModelAlias>) -> Self {
        self.aliases = aliases;
        self
    }
}

impl std::fmt::Debug for ProviderRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistration")
            .field("id", &self.id)
            .field("display_name", &self.display_name)
            .field("capabilities", &self.capabilities)
            .field("aliases", &self.aliases)
            .finish_non_exhaustive()
    }
}

/// Which providers should auto-connect when `Host::start` runs.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum StartupConnectPolicy {
    /// Only providers in this allow-list are auto-connected.
    OptIn(Vec<ProviderId>),
    /// Every provider in `HostConfig::providers` is auto-connected.
    All,
    /// Skip auto-connect entirely; pool starts empty.
    None,
    /// Auto-connect only the provider(s) recorded in
    /// `~/.savvagent/state.toml`'s `last_used` field. Resolution happens
    /// in the embedder (TUI) before the policy is built — by the time
    /// the host sees `LastUsed`, the inner vec is already populated.
    LastUsed(Vec<ProviderId>),
}

impl Default for StartupConnectPolicy {
    fn default() -> Self {
        Self::OptIn(Vec::new())
    }
}

/// Top-level host configuration.
// `Clone` is intentionally absent: `ProviderRegistration` holds an
// `Arc<dyn ProviderClient>` which could be cloned but shouldn't be — the host
// owns a single Arc per provider.  Debug is implemented manually for the same
// reason.
pub struct HostConfig {
    /// Legacy single-provider endpoint, used only when [`Self::providers`] is
    /// empty (rmcp HTTP debug transport).
    pub provider: ProviderEndpoint,
    /// Tool MCP servers to spawn at startup.
    pub tools: Vec<ToolEndpoint>,
    /// Provider-specific model identifier (forwarded in every request).
    pub model: String,
    /// Hard cap on response tokens for each `complete` call.
    pub max_tokens: u32,
    /// Project root, used to locate `SAVVAGENT.md` and as default cwd context.
    pub project_root: PathBuf,
    /// Optional embedder-supplied system-prompt layer. Composed (in
    /// order) after the built-in default prompt and before the
    /// `SAVVAGENT.md` body via [`crate::project::layered_prompt`].
    /// When `None`, only the default and `SAVVAGENT.md` layers
    /// contribute. The default layer can itself be suppressed via
    /// [`Self::with_default_prompt_disabled`].
    pub system_prompt: Option<String>,
    /// Hard cap on tool-use iterations within a single turn. Guards against
    /// pathological loops.
    pub max_iterations: u32,
    /// Permission policy override. `None` means the host builds a default
    /// policy ([`PermissionPolicy::default_for`]) from `project_root`.
    pub policy: Option<PermissionPolicy>,
    /// OS-level sandbox configuration for tool spawns (Layer 3).
    ///
    /// When `None`, the host loads `~/.savvagent/sandbox.toml` via
    /// [`SandboxConfig::load`]. Sandboxing is enabled by default on Linux and
    /// macOS as of v0.7; set the mode to [`SandboxMode::Off`] to disable, or
    /// run `/sandbox off` in the TUI.
    ///
    /// [`SandboxMode::Off`]: crate::SandboxMode::Off
    pub sandbox: Option<SandboxConfig>,

    /// When true (the default), `Host::start` builds and prepends a
    /// default system prompt that introduces Savvagent's identity,
    /// environment, and tool affordances. Disabling this suppresses
    /// **only the built-in default layer** — the [`Self::system_prompt`]
    /// override and the parsed `SAVVAGENT.md` body still compose. See
    /// [`Self::with_default_prompt_disabled`].
    pub default_prompt_enabled: bool,

    /// Embedder-supplied app version, surfaced in the default prompt's
    /// Environment section. When `None`, the prompt falls back to the
    /// `savvagent-host` crate version with an explicit "host crate"
    /// label. Stored as an owned `String` so embedders that compute
    /// the version at runtime (config file, plugin host wrapper) can
    /// pass it without lifetime acrobatics. See [`Self::with_app_version`].
    pub app_version: Option<String>,

    /// Providers handed in at construction time. Each entry becomes a
    /// `PoolEntry` in the host's provider pool, subject to
    /// `startup_connect`. The legacy `provider: ProviderEndpoint` field
    /// is preserved for the rmcp HTTP-transport debug path; when
    /// `providers` is non-empty, the host uses the pool and ignores
    /// the legacy field.
    pub providers: Vec<ProviderRegistration>,

    /// Which `providers` entries to actually connect at `Host::start`.
    pub startup_connect: StartupConnectPolicy,

    /// Per-provider timeout (milliseconds) for auto-connect during
    /// `Host::start`.
    pub connect_timeout_ms: u64,

    /// Grace period (milliseconds) for `DisconnectMode::Force` between
    /// emitting the cooperative cancel signal and aborting outstanding
    /// turn tasks.
    pub force_disconnect_grace_ms: u64,

    /// Filesystem path to the user's `routing.toml`. `None` means
    /// "don't load any rules; treat as `RoutingRules::empty()`." The
    /// TUI sets this to `~/.savvagent/routing.toml`; tests and the
    /// headless example pass `None`.
    pub routing_rules_path: Option<PathBuf>,

    /// Stable identifier for the constructed [`crate::Host`]. Surfaced to
    /// in-process tools via [`crate::ToolCallContext`] so subagent hooks
    /// can correlate across nesting levels. When `None`, the host
    /// generates a fresh UUID v4 at construction time. Embedders that
    /// want to thread an external session id through (e.g. a TUI session
    /// id) pass it here via [`Self::with_session_id`].
    pub session_id: Option<String>,
}

impl HostConfig {
    /// New config with sensible defaults: 4096 max tokens, 20 iteration cap,
    /// project root = current dir, no static tools, no system-prompt override,
    /// sandbox loaded from disk (`SandboxConfig::load`), default system
    /// prompt enabled (see [`Self::default_prompt_enabled`]), no embedder
    /// app version (see [`Self::with_app_version`]).
    pub fn new(provider: ProviderEndpoint, model: impl Into<String>) -> Self {
        Self {
            provider,
            tools: Vec::new(),
            model: model.into(),
            max_tokens: 4096,
            project_root: PathBuf::from("."),
            system_prompt: None,
            max_iterations: 20,
            policy: None,
            sandbox: None,
            default_prompt_enabled: true,
            app_version: None,
            providers: Vec::new(),
            startup_connect: StartupConnectPolicy::default(),
            connect_timeout_ms: 3000,
            force_disconnect_grace_ms: 500,
            routing_rules_path: None,
            session_id: None,
        }
    }

    /// Override the host session identifier. When unset, the host
    /// generates a fresh UUID v4 at construction time.
    pub fn with_session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }

    /// Add a tool endpoint to the config.
    pub fn with_tool(mut self, tool: ToolEndpoint) -> Self {
        self.tools.push(tool);
        self
    }

    /// Override the project root.
    pub fn with_project_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.project_root = path.into();
        self
    }

    /// Override the system prompt.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Override the max-tokens cap.
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    /// Override the iteration cap.
    pub fn with_max_iterations(mut self, n: u32) -> Self {
        self.max_iterations = n;
        self
    }

    /// Override the permission policy. When unset, the host builds
    /// [`PermissionPolicy::default_for(project_root)`] at startup.
    pub fn with_policy(mut self, policy: PermissionPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Override the sandbox configuration. When unset, the host loads
    /// `~/.savvagent/sandbox.toml` at startup. Sandboxing is enabled by
    /// default on Linux and macOS as of v0.7.
    pub fn with_sandbox(mut self, sandbox: SandboxConfig) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    /// Disable the built-in default-prompt layer. Does NOT disable
    /// the [`Self::system_prompt`] override or `SAVVAGENT.md` body —
    /// those still compose if present. To suppress every layer:
    /// call this AND leave `system_prompt` unset AND point
    /// `project_root` at a directory with no `SAVVAGENT.md`.
    pub fn with_default_prompt_disabled(mut self) -> Self {
        self.default_prompt_enabled = false;
        self
    }

    /// Set the app version label rendered in the default prompt's
    /// Environment section. Accepts anything convertible to `String`
    /// (`&'static str` literal, `String`, `&str` over a runtime value)
    /// so embedders that compute the version at runtime can pass it
    /// directly. Pass `env!("CARGO_PKG_VERSION")` from the binary the
    /// user actually launched so the version matches what they
    /// installed.
    pub fn with_app_version(mut self, version: impl Into<String>) -> Self {
        self.app_version = Some(version.into());
        self
    }
}

impl std::fmt::Debug for HostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostConfig")
            .field("provider", &self.provider)
            .field("tools", &self.tools)
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .field("project_root", &self.project_root)
            .field("system_prompt", &self.system_prompt)
            .field("max_iterations", &self.max_iterations)
            .field("policy", &self.policy)
            .field("sandbox", &self.sandbox)
            .field("default_prompt_enabled", &self.default_prompt_enabled)
            .field("app_version", &self.app_version)
            .field("providers", &self.providers)
            .field("startup_connect", &self.startup_connect)
            .field("connect_timeout_ms", &self.connect_timeout_ms)
            .field("force_disconnect_grace_ms", &self.force_disconnect_grace_ms)
            .field("routing_rules_path", &self.routing_rules_path)
            .finish()
    }
}

#[cfg(test)]
mod registration_tests {
    use super::*;
    use crate::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
    use async_trait::async_trait;
    use savvagent_mcp::ProviderClient;
    use savvagent_protocol::{
        CompleteRequest, CompleteResponse, ListModelsResponse, ProviderError, ProviderId,
        StreamEvent,
    };
    use std::sync::Arc;
    use tokio::sync::mpsc;

    struct StubClient;
    #[async_trait]
    impl ProviderClient for StubClient {
        async fn complete(
            &self,
            _: CompleteRequest,
            _: Option<mpsc::Sender<StreamEvent>>,
        ) -> Result<CompleteResponse, ProviderError> {
            unreachable!()
        }
        async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
            unreachable!()
        }
    }

    #[test]
    fn provider_registration_constructs() {
        let caps = ProviderCapabilities::new(
            vec![ModelCapabilities {
                id: "m".into(),
                display_name: "M".into(),
                supports_vision: false,
                supports_audio: false,
                context_window: 1000,
                cost_tier: CostTier::Standard,
            }],
            "m".into(),
        )
        .expect("valid test caps");
        let reg = ProviderRegistration::new(
            ProviderId::new("stub").unwrap(),
            "Stub",
            Arc::new(StubClient) as Arc<dyn ProviderClient + Send + Sync>,
            caps,
        );
        assert_eq!(reg.id.as_str(), "stub");
        assert_eq!(reg.display_name, "Stub");
    }

    #[test]
    fn startup_policy_defaults_to_opt_in() {
        let p = StartupConnectPolicy::default();
        assert!(matches!(p, StartupConnectPolicy::OptIn(ref v) if v.is_empty()));
    }

    #[test]
    fn host_config_has_pool_fields_with_defaults() {
        let cfg = HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "http://x".into(),
            },
            "model",
        );
        assert!(cfg.providers.is_empty());
        assert!(matches!(
            cfg.startup_connect,
            StartupConnectPolicy::OptIn(_)
        ));
        assert_eq!(cfg.connect_timeout_ms, 3000);
        assert_eq!(cfg.force_disconnect_grace_ms, 500);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> HostConfig {
        HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "http://localhost/mcp".into(),
            },
            "test-model",
        )
    }

    #[test]
    fn default_prompt_enabled_defaults_to_true() {
        assert!(cfg().default_prompt_enabled);
    }

    #[test]
    fn with_default_prompt_disabled_flips_flag() {
        let c = cfg().with_default_prompt_disabled();
        assert!(!c.default_prompt_enabled);
    }

    #[test]
    fn app_version_defaults_to_none() {
        assert!(cfg().app_version.is_none());
    }

    #[test]
    fn with_app_version_accepts_static_str_literal() {
        let c = cfg().with_app_version("9.9.9");
        assert_eq!(c.app_version.as_deref(), Some("9.9.9"));
    }

    #[test]
    fn with_app_version_accepts_runtime_owned_string() {
        // Embedders that read the version from a config file at runtime
        // must be able to pass a `String` — the builder takes
        // `impl Into<String>`, which accepts owned values without
        // requiring a `'static` lifetime.
        let runtime_value: String = format!("{}.{}.{}", 1, 2, 3);
        let c = cfg().with_app_version(runtime_value);
        assert_eq!(c.app_version.as_deref(), Some("1.2.3"));
    }
}
