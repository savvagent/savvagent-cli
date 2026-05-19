//! Routing layers. Owns the layered [`router::Router`] (override →
//! modality → rules → heuristic → default) plus the supporting modules
//! that each layer pulls in (rules from `~/.savvagent/routing.toml`,
//! modality detection, `@`-prefix parsing, heuristic classifier).

pub mod heuristics;
pub mod legacy_model;
pub mod modality;
pub mod namespace;
pub mod prefix;
#[allow(clippy::module_inception)]
pub mod router;
pub mod rules;

pub use heuristics::HeuristicKind;
pub use legacy_model::{LegacyModelResolution, ProviderView, resolve_legacy_model};
pub use modality::{
    RequiredModalities, RequiredModalityKind, pick_vision_capable, required_modalities,
};
pub use router::{Router, RoutingDecision, RoutingOverride, RoutingReason};
pub use rules::{
    BadModel, DefaultPick, ROUTING_RULES_SCHEMA_VERSION, RoutingRule, RoutingRules,
    RoutingRulesError, RuleMatch, RuleSignals,
};
