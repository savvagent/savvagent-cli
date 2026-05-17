//! Per-turn routing decisions for the multi-provider pool.
//!
//! The router takes the current request, a snapshot of the connected
//! pool, and any explicit override parsed from a `@`-prefix, and returns
//! a `(provider, model, reason)` triple that the host pins for the
//! duration of the user turn.
//!
//! Phase 3 ships only two of the five planned layers:
//!
//! - Layer 1 — `@provider[:model]` override (Override reason)
//! - Layer 5 — fall through to the active provider + its default model
//!             (Default reason)
//!
//! Layers 2-4 (modality, user rules, heuristics) are reserved for
//! Phases 4-6; `RoutingReason` is `#[non_exhaustive]` so adding them
//! later is additive, not breaking.

use savvagent_protocol::ProviderId;

/// An explicit routing override the user expressed via an `@`-prefix.
/// Always wins over every other layer in [`Router::pick`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingOverride {
    /// The provider the user named (or that an alias resolved to).
    pub provider: ProviderId,
    /// The model the user named. `None` means "use this provider's
    /// default model" (the `@provider <rest>` form).
    pub model: Option<String>,
}

/// Why the router picked the provider/model it did. Surfaced in the
/// transcript badge so the user can always answer "why did it pick that?".
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RoutingReason {
    /// The user supplied an explicit `@`-prefix that resolved cleanly.
    Override,
    /// No higher-priority layer matched; fell through to the active
    /// provider + its default model.
    Default,
}

impl std::fmt::Display for RoutingReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoutingReason::Override => f.write_str("Override"),
            RoutingReason::Default => f.write_str("Default"),
        }
    }
}

/// What the router decided. Pinned for the duration of a user turn,
/// including every tool-use iteration within that turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingDecision {
    /// Provider that will handle this turn.
    pub provider_id: ProviderId,
    /// Model that will handle this turn.
    pub model_id: String,
    /// Why the router picked this `(provider, model)` pair.
    pub reason: RoutingReason,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_reason_displays() {
        assert_eq!(format!("{}", RoutingReason::Override), "Override");
        assert_eq!(format!("{}", RoutingReason::Default), "Default");
    }

    #[test]
    fn routing_override_constructs() {
        let p = ProviderId::new("anthropic").unwrap();
        let o = RoutingOverride {
            provider: p.clone(),
            model: Some("claude-opus-4-7".into()),
        };
        assert_eq!(o.provider, p);
        assert_eq!(o.model.as_deref(), Some("claude-opus-4-7"));
    }

    #[test]
    fn routing_decision_constructs() {
        let p = ProviderId::new("anthropic").unwrap();
        let d = RoutingDecision {
            provider_id: p.clone(),
            model_id: "claude-opus-4-7".into(),
            reason: RoutingReason::Override,
        };
        assert_eq!(d.provider_id, p);
        assert_eq!(d.model_id, "claude-opus-4-7");
        assert_eq!(d.reason, RoutingReason::Override);
    }
}
