//! Layer 4 of the router stack — hardcoded heuristic classifier.
//!
//! Gated on `RoutingRules::heuristics == true`. Pure functions, no I/O,
//! no async. Adding new `HeuristicKind` variants is additive thanks to
//! `#[non_exhaustive]`.

use crate::capabilities::CostTier;
use crate::router::ProviderView;
use crate::router::rules::DefaultPick;
use savvagent_protocol::ProviderId;

/// Coding-flavored substring keywords (lowercase). Substring (not
/// whole-word) match — `function` matches `functional`, `refactor`
/// matches `refactored`. List is hardcoded in v1; users who want a
/// different list write explicit `[[rule]]` entries (rules run before
/// the heuristic, so a rule match always beats this).
const CODING_KEYWORDS: &[&str] = &[
    "refactor",
    "implement",
    "debug",
    "fix bug",
    "compile",
    "stack trace",
    "function",
    "class",
    "error",
];

/// Max character length for a turn to qualify as a short factoid.
const SHORT_FACTOID_MAX_CHARS: usize = 200;

/// What the classifier matched. `#[non_exhaustive]` so future kinds
/// (translation, summarization, …) land additively.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeuristicKind {
    /// Short question, e.g. "what is 2+2?". Routes to a cheap model.
    ShortFactoid,
    /// Coding-flavored instruction. Routes to a premium model.
    Coding,
}

impl std::fmt::Display for HeuristicKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            HeuristicKind::ShortFactoid => "short",
            HeuristicKind::Coding => "coding",
        })
    }
}

/// Classify a user message. `None` = no heuristic match; the router
/// falls through to the next layer (Default).
///
/// Precedence inside the classifier:
/// 1. Coding keyword present → `Coding` (more specific signal wins).
/// 2. ≤200 chars and contains '?' → `ShortFactoid`.
/// 3. Else `None`.
pub fn classify(user_text: &str) -> Option<HeuristicKind> {
    let lower = user_text.to_lowercase();
    if CODING_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
        return Some(HeuristicKind::Coding);
    }
    if user_text.contains('?') && user_text.chars().count() <= SHORT_FACTOID_MAX_CHARS {
        return Some(HeuristicKind::ShortFactoid);
    }
    None
}

