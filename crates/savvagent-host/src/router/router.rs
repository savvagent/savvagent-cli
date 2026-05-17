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
//!   (Default reason)
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

/// Layered router. Stateless — every `pick` call is independent of the
/// last; the host pins the result for the duration of a turn but the
/// router itself holds no per-conversation memory.
pub struct Router;

impl Router {
    /// Pick a `(provider, model, reason)` triple for a turn.
    ///
    /// Phase 3 active layers:
    /// - **Override** — if `override_` is `Some` and resolves to a
    ///   connected provider, use it. The model is the override's model
    ///   if specified, else the provider's default model.
    /// - **Default** — otherwise, use `active_provider` + `active_model`.
    ///
    /// A stale override that points at a now-disconnected provider falls
    /// through to Default (defensive — `parse_at_prefix` already filters
    /// these, but defending against a TOCTOU window between parse and
    /// pick is cheap).
    pub fn pick(
        override_: Option<RoutingOverride>,
        providers: &[crate::router::ProviderView<'_>],
        active_provider: &ProviderId,
        active_model: &str,
    ) -> RoutingDecision {
        if let Some(o) = override_ {
            if let Some(view) = providers.iter().find(|p| p.id == &o.provider) {
                let model_id = o
                    .model
                    .unwrap_or_else(|| view.capabilities.default_model_id().to_string());
                return RoutingDecision {
                    provider_id: o.provider,
                    model_id,
                    reason: RoutingReason::Override,
                };
            }
            // Stale override — provider gone since parse. Fall through.
        }
        RoutingDecision {
            provider_id: active_provider.clone(),
            model_id: active_model.to_string(),
            reason: RoutingReason::Default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
    use crate::router::ProviderView;

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

    fn caps(model: &str) -> ProviderCapabilities {
        ProviderCapabilities::new(
            vec![ModelCapabilities {
                id: model.into(),
                display_name: model.into(),
                supports_vision: false,
                supports_audio: false,
                context_window: 0,
                cost_tier: CostTier::Standard,
            }],
            model.into(),
        )
        .expect("valid caps")
    }

    #[test]
    fn pick_default_when_no_override() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = caps("claude-opus-4-7");
        let views = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];

        let r = Router::pick(None, &views, &a_id, "claude-opus-4-7");
        assert_eq!(r.provider_id, a_id);
        assert_eq!(r.model_id, "claude-opus-4-7");
        assert_eq!(r.reason, RoutingReason::Default);
    }

    #[test]
    fn pick_override_with_model() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps("claude-opus-4-7");
        let g_caps = caps("gemini-2.0-flash");
        let views = vec![
            ProviderView {
                id: &a_id,
                capabilities: &a_caps,
            },
            ProviderView {
                id: &g_id,
                capabilities: &g_caps,
            },
        ];
        let override_ = RoutingOverride {
            provider: g_id.clone(),
            model: Some("gemini-2.0-flash".into()),
        };

        let r = Router::pick(Some(override_), &views, &a_id, "claude-opus-4-7");
        assert_eq!(r.provider_id, g_id);
        assert_eq!(r.model_id, "gemini-2.0-flash");
        assert_eq!(r.reason, RoutingReason::Override);
    }

    #[test]
    fn pick_override_without_model_uses_provider_default() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps("claude-opus-4-7");
        let g_caps = caps("gemini-2.0-flash");
        let views = vec![
            ProviderView {
                id: &a_id,
                capabilities: &a_caps,
            },
            ProviderView {
                id: &g_id,
                capabilities: &g_caps,
            },
        ];
        let override_ = RoutingOverride {
            provider: g_id.clone(),
            model: None,
        };

        let r = Router::pick(Some(override_), &views, &a_id, "claude-opus-4-7");
        assert_eq!(r.provider_id, g_id);
        assert_eq!(r.model_id, "gemini-2.0-flash");
        assert_eq!(r.reason, RoutingReason::Override);
    }

    #[test]
    fn pick_override_for_disconnected_provider_falls_through() {
        // The @-parser already filters disconnected providers, so the
        // router's contract is "trust the override." But defending
        // against a stale override (provider just got disconnected
        // between parse and pick) keeps the host from panicking — fall
        // through to Default.
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps("claude-opus-4-7");
        let views = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];
        let stale_override = RoutingOverride {
            provider: g_id,
            model: None,
        };

        let r = Router::pick(Some(stale_override), &views, &a_id, "claude-opus-4-7");
        assert_eq!(r.provider_id, a_id);
        assert_eq!(r.model_id, "claude-opus-4-7");
        assert_eq!(r.reason, RoutingReason::Default);
    }
}
