//! Savvagent agent engine.
//!
//! `savvagent-host` is the runtime that the TUI links as a library. It owns
//! conversation state, drives the tool-use loop, and orchestrates connections
//! to one provider MCP server (over Streamable HTTP) and any number of tool
//! MCP servers (over stdio).
//!
//! Public surface:
//!
//! - [`HostConfig`] — declarative configuration: provider endpoint, tool
//!   endpoints, model, project root, system prompt overrides.
//! - [`Host`] — connect once via [`Host::start`], then call [`Host::run_turn`]
//!   for each user message. [`Host::shutdown`] cleans up child processes.
//! - [`TurnOutcome`] — final assistant response plus a per-turn trace of
//!   tool calls.
//! - [`HostError`] — top-level error type.

#![allow(clippy::collapsible_if)] // pre-existing debt; many sites under rustc 1.95 new lint
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod capabilities;
mod config;
pub mod pool;
pub mod resources;
pub mod router;
pub use pool::{DisconnectMode, PoolEntry, PoolError, ProviderLease};
pub use router::{
    BadModel, DefaultPick, HeuristicKind, LegacyModelResolution, ProviderView,
    ROUTING_RULES_SCHEMA_VERSION, RequiredModalities, RequiredModalityKind, Router,
    RoutingDecision, RoutingOverride, RoutingReason, RoutingRule, RoutingRules, RoutingRulesError,
    RuleMatch, RuleSignals, pick_vision_capable, required_modalities, resolve_legacy_model,
};
mod default_prompt;
mod logging;
mod permissions;
mod project;
mod provider;
mod sandbox;
mod scoped_registry;
pub mod sensitive_paths;
mod session;
mod subhost;
mod tools;

pub use capabilities::{
    CapabilitiesError, CostTier, ModelAlias, ModelCapabilities, ProviderCapabilities,
};
pub use config::{
    HostConfig, ProviderEndpoint, ProviderRegistration, StartupConnectPolicy, ToolEndpoint,
};
pub use default_prompt::AppVersion;
pub use permissions::{
    ArgPattern, BashNetworkChoice, BashNetworkPolicy, FrontMatterPermissions, PermissionDecision,
    PermissionPolicy, PermissionsToml, Rule, SerializableRule, Verdict,
};
pub use sandbox::{
    SCHEMA_VERSION, SandboxConfig, SandboxLoadStatus, SandboxMode, SandboxWrapper,
    ToolSandboxOverride, apply_sandbox,
};
pub use savvagent_protocol::{ListModelsResponse, ModelInfo, ToolDef};
pub use session::{
    BASH_NETWORK_PROMPT_SUMMARY, BashNetResolveError, CancellationReason, Host, HostError,
    TRANSCRIPT_SCHEMA_VERSION, ToolCall, ToolCallStatus, TranscriptError, TranscriptFile,
    TurnEvent, TurnOutcome,
};
pub use subhost::{SUBAGENT_NAME, SubHost, SubHostError, max_depth_from_env};
pub use tools::{BashNetContext, BashNetResolver, NetOverride, SubagentContext, ToolCallContext};

/// `PreToolUseGate` trait and `PreToolDecision` enum.
pub mod pre_tool_gate;
pub use pre_tool_gate::{PreToolDecision, PreToolUseGate};

#[doc(hidden)]
pub use provider::RmcpProviderClient;
