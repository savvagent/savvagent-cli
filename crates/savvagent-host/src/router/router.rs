//! Per-turn routing decisions for the multi-provider pool.
//!
//! The router takes the current request, a snapshot of the connected
//! pool, and any explicit override parsed from a `@`-prefix, and returns
//! a `(provider, model, reason)` triple that the host pins for the
//! duration of the user turn.
//!
//! Layers (first match wins):
//!
//! - Layer 1 — `@provider[:model]` override (Override reason)
//! - Layer 2 — required-modality redirect (Modality reason)
//! - Layer 3 — user rules from `~/.savvagent/routing.toml` (Rule reason)
//! - Layer 4 — heuristic classifier (not yet implemented)
//! - Layer 5 — fall through to the active provider + its default model
//!   (Default reason)
//!
//! `RoutingReason` is `#[non_exhaustive]` so adding the heuristic
//! variant later is additive, not breaking.

use savvagent_protocol::ProviderId;

use crate::router::modality;

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
    /// The user's input required a modality the current model doesn't
    /// support; the router redirected to a model that does.
    Modality {
        /// Which modality forced the redirect (e.g. `Image`).
        kind: modality::RequiredModalityKind,
    },
    /// A user-defined rule from `routing.toml` matched this turn.
    Rule {
        /// The matching rule's `name` field.
        name: String,
    },
    /// No higher-priority layer matched; fell through to the active
    /// provider + its default model.
    Default,
}