/// Pick a `(provider, model)` for a classified turn. Returns `None` when:
/// - The active provider's active model is already in the desired tier
///   (no-op routing — avoids `Heuristic(short)` badges on a Haiku session).
/// - No connected model matches the desired tier set.
///
/// Tier preferences:
/// - `ShortFactoid` → `[Free, Cheap]`, in that order.
/// - `Coding` → `[Premium, Standard]`, in that order.
///
/// Per-tier candidate ordering: active provider's models first (in
/// declaration order), then the rest of `providers` in input order.
pub fn pick_for_kind(
    kind: HeuristicKind,
    active_provider: &ProviderId,
    active_model: &str,
    providers: &[ProviderView<'_>],
) -> Option<DefaultPick> {
    let preferred_tiers: &[CostTier] = match kind {
        HeuristicKind::ShortFactoid => &[CostTier::Free, CostTier::Cheap],
        HeuristicKind::Coding => &[CostTier::Premium, CostTier::Standard],
    };

    // Short-circuit: if the active provider's active model is already
    // in the desired tier set, there's nothing to do.
    if let Some(active_view) = providers.iter().find(|p| *p.id == *active_provider)
        && let Some(m) = active_view.capabilities.model(active_model)
        && preferred_tiers.contains(&m.cost_tier)
    {
        return None;
    }

    for tier in preferred_tiers {
        // Active provider's models first, in declaration order.
        if let Some(active_view) = providers.iter().find(|p| *p.id == *active_provider)
            && let Some(m) = active_view
                .capabilities
                .models()
                .iter()
                .find(|m| &m.cost_tier == tier)
        {
            return Some(DefaultPick {
                provider: active_provider.clone(),
                model: m.id.clone(),
            });
        }
        // Then the rest of the pool, in `providers` order.
        for view in providers.iter().filter(|p| *p.id != *active_provider) {
            if let Some(m) = view
                .capabilities
                .models()
                .iter()
                .find(|m| &m.cost_tier == tier)
            {
                return Some(DefaultPick {
                    provider: view.id.clone(),
                    model: m.id.clone(),
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::router::heuristics::{classify, pick_for_kind, HeuristicKind};

    #[test]
    fn classify_returns_none_for_empty_input() {
        assert_eq!(classify(""), None);
        assert_eq!(classify("   "), None);
        assert_eq!(classify("hello"), None);
    }

    #[test]
    fn classify_short_factoid_requires_question_mark() {
        assert_eq!(classify("what is 2+2?"), Some(HeuristicKind::ShortFactoid));
        // Same text without a `?` is *not* a short factoid.
        assert_eq!(classify("what is 2+2"), None);
    }

    #[test]
    fn classify_short_factoid_respects_200_char_threshold() {
        assert_eq!(classify("is this short?"), Some(HeuristicKind::ShortFactoid));
        // 201 chars + `?` is over the cutoff → no match.
        let long = format!("is {}?", "x".repeat(200));
        assert_eq!(classify(&long), None);
    }

    #[test]
    fn classify_coding_matches_each_keyword_case_insensitive() {
        for kw in [
            "refactor",
            "implement",
            "debug",
            "fix bug",
            "compile",
            "stack trace",
            "function",
            "class",
            "error",
        ] {
            let upper = kw.to_uppercase();
            assert_eq!(
                classify(&format!("please {upper} this")),
                Some(HeuristicKind::Coding),
                "uppercase keyword '{upper}' should match Coding"
            );
            assert_eq!(
                classify(&format!("please {kw} this")),
                Some(HeuristicKind::Coding),
                "lowercase keyword '{kw}' should match Coding"
            );
        }
    }

    #[test]
    fn classify_coding_beats_short_factoid_when_both_match() {
        // 24 chars, contains `?`, AND contains the keyword `debug`.
        // The more specific signal (Coding) must win.
        assert_eq!(
            classify("can you debug this?"),
            Some(HeuristicKind::Coding)
        );
    }

    #[test]
    fn classify_substring_match_documented() {
        // `function` matches `functional`. This is the v1 contract;
        // whole-word matching is a future opt-in.
        assert_eq!(
            classify("functional programming"),
            Some(HeuristicKind::Coding)
        );
    }

    #[test]
    fn heuristic_kind_display() {
        assert_eq!(format!("{}", HeuristicKind::ShortFactoid), "short");
        assert_eq!(format!("{}", HeuristicKind::Coding), "coding");
    }

    use crate::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
    use crate::router::ProviderView;
    use savvagent_protocol::ProviderId;

    fn pid(s: &str) -> ProviderId {
        ProviderId::new(s).expect("valid provider id")
    }

    fn caps_with_tiers(models: &[(&str, CostTier)], default_idx: usize) -> ProviderCapabilities {
        ProviderCapabilities::new(
            models
                .iter()
                .map(|(id, tier)| ModelCapabilities {
                    id: (*id).into(),
                    display_name: (*id).into(),
                    supports_vision: false,
                    supports_audio: false,
                    context_window: 0,
                    cost_tier: tier.clone(),
                })
                .collect(),
            models[default_idx].0.into(),
        )
        .expect("valid caps")
    }

    #[test]
    fn pick_for_kind_short_factoid_prefers_cheap_then_free() {
        // anthropic: opus (Premium, default + active), haiku (Cheap)
        let a_id = pid("anthropic");
        let a_caps = caps_with_tiers(
            &[("opus", CostTier::Premium), ("haiku", CostTier::Cheap)],
            0,
        );
        let providers = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];

        let pick = pick_for_kind(HeuristicKind::ShortFactoid, &a_id, "opus", &providers)
            .expect("should pick");
        assert_eq!(pick.provider, a_id);
        assert_eq!(pick.model, "haiku");
    }

    #[test]
    fn pick_for_kind_coding_prefers_premium_then_standard() {
        // anthropic: haiku (Cheap, active), sonnet (Standard), opus (Premium)
        let a_id = pid("anthropic");
        let a_caps = caps_with_tiers(
            &[
                ("haiku", CostTier::Cheap),
                ("sonnet", CostTier::Standard),
                ("opus", CostTier::Premium),
            ],
            0,
        );
        let providers = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];

        let pick = pick_for_kind(HeuristicKind::Coding, &a_id, "haiku", &providers)
            .expect("should pick");
        assert_eq!(pick.provider, a_id);
        assert_eq!(pick.model, "opus");
    }

    #[test]
    fn pick_for_kind_returns_none_when_active_already_in_tier() {
        // ShortFactoid + active model is already Cheap → no-op
        let a_id = pid("anthropic");
        let a_caps = caps_with_tiers(
            &[("opus", CostTier::Premium), ("haiku", CostTier::Cheap)],
            1,
        );
        let providers = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];
        assert_eq!(
            pick_for_kind(HeuristicKind::ShortFactoid, &a_id, "haiku", &providers),
            None
        );
    }

    #[test]
    fn pick_for_kind_returns_none_when_no_tier_matches() {
        // ShortFactoid wants Free|Cheap; only Standard is connected.
        let a_id = pid("anthropic");
        let a_caps = caps_with_tiers(&[("sonnet", CostTier::Standard)], 0);
        let providers = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];
        assert_eq!(
            pick_for_kind(HeuristicKind::ShortFactoid, &a_id, "sonnet", &providers),
            None
        );
    }

    #[test]
    fn pick_for_kind_walks_pool_when_active_provider_has_no_match() {
        // ShortFactoid: active provider has only Standard; sibling has Cheap.
        let a_id = pid("anthropic");
        let g_id = pid("gemini");
        let a_caps = caps_with_tiers(&[("sonnet", CostTier::Standard)], 0);
        let g_caps = caps_with_tiers(&[("flash", CostTier::Cheap)], 0);
        let providers = vec![
            ProviderView {
                id: &a_id,
                capabilities: &a_caps,
            },
            ProviderView {
                id: &g_id,
                capabilities: &g_caps,
            },
        ];

        let pick = pick_for_kind(HeuristicKind::ShortFactoid, &a_id, "sonnet", &providers)
            .expect("should pick");
        assert_eq!(pick.provider, g_id);
        assert_eq!(pick.model, "flash");
    }

    #[test]
    fn pick_for_kind_prefers_active_provider_over_sibling_at_same_tier() {
        // Both active and sibling expose a Premium model. Active wins.
        let a_id = pid("anthropic");
        let g_id = pid("gemini");
        let a_caps = caps_with_tiers(&[("opus", CostTier::Premium)], 0);
        let g_caps = caps_with_tiers(&[("gemini-pro", CostTier::Premium)], 0);
        let providers = vec![
            ProviderView {
                id: &a_id,
                capabilities: &a_caps,
            },
            ProviderView {
                id: &g_id,
                capabilities: &g_caps,
            },
        ];

        // Active provider is anthropic with a non-Premium *active* model
        // ("synthetic" — not in catalog) so the picker doesn't short-circuit.
        let pick = pick_for_kind(HeuristicKind::Coding, &a_id, "synthetic-active", &providers)
            .expect("should pick");
        assert_eq!(pick.provider, a_id);
        assert_eq!(pick.model, "opus");
    }

    #[test]
    fn pick_for_kind_active_model_not_in_catalog_proceeds() {
        // Active model not in active provider's catalog (transient mismatch)
        // → treat as not-in-tier; pick the first matching tier.
        let a_id = pid("anthropic");
        let a_caps = caps_with_tiers(
            &[("opus", CostTier::Premium), ("haiku", CostTier::Cheap)],
            0,
        );
        let providers = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];

        let pick = pick_for_kind(HeuristicKind::ShortFactoid, &a_id, "ghost-model", &providers)
            .expect("should still pick");
        assert_eq!(pick.model, "haiku");
    }
}
