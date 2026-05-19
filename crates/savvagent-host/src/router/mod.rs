//! Routing layers. Phase 5 ships Layer 3 of the parent spec's router stack. The rules
//! are parsed once at `Host::start` and re-parsed on `/route reload`;
//! the evaluator is a pure function called from `Router::pick` after
//! `@`-override and modality have had their say.

pub mod legacy_model;
pub mod modality;
pub mod namespace;
pub mod prefix;
#[allow(clippy::module_inception)]
pub mod router;
pub mod rules;

pub use legacy_model::{LegacyModelResolution, ProviderView, resolve_legacy_model};
pub use modality::{
    RequiredModalities, RequiredModalityKind, pick_vision_capable, required_modalities,
};
pub use router::{Router, RoutingDecision, RoutingOverride, RoutingReason};
pub use rules::{
    DefaultPick, ROUTING_RULES_SCHEMA_VERSION, RoutingRule, RoutingRules, RoutingRulesError,
    RuleMatch, RuleSignals,
};