impl std::fmt::Display for RoutingReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoutingReason::Override => f.write_str("Override"),
            RoutingReason::Modality { kind } => write!(f, "Modality({kind})"),
            RoutingReason::Rule { name } => write!(f, "Rule({name})"),
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
    /// Layers (first match wins):
    /// 1. **Override** — `@`-prefix from the user input.
    /// 2. **Modality** — same-provider redirect when the active model
    ///    lacks a required modality.
    /// 3. **Rules** — first matching rule from `~/.savvagent/routing.toml`.
    /// 4. **Heuristic** — not yet implemented.
    /// 5. **Default** — active provider + active model.
    pub fn pick(
        override_: Option<RoutingOverride>,
        providers: &[crate::router::ProviderView<'_>],
        active_provider: &ProviderId,
        active_model: &str,
        required: modality::RequiredModalities,
        rules: &crate::router::rules::RoutingRules,
        user_text: &str,
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
            // Stale override; fall through.
        }

        if let Some(kind) = required.primary_kind()
            && let Some((p, m)) =
                modality::pick_vision_capable(required, active_provider, active_model, providers)
        {
            return RoutingDecision {
                provider_id: p,
                model_id: m,
                reason: RoutingReason::Modality { kind },
            };
        }

        // `providers` is the same `&[ProviderView]` the rules layer
        // needs — no extra allocation.
        let signals = crate::router::rules::RuleSignals {
            required,
            user_text,
        };
        if let Some((name, pick)) = rules.evaluate(&signals, providers) {
            return RoutingDecision {
                provider_id: pick.provider,
                model_id: pick.model,
                reason: RoutingReason::Rule { name },
            };
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
    use crate::router::modality::RequiredModalities;
    use crate::router::rules::RoutingRules;

    #[test]
    fn routing_reason_displays() {
        assert_eq!(format!("{}", RoutingReason::Override), "Override");
        assert_eq!(format!("{}", RoutingReason::Default), "Default");
    }

    #[test]
    fn routing_reason_modality_displays() {
        use crate::router::modality::RequiredModalityKind;
        let r = RoutingReason::Modality {
            kind: RequiredModalityKind::Image,
        };
        assert_eq!(format!("{r}"), "Modality(image)");
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

    fn caps_with_vision(model: &str, vision: bool) -> ProviderCapabilities {
        ProviderCapabilities::new(
            vec![ModelCapabilities {
                id: model.into(),
                display_name: model.into(),
                supports_vision: vision,
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

        let r = Router::pick(
            None,
            &views,
            &a_id,
            "claude-opus-4-7",
            RequiredModalities::default(),
            &RoutingRules::empty(),
            "",
        );
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

        let r = Router::pick(
            Some(override_),
            &views,
            &a_id,
            "claude-opus-4-7",
            RequiredModalities::default(),
            &RoutingRules::empty(),
            "",
        );
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

        let r = Router::pick(
            Some(override_),
            &views,
            &a_id,
            "claude-opus-4-7",
            RequiredModalities::default(),
            &RoutingRules::empty(),
            "",
        );
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

        let r = Router::pick(
            Some(stale_override),
            &views,
            &a_id,
            "claude-opus-4-7",
            RequiredModalities::default(),
            &RoutingRules::empty(),
            "",
        );
        assert_eq!(r.provider_id, a_id);
        assert_eq!(r.model_id, "claude-opus-4-7");
        assert_eq!(r.reason, RoutingReason::Default);
    }

    #[test]
    fn pick_modality_redirects_to_same_provider_sibling_model() {
        use crate::router::modality::RequiredModalityKind;
        // Active = anthropic default haiku (no vision); anthropic also has
        // opus (vision). Same-provider sibling wins.
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = ProviderCapabilities::new(
            vec![
                ModelCapabilities {
                    id: "haiku".into(),
                    display_name: "haiku".into(),
                    supports_vision: false,
                    supports_audio: false,
                    context_window: 0,
                    cost_tier: CostTier::Standard,
                },
                ModelCapabilities {
                    id: "opus".into(),
                    display_name: "opus".into(),
                    supports_vision: true,
                    supports_audio: false,
                    context_window: 0,
                    cost_tier: CostTier::Standard,
                },
            ],
            "haiku".into(),
        )
        .expect("valid caps");
        let views = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];

        let r = Router::pick(
            None,
            &views,
            &a_id,
            "haiku",
            RequiredModalities {
                has_image: true,
                ..Default::default()
            },
            &RoutingRules::empty(),
            "",
        );
        assert_eq!(r.provider_id, a_id);
        assert_eq!(r.model_id, "opus");
        assert_eq!(
            r.reason,
            RoutingReason::Modality {
                kind: RequiredModalityKind::Image
            }
        );
    }

    #[test]
    fn pick_override_wins_over_modality() {
        // @o3 + image attached. The user explicitly chose o3 (no vision).
        // The override must win — modality does not get to overrule a
        // user-typed override. Provider will return whatever error it
        // returns; the host emits a ModalityWarning in this case (Task 5).
        let a_id = ProviderId::new("anthropic").unwrap();
        let o_id = ProviderId::new("openai").unwrap();
        let a_caps = caps_with_vision("haiku", false);
        let o_caps = caps_with_vision("o3", false);
        let views = vec![
            ProviderView {
                id: &a_id,
                capabilities: &a_caps,
            },
            ProviderView {
                id: &o_id,
                capabilities: &o_caps,
            },
        ];
        let override_ = RoutingOverride {
            provider: o_id.clone(),
            model: Some("o3".into()),
        };
        let r = Router::pick(
            Some(override_),
            &views,
            &a_id,
            "haiku",
            RequiredModalities {
                has_image: true,
                ..Default::default()
            },
            &RoutingRules::empty(),
            "",
        );
        assert_eq!(r.provider_id, o_id);
        assert_eq!(r.model_id, "o3");
        assert_eq!(r.reason, RoutingReason::Override);
    }

    #[test]
    fn pick_modality_no_op_when_default_already_supports_vision() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = caps_with_vision("opus", true);
        let views = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];

        let r = Router::pick(
            None,
            &views,
            &a_id,
            "opus",
            RequiredModalities {
                has_image: true,
                ..Default::default()
            },
            &RoutingRules::empty(),
            "",
        );
        assert_eq!(r.provider_id, a_id);
        assert_eq!(r.model_id, "opus");
        assert_eq!(r.reason, RoutingReason::Default);
    }

    #[test]
    fn pick_modality_does_not_silently_cross_provider() {
        // Active = anthropic with no vision-capable model; gemini connected
        // with a vision model. The same-provider-only policy refuses the
        // silent cross-provider jump — falls through to Default.
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps_with_vision("haiku", false);
        let g_caps = caps_with_vision("flash", true);
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

        let r = Router::pick(
            None,
            &views,
            &a_id,
            "haiku",
            RequiredModalities {
                has_image: true,
                ..Default::default()
            },
            &RoutingRules::empty(),
            "",
        );
        assert_eq!(r.provider_id, a_id);
        assert_eq!(r.model_id, "haiku");
        assert_eq!(r.reason, RoutingReason::Default);
    }

    #[test]
    fn routing_reason_rule_displays() {
        let r = RoutingReason::Rule {
            name: "deep-reasoning".to_string(),
        };
        assert_eq!(format!("{r}"), "Rule(deep-reasoning)");
    }

    use crate::router::rules::{DefaultPick, RoutingRule, RuleMatch};

    fn rules_with_one_rule(name: &str, pick: DefaultPick, match_: RuleMatch) -> RoutingRules {
        RoutingRules {
            default: None,
            heuristics: false,
            rules: vec![RoutingRule {
                name: name.to_string(),
                match_,
                use_: pick,
            }],
        }
    }

    #[test]
    fn pick_rule_matches_and_routes() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps("haiku");
        let g_caps = caps("flash");
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
        let rules = rules_with_one_rule(
            "refactor",
            DefaultPick {
                provider: g_id.clone(),
                model: "flash".into(),
            },
            RuleMatch {
                keywords: vec!["refactor".into()],
                ..Default::default()
            },
        );
        let r = Router::pick(
            None,
            &views,
            &a_id,
            "haiku",
            RequiredModalities::default(),
            &rules,
            "please refactor this",
        );
        assert_eq!(r.provider_id, g_id);
        assert_eq!(r.model_id, "flash");
        assert_eq!(
            r.reason,
            RoutingReason::Rule {
                name: "refactor".into()
            }
        );
    }

    #[test]
    fn pick_override_beats_matching_rule() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps("haiku");
        let g_caps = caps("flash");
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
        let rules = rules_with_one_rule(
            "x",
            DefaultPick {
                provider: g_id.clone(),
                model: "flash".into(),
            },
            RuleMatch {
                keywords: vec!["x".into()],
                ..Default::default()
            },
        );
        let override_ = RoutingOverride {
            provider: a_id.clone(),
            model: Some("haiku".into()),
        };
        let r = Router::pick(
            Some(override_),
            &views,
            &a_id,
            "haiku",
            RequiredModalities::default(),
            &rules,
            "xenon",
        );
        assert_eq!(r.reason, RoutingReason::Override);
        assert_eq!(r.provider_id, a_id);
    }

    #[test]
    fn pick_modality_beats_matching_rule() {
        // Active = anthropic with both haiku (no vision) and opus (vision).
        // Image attached. A keyword rule also matches. Modality (Layer 2)
        // wins — rules run later in pick order.
        use crate::router::modality::RequiredModalityKind;
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = ProviderCapabilities::new(
            vec![
                ModelCapabilities {
                    id: "haiku".into(),
                    display_name: "haiku".into(),
                    supports_vision: false,
                    supports_audio: false,
                    context_window: 0,
                    cost_tier: CostTier::Standard,
                },
                ModelCapabilities {
                    id: "opus".into(),
                    display_name: "opus".into(),
                    supports_vision: true,
                    supports_audio: false,
                    context_window: 0,
                    cost_tier: CostTier::Standard,
                },
            ],
            "haiku".into(),
        )
        .expect("valid caps");
        let g_id = ProviderId::new("gemini").unwrap();
        let g_caps = caps("flash");
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
        let rules = rules_with_one_rule(
            "x",
            DefaultPick {
                provider: g_id.clone(),
                model: "flash".into(),
            },
            RuleMatch {
                keywords: vec!["x".into()],
                ..Default::default()
            },
        );
        let r = Router::pick(
            None,
            &views,
            &a_id,
            "haiku",
            RequiredModalities {
                has_image: true,
                ..Default::default()
            },
            &rules,
            "xenon",
        );
        assert_eq!(r.provider_id, a_id);
        assert_eq!(r.model_id, "opus");
        assert_eq!(
            r.reason,
            RoutingReason::Modality {
                kind: RequiredModalityKind::Image
            }
        );
    }

    #[test]
    fn pick_falls_through_when_rule_target_disconnected() {
        // Rule points at gemini; only anthropic connected. Rule is
        // silently skipped; Default fires.
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps("haiku");
        let views = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];
        let rules = rules_with_one_rule(
            "x",
            DefaultPick {
                provider: g_id,
                model: "flash".into(),
            },
            RuleMatch {
                keywords: vec!["x".into()],
                ..Default::default()
            },
        );
        let r = Router::pick(
            None,
            &views,
            &a_id,
            "haiku",
            RequiredModalities::default(),
            &rules,
            "xenon",
        );
        assert_eq!(r.reason, RoutingReason::Default);
        assert_eq!(r.provider_id, a_id);
    }

    #[test]
    fn pick_empty_rules_falls_through_to_default() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = caps("haiku");
        let views = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];
        let r = Router::pick(
            None,
            &views,
            &a_id,
            "haiku",
            RequiredModalities::default(),
            &RoutingRules::empty(),
            "anything",
        );
        assert_eq!(r.reason, RoutingReason::Default);
    }
}
