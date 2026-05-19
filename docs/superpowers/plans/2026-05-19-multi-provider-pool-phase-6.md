# Multi-provider pool — Phase 6 (heuristic classifier) implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land Layer 4 of the router stack — a hardcoded heuristic classifier that, when the user opts in via `heuristics = true` in `~/.savvagent/routing.toml`, routes short-factoid turns (≤200 chars + `?`) to cheap models and coding-keyword turns to premium models. Override, Modality, and matching Rules still win when they apply.

**Architecture:**
- New host-side module `crates/savvagent-host/src/router/heuristics.rs` owns `HeuristicKind`, `classify(user_text)`, and `pick_for_kind(kind, active_provider, active_model, providers)`. Pure functions; no async, no I/O.
- `Router::pick` gains a Layer-4 step between Rules (Layer 3) and Default (Layer 5). No signature change — every input the classifier needs is already in scope.
- `RoutingReason::Heuristic { kind: HeuristicKind }` is the new variant on the existing `#[non_exhaustive]` enum.
- TUI's `render_routing_show` (in `crates/savvagent/src/main.rs`) swaps the Phase 5 "ships in a future release" placeholder for a new active-classifier description line when `heuristics = true`.
- Workspace version bumps to `0.20.0` (per-phase scaffolding; the actual tag rolls up all phases later per [[project_multi_provider_release.md]] in user memory).

**Tech Stack:** Rust 2024, Tokio, `async-trait`, `rust_i18n` (locale loading), `toml` (already in workspace). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-05-19-multi-provider-pool-phase-6-design.md`. Parent spec: `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`.

---

## File structure (Phase 6)

**New files:**
- `crates/savvagent-host/src/router/heuristics.rs` — `HeuristicKind`, `classify`, `pick_for_kind`, plus unit tests.
- `crates/savvagent-host/tests/heuristic_e2e.rs` — three end-to-end scenarios (short factoid, coding keyword, heuristic off).

**Modified files:**
- `crates/savvagent-host/src/router/mod.rs` — declare `heuristics` submodule + re-export `HeuristicKind`.
- `crates/savvagent-host/src/router/router.rs` — add `RoutingReason::Heuristic { kind: HeuristicKind }` variant; extend Display; add Layer-4 step inside `Router::pick`; add the seven new router-integration tests.
- `crates/savvagent-host/src/lib.rs` — re-export `HeuristicKind`.
- `crates/savvagent/src/main.rs` — replace the `routing.show-heuristics-pending` branch in `render_routing_show` with a new `routing.show-heuristics-active` branch gated on `rules.heuristics`; extend `render_routing_show_tests` with two new cases.
- `crates/savvagent/locales/en.toml`, `es.toml`, `pt.toml`, `hi.toml` — add `routing.show-heuristics-active` key under `[routing]`.
- `Cargo.toml` (workspace root) — bump `[workspace.package].version` to `0.20.0` and every `version = "0.19.0"` literal in `[workspace.dependencies]` to `0.20.0`.
- `CHANGELOG.md` — add `## 0.20.0 - 2026-05-19` entry.
- `README.md` — add a short "Heuristic classifier" subsection under the routing-rules section.

---

## Task 1: `HeuristicKind` + `classify` pure function

**Files:**
- Create: `crates/savvagent-host/src/router/heuristics.rs`
- Modify: `crates/savvagent-host/src/router/mod.rs`

Pure data + classification. No async, no `Router` change yet — the goal is to make `classify(user_text)` self-contained and well-tested before wiring anything to a turn.

- [ ] **Step 1: Declare the module**

Edit `crates/savvagent-host/src/router/mod.rs`. Add `pub mod heuristics;` alphabetically next to the other declarations, and extend the existing public re-export block. Final state of the relevant section:

```rust
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
```

Also update the module-level docstring comment above the imports if present — change `override → modality → rules → default` to `override → modality → rules → heuristic → default`.

- [ ] **Step 2: Write the failing tests**

Create `crates/savvagent-host/src/router/heuristics.rs` with the test cases first (TDD). At this point the test module will not compile because `HeuristicKind` / `classify` do not exist; that's the failing state we want:

```rust
//! Layer 4 of the router stack — hardcoded heuristic classifier.
//!
//! Gated on `RoutingRules::heuristics == true`. Pure functions, no I/O,
//! no async. Adding new `HeuristicKind` variants is additive thanks to
//! `#[non_exhaustive]`.

#[cfg(test)]
mod tests {
    use super::*;

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
}
```

- [ ] **Step 3: Run tests to verify they fail to compile**

Run: `cargo test -p savvagent-host --lib router::heuristics 2>&1 | head -40`

Expected: `error[E0412]: cannot find type 'HeuristicKind' in this scope` (or similar — the test file references items that don't exist yet).

- [ ] **Step 4: Implement `HeuristicKind` and `classify`**

Insert above the `#[cfg(test)] mod tests` block in `crates/savvagent-host/src/router/heuristics.rs`:

```rust
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p savvagent-host --lib router::heuristics`

Expected: all 7 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/router/heuristics.rs crates/savvagent-host/src/router/mod.rs
git commit -m "feat(host): HeuristicKind + classify (Phase 6 scaffold)"
```

---

## Task 2: `pick_for_kind` picker function

**Files:**
- Modify: `crates/savvagent-host/src/router/heuristics.rs`

Pure picker that maps a `HeuristicKind` + pool state to a `(provider, model)` pick. Returns `None` for the "no-op" cases (active already in tier; no matching tier in pool).

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `crates/savvagent-host/src/router/heuristics.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test -p savvagent-host --lib router::heuristics 2>&1 | head -30`

Expected: `error[E0425]: cannot find function 'pick_for_kind'` (or similar).

- [ ] **Step 3: Implement `pick_for_kind`**

Insert above the `#[cfg(test)] mod tests` block in `crates/savvagent-host/src/router/heuristics.rs`, alongside the existing `classify` function:

```rust
use crate::capabilities::CostTier;
use crate::router::ProviderView;
use crate::router::rules::DefaultPick;
use savvagent_protocol::ProviderId;

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
```

**Note on `models()` accessor:** `ProviderCapabilities` exposes its models through a `model(id)` accessor and a `models()` slice accessor. Check `crates/savvagent-host/src/capabilities.rs` and confirm both are public; if `models()` does not exist, add a `pub fn models(&self) -> &[ModelCapabilities]` accessor in a separate one-line edit and include it in this commit. (The existing `model(id)` accessor is used by Phase 4's modality picker — keep it.)

If `models()` does not already exist, add it in `crates/savvagent-host/src/capabilities.rs` right after `model(&self, …)`:

```rust
    /// Borrow the full models slice. Used by routing layers that need to
    /// iterate every model on a provider (e.g. the heuristic classifier).
    pub fn models(&self) -> &[ModelCapabilities] {
        &self.models
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p savvagent-host --lib router::heuristics`

Expected: all 14 tests pass (7 from Task 1 + 7 new).

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/router/heuristics.rs crates/savvagent-host/src/capabilities.rs
git commit -m "feat(host): heuristics::pick_for_kind + tier-priority picker (Phase 6)"
```

---

## Task 3: `RoutingReason::Heuristic` variant + re-exports

**Files:**
- Modify: `crates/savvagent-host/src/router/router.rs`
- Modify: `crates/savvagent-host/src/lib.rs`

Adds the new reason variant and its Display. No `Router::pick` wiring yet — that's Task 4.

- [ ] **Step 1: Write the failing Display test**

Append to the `#[cfg(test)] mod tests` block in `crates/savvagent-host/src/router/router.rs`:

```rust
    #[test]
    fn routing_reason_heuristic_displays() {
        use crate::router::heuristics::HeuristicKind;
        let r = RoutingReason::Heuristic {
            kind: HeuristicKind::ShortFactoid,
        };
        assert_eq!(format!("{r}"), "Heuristic(short)");
        let r = RoutingReason::Heuristic {
            kind: HeuristicKind::Coding,
        };
        assert_eq!(format!("{r}"), "Heuristic(coding)");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p savvagent-host --lib router::router::tests::routing_reason_heuristic_displays 2>&1 | head -20`

Expected: `no variant or associated item named 'Heuristic' found for enum 'RoutingReason'`.

- [ ] **Step 3: Add the variant + Display arm**

Edit `crates/savvagent-host/src/router/router.rs`. In the `RoutingReason` enum (around line 39), add the new variant alphabetically between `Default` and `Rule` is not required; place it between `Rule` and `Default` to mirror the layer order:

```rust
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
    /// The opt-in heuristic classifier matched this turn. (Layer 4.)
    Heuristic {
        /// Which heuristic category fired (short factoid vs. coding).
        kind: crate::router::heuristics::HeuristicKind,
    },
    /// No higher-priority layer matched; fell through to the active
    /// provider + its default model.
    Default,
}
```

Then extend the `Display` impl just below to add the new arm:

```rust
impl std::fmt::Display for RoutingReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoutingReason::Override => f.write_str("Override"),
            RoutingReason::Modality { kind } => write!(f, "Modality({kind})"),
            RoutingReason::Rule { name } => write!(f, "Rule({name})"),
            RoutingReason::Heuristic { kind } => write!(f, "Heuristic({kind})"),
            RoutingReason::Default => f.write_str("Default"),
        }
    }
}
```

Update the doc comment at the top of the file to reflect the no-longer-unimplemented Layer 4:

```rust
//! Layers (first match wins):
//!
//! - Layer 1 — `@provider[:model]` override (Override reason)
//! - Layer 2 — required-modality redirect (Modality reason)
//! - Layer 3 — user rules from `~/.savvagent/routing.toml` (Rule reason)
//! - Layer 4 — heuristic classifier, opt-in via `heuristics = true` in
//!             routing.toml (Heuristic reason)
//! - Layer 5 — fall through to the active provider + its default model
//!   (Default reason)
//!
//! `RoutingReason` is `#[non_exhaustive]` so adding new heuristic kinds
//! later is additive, not breaking.
```

- [ ] **Step 4: Re-export `HeuristicKind` from the crate root**

Edit `crates/savvagent-host/src/lib.rs`. Update the existing `pub use router::{ … }` block to include `HeuristicKind`:

```rust
pub use router::{
    BadModel, DefaultPick, HeuristicKind, LegacyModelResolution, ProviderView,
    ROUTING_RULES_SCHEMA_VERSION, RequiredModalities, RequiredModalityKind, Router,
    RoutingDecision, RoutingOverride, RoutingReason, RoutingRule, RoutingRules,
    RoutingRulesError, RuleMatch, RuleSignals, pick_vision_capable, required_modalities,
    resolve_legacy_model,
};
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p savvagent-host --lib router::router::tests::routing_reason_heuristic_displays`

Expected: PASS.

Also confirm the crate still builds:

Run: `cargo build -p savvagent-host`

Expected: clean build (no `dead_code` warning on the new variant since it's now referenced by the Display impl + the test).

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/router/router.rs crates/savvagent-host/src/lib.rs
git commit -m "feat(host): RoutingReason::Heuristic variant + Display (Phase 6)"
```

---

## Task 4: Layer 4 wiring inside `Router::pick`

**Files:**
- Modify: `crates/savvagent-host/src/router/router.rs`

Wire the heuristic layer into `Router::pick`. No signature change. Seven new tests cover the layered-precedence contract.

- [ ] **Step 1: Write the failing router-integration tests**

Append to the `#[cfg(test)] mod tests` block in `crates/savvagent-host/src/router/router.rs`. (The helpers `caps`, `caps_with_vision`, `rules_with_one_rule`, etc. already exist there from Phase 5 — reuse them. Define a new helper for tier-bearing caps:)

```rust
    fn caps_tier(model: &str, tier: CostTier) -> ProviderCapabilities {
        ProviderCapabilities::new(
            vec![ModelCapabilities {
                id: model.into(),
                display_name: model.into(),
                supports_vision: false,
                supports_audio: false,
                context_window: 0,
                cost_tier: tier,
            }],
            model.into(),
        )
        .expect("valid caps")
    }

    fn caps_two_tiers(
        m1: &str, t1: CostTier, m2: &str, t2: CostTier, default_idx: usize,
    ) -> ProviderCapabilities {
        let models = vec![
            ModelCapabilities {
                id: m1.into(),
                display_name: m1.into(),
                supports_vision: false,
                supports_audio: false,
                context_window: 0,
                cost_tier: t1,
            },
            ModelCapabilities {
                id: m2.into(),
                display_name: m2.into(),
                supports_vision: false,
                supports_audio: false,
                context_window: 0,
                cost_tier: t2,
            },
        ];
        let default = models[default_idx].id.clone();
        ProviderCapabilities::new(models, default).expect("valid caps")
    }

    fn rules_heuristics_only(on: bool) -> RoutingRules {
        RoutingRules {
            default: None,
            heuristics: on,
            rules: vec![],
        }
    }

    #[test]
    fn pick_heuristic_short_factoid_routes_to_cheap() {
        use crate::router::heuristics::HeuristicKind;
        // Anthropic: opus (Premium, active) + haiku (Cheap).
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = caps_two_tiers("opus", CostTier::Premium, "haiku", CostTier::Cheap, 0);
        let views = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];
        let rules = rules_heuristics_only(true);

        let r = Router::pick(
            None,
            &views,
            &a_id,
            "opus",
            RequiredModalities::default(),
            &rules,
            "what is 2+2?",
        );
        assert_eq!(r.provider_id, a_id);
        assert_eq!(r.model_id, "haiku");
        assert_eq!(
            r.reason,
            RoutingReason::Heuristic {
                kind: HeuristicKind::ShortFactoid,
            }
        );
    }

    #[test]
    fn pick_heuristic_coding_routes_to_premium() {
        use crate::router::heuristics::HeuristicKind;
        // Anthropic: haiku (Cheap, active) + opus (Premium).
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = caps_two_tiers("haiku", CostTier::Cheap, "opus", CostTier::Premium, 0);
        let views = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];
        let rules = rules_heuristics_only(true);

        let r = Router::pick(
            None,
            &views,
            &a_id,
            "haiku",
            RequiredModalities::default(),
            &rules,
            "please refactor this",
        );
        assert_eq!(r.provider_id, a_id);
        assert_eq!(r.model_id, "opus");
        assert_eq!(
            r.reason,
            RoutingReason::Heuristic {
                kind: HeuristicKind::Coding,
            }
        );
    }

    #[test]
    fn pick_heuristic_off_falls_through_to_default() {
        // Same setup as `pick_heuristic_short_factoid_routes_to_cheap` but
        // with heuristics=false. The classifier must not run.
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = caps_two_tiers("opus", CostTier::Premium, "haiku", CostTier::Cheap, 0);
        let views = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];
        let rules = rules_heuristics_only(false);

        let r = Router::pick(
            None,
            &views,
            &a_id,
            "opus",
            RequiredModalities::default(),
            &rules,
            "what is 2+2?",
        );
        assert_eq!(r.provider_id, a_id);
        assert_eq!(r.model_id, "opus");
        assert_eq!(r.reason, RoutingReason::Default);
    }

    #[test]
    fn pick_rule_beats_heuristic() {
        // Heuristic on, would match Coding. But a Rule also matches the
        // same turn — Layer 3 runs first.
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps_two_tiers("haiku", CostTier::Cheap, "opus", CostTier::Premium, 0);
        let g_caps = caps_tier("flash", CostTier::Cheap);
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
        let mut rules = rules_heuristics_only(true);
        rules.rules.push(RoutingRule {
            name: "rule-wins".into(),
            match_: RuleMatch {
                keywords: vec!["refactor".into()],
                ..Default::default()
            },
            use_: DefaultPick {
                provider: g_id.clone(),
                model: "flash".into(),
            },
        });

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
                name: "rule-wins".into(),
            }
        );
    }

    #[test]
    fn pick_modality_beats_heuristic() {
        // Image attached + coding keyword. Modality (Layer 2) wins.
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
                    cost_tier: CostTier::Cheap,
                },
                ModelCapabilities {
                    id: "opus".into(),
                    display_name: "opus".into(),
                    supports_vision: true,
                    supports_audio: false,
                    context_window: 0,
                    cost_tier: CostTier::Premium,
                },
            ],
            "haiku".into(),
        )
        .expect("valid caps");
        let views = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];
        let rules = rules_heuristics_only(true);

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
            "refactor this code",
        );
        assert_eq!(r.provider_id, a_id);
        assert_eq!(r.model_id, "opus");
        assert_eq!(
            r.reason,
            RoutingReason::Modality {
                kind: RequiredModalityKind::Image,
            }
        );
    }

    #[test]
    fn pick_override_beats_heuristic() {
        // @-override + coding keyword. Override (Layer 1) wins.
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps_two_tiers("haiku", CostTier::Cheap, "opus", CostTier::Premium, 0);
        let g_caps = caps_tier("flash", CostTier::Cheap);
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
        let rules = rules_heuristics_only(true);
        let override_ = RoutingOverride {
            provider: g_id.clone(),
            model: Some("flash".into()),
        };

        let r = Router::pick(
            Some(override_),
            &views,
            &a_id,
            "haiku",
            RequiredModalities::default(),
            &rules,
            "please refactor this",
        );
        assert_eq!(r.provider_id, g_id);
        assert_eq!(r.model_id, "flash");
        assert_eq!(r.reason, RoutingReason::Override);
    }

    #[test]
    fn pick_heuristic_returns_default_when_active_already_in_tier() {
        // Active = haiku (Cheap); short factoid wants Cheap → no-op.
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = caps_two_tiers("haiku", CostTier::Cheap, "opus", CostTier::Premium, 0);
        let views = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];
        let rules = rules_heuristics_only(true);

        let r = Router::pick(
            None,
            &views,
            &a_id,
            "haiku",
            RequiredModalities::default(),
            &rules,
            "what is 2+2?",
        );
        assert_eq!(r.provider_id, a_id);
        assert_eq!(r.model_id, "haiku");
        assert_eq!(r.reason, RoutingReason::Default);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p savvagent-host --lib router::router::tests::pick_heuristic 2>&1 | head -40`

Expected: tests `pick_heuristic_short_factoid_routes_to_cheap`, `pick_heuristic_coding_routes_to_premium`, and `pick_heuristic_returns_default_when_active_already_in_tier` FAIL because `Router::pick` does not yet call the heuristic layer — they expect `Heuristic(short)` / `Heuristic(coding)` but get `Default`. The "off / rule beats / modality beats / override beats" tests pass since they expect non-Heuristic outcomes.

- [ ] **Step 3: Wire Layer 4 in `Router::pick`**

Edit `crates/savvagent-host/src/router/router.rs`, inside the `Router::pick` function. Insert the new layer between the rules layer and the default-fallthrough return (today's code: the `if let Some((name, pick)) = rules.evaluate(...) { … }` block ends, then the function falls through to the Default return). Add the heuristic layer immediately after the rules block:

```rust
        if let Some((name, pick)) = rules.evaluate(&signals, providers) {
            return RoutingDecision {
                provider_id: pick.provider,
                model_id: pick.model,
                reason: RoutingReason::Rule { name },
            };
        }

        // Layer 4 — heuristic classifier (opt-in via routing.toml).
        if rules.heuristics
            && let Some(kind) = crate::router::heuristics::classify(user_text)
            && let Some(pick) = crate::router::heuristics::pick_for_kind(
                kind,
                active_provider,
                active_model,
                providers,
            )
        {
            tracing::info!(
                kind = %kind,
                provider = %pick.provider.as_str(),
                model = %pick.model,
                "routing: heuristic classifier matched"
            );
            return RoutingDecision {
                provider_id: pick.provider,
                model_id: pick.model,
                reason: RoutingReason::Heuristic { kind },
            };
        }

        RoutingDecision {
            provider_id: active_provider.clone(),
            model_id: active_model.to_string(),
            reason: RoutingReason::Default,
        }
```

Also update the doc comment on `Router::pick` to flip "Heuristic — not yet implemented" to a real description:

```rust
    /// Pick a `(provider, model, reason)` triple for a turn.
    ///
    /// Layers (first match wins):
    /// 1. **Override** — `@`-prefix from the user input.
    /// 2. **Modality** — same-provider redirect when the active model
    ///    lacks a required modality.
    /// 3. **Rules** — first matching rule from `~/.savvagent/routing.toml`.
    /// 4. **Heuristic** — opt-in classifier gated on `rules.heuristics`.
    /// 5. **Default** — active provider + active model.
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p savvagent-host --lib router::router::tests`

Expected: all router-integration tests (pre-existing + 7 new + the Display test from Task 3) pass.

Run the broader crate tests too, to confirm no Phase 5 regressions:

Run: `cargo test -p savvagent-host`

Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/router/router.rs
git commit -m "feat(host): Router::pick Layer 4 — heuristic classifier (Phase 6)"
```

---

## Task 5: End-to-end integration tests

**Files:**
- Create: `crates/savvagent-host/tests/heuristic_e2e.rs`

Three end-to-end scenarios that exercise the full `Host::run_turn_streaming` path with the heuristic classifier enabled. Mirrors the structure of `crates/savvagent-host/tests/route_rules_e2e.rs`.

- [ ] **Step 1: Write the end-to-end tests**

Create `crates/savvagent-host/tests/heuristic_e2e.rs`:

```rust
//! End-to-end heuristic-classifier integration tests.
//!
//! Three scenarios:
//!
//! 1. **Short factoid** — heuristics=true, active=opus (Premium). A short
//!    `?`-bearing turn routes to haiku (Cheap) with badge `Heuristic(short)`.
//! 2. **Coding keyword** — heuristics=true, active=haiku (Cheap). A
//!    "refactor"-bearing turn routes to opus (Premium) with badge
//!    `Heuristic(coding)`.
//! 3. **Heuristic off** — heuristics=false. The same inputs route to the
//!    active model with `RoutingReason::Default`.

use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
use savvagent_host::{
    HeuristicKind, Host, HostConfig, ProviderEndpoint, ProviderRegistration, RoutingReason,
    StartupConnectPolicy, TurnEvent,
};
use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ContentBlock, ListModelsResponse, ProviderError, ProviderId,
    StopReason, StreamEvent,
};
use tokio::sync::{Mutex, mpsc};

struct StubProvider {
    seen_model: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl ProviderClient for StubProvider {
    async fn complete(
        &self,
        req: CompleteRequest,
        _stream: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        *self.seen_model.lock().await = Some(req.model.clone());
        Ok(CompleteResponse {
            id: "stub-0".into(),
            model: req.model.clone(),
            content: vec![ContentBlock::Text { text: "ok".into() }],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Default::default(),
        })
    }

    async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
        Ok(ListModelsResponse {
            models: vec![],
            default_model_id: None,
        })
    }
}

fn caps_haiku_opus(default: &str) -> ProviderCapabilities {
    ProviderCapabilities::new(
        vec![
            ModelCapabilities {
                id: "claude-haiku-4-5".into(),
                display_name: "Claude Haiku 4.5".into(),
                supports_vision: false,
                supports_audio: false,
                context_window: 0,
                cost_tier: CostTier::Cheap,
            },
            ModelCapabilities {
                id: "claude-opus-4-7".into(),
                display_name: "Claude Opus 4.7".into(),
                supports_vision: false,
                supports_audio: false,
                context_window: 0,
                cost_tier: CostTier::Premium,
            },
        ],
        default.into(),
    )
    .expect("valid caps")
}

fn write_routing_toml(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("routing.toml");
    let mut f = std::fs::File::create(&path).expect("create routing.toml");
    f.write_all(content.as_bytes()).expect("write");
    (dir, path)
}

fn reg(
    id: &str,
    caps: ProviderCapabilities,
    seen: Arc<Mutex<Option<String>>>,
) -> ProviderRegistration {
    ProviderRegistration::new(
        ProviderId::new(id).expect("valid provider id"),
        id,
        Arc::new(StubProvider { seen_model: seen }) as Arc<dyn ProviderClient + Send + Sync>,
        caps,
    )
}

async fn collect_events(rx: &mut mpsc::Receiver<TurnEvent>, timeout_ms: u64) -> Vec<TurnEvent> {
    let mut out = Vec::new();
    while let Ok(Some(ev)) =
        tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx.recv()).await
    {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn heuristic_short_factoid_routes_to_cheap_model() {
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        caps_haiku_opus("claude-opus-4-7"),
        Arc::clone(&a_seen),
    );

    let (_dir, path) = write_routing_toml("heuristics = true\n");

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "claude-opus-4-7",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;
    cfg.routing_rules_path = Some(path);

    let host = Host::start(cfg).await.expect("host starts");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _ = host
        .run_turn_streaming("what is 2+2?", tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 200).await;
    let saw_heuristic = events.iter().any(|ev| {
        matches!(
            ev,
            TurnEvent::RouteSelected {
                reason: RoutingReason::Heuristic { kind: HeuristicKind::ShortFactoid },
                model_id,
                ..
            } if model_id == "claude-haiku-4-5"
        )
    });
    assert!(
        saw_heuristic,
        "expected Heuristic(short) → haiku; got {events:?}"
    );

    assert_eq!(
        a_seen.lock().await.as_deref(),
        Some("claude-haiku-4-5"),
        "the provider should have been invoked with the cheap model"
    );

    host.shutdown().await;
}

#[tokio::test]
async fn heuristic_coding_routes_to_premium_model() {
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        caps_haiku_opus("claude-haiku-4-5"),
        Arc::clone(&a_seen),
    );

    let (_dir, path) = write_routing_toml("heuristics = true\n");

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "claude-haiku-4-5",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;
    cfg.routing_rules_path = Some(path);

    let host = Host::start(cfg).await.expect("host starts");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _ = host
        .run_turn_streaming("please refactor this function", tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 200).await;
    let saw_heuristic = events.iter().any(|ev| {
        matches!(
            ev,
            TurnEvent::RouteSelected {
                reason: RoutingReason::Heuristic { kind: HeuristicKind::Coding },
                model_id,
                ..
            } if model_id == "claude-opus-4-7"
        )
    });
    assert!(
        saw_heuristic,
        "expected Heuristic(coding) → opus; got {events:?}"
    );

    assert_eq!(
        a_seen.lock().await.as_deref(),
        Some("claude-opus-4-7"),
        "the provider should have been invoked with the premium model"
    );

    host.shutdown().await;
}

#[tokio::test]
async fn heuristic_off_falls_through_to_default() {
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        caps_haiku_opus("claude-opus-4-7"),
        Arc::clone(&a_seen),
    );

    // No routing.toml ⇒ heuristics defaults to false.
    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "claude-opus-4-7",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;
    cfg.routing_rules_path = None;

    let host = Host::start(cfg).await.expect("host starts");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _ = host
        .run_turn_streaming("what is 2+2?", tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 200).await;
    let saw_default = events.iter().any(|ev| {
        matches!(
            ev,
            TurnEvent::RouteSelected {
                reason: RoutingReason::Default,
                model_id,
                ..
            } if model_id == "claude-opus-4-7"
        )
    });
    assert!(
        saw_default,
        "expected Default → opus (heuristics off); got {events:?}"
    );

    host.shutdown().await;
}
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test -p savvagent-host --test heuristic_e2e`

Expected: all 3 tests pass. If any test hangs, double-check streaming permissions — synthetic turns with tool calls require a pre-registered `Allow` rule per [[feedback_streaming_test_permissions]]. (This test path does not use tools, so an `Allow` rule should not be needed; if a hang appears anyway, mirror the pre-registration pattern from `tests/modality_routing.rs`.)

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent-host/tests/heuristic_e2e.rs
git commit -m "test(host): end-to-end Phase 6 heuristic classifier (3 scenarios)"
```

---

## Task 6: TUI `/route show` active-classifier line + locales

**Files:**
- Modify: `crates/savvagent/locales/en.toml`
- Modify: `crates/savvagent/locales/es.toml`
- Modify: `crates/savvagent/locales/pt.toml`
- Modify: `crates/savvagent/locales/hi.toml`
- Modify: `crates/savvagent/src/main.rs`

Swap Phase 5's "ships in a future release" placeholder for a description of what the active classifier does.

- [ ] **Step 1: Write the failing render tests**

Append to the `render_routing_show_tests` module in `crates/savvagent/src/main.rs` (around line 3045, just before the closing brace of the `#[cfg(test)] mod render_routing_show_tests { … }` block):

```rust
    #[test]
    fn heuristic_active_line_shown_when_heuristics_true() {
        // Lock the locale to en so substring assertions are stable in
        // parallel test runs (per feedback_test_locale_isolation).
        let _g = crate::tests::HOME_LOCK.lock().expect("home lock");
        rust_i18n::set_locale("en");

        let mut app = build_app();
        let rules = RoutingRules {
            default: None,
            heuristics: true,
            rules: vec![],
        };
        render_routing_show(&mut app, &rules);
        let notes = collect_notes(&app);
        let saw_active = notes
            .iter()
            .any(|n| n.to_lowercase().contains("heuristics: enabled"));
        assert!(
            saw_active,
            "expected an active-heuristics line; got {notes:?}"
        );
        // The Phase 5 placeholder must NOT appear when heuristics=true.
        assert!(
            !notes.iter().any(|n| n.contains("future release")),
            "Phase 5 placeholder must not be emitted when heuristics is on; got {notes:?}"
        );
    }

    #[test]
    fn heuristic_line_omitted_when_heuristics_false() {
        let _g = crate::tests::HOME_LOCK.lock().expect("home lock");
        rust_i18n::set_locale("en");

        let mut app = build_app();
        let rules = RoutingRules {
            default: None,
            heuristics: false,
            rules: vec![],
        };
        render_routing_show(&mut app, &rules);
        let notes = collect_notes(&app);
        let saw_any_heuristics = notes
            .iter()
            .any(|n| n.to_lowercase().contains("heuristics"));
        assert!(
            !saw_any_heuristics,
            "no heuristic line should be emitted when heuristics is off; got {notes:?}"
        );
    }
```

**Confirm `HOME_LOCK` location.** Phase 5 tests reference `crate::tests::HOME_LOCK` from inside `render_routing_show_tests`. If grep shows it lives elsewhere (e.g. `crate::ui::tests::HOME_LOCK`), update the path. Run:

```bash
grep -n "HOME_LOCK" crates/savvagent/src/main.rs crates/savvagent/src/tests/ 2>/dev/null | head
```

…and adjust the `_g = …` line accordingly.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p savvagent --lib render_routing_show_tests::heuristic 2>&1 | head -30`

Expected: `heuristic_active_line_shown_when_heuristics_true` FAILS because today's code emits `routing.show-heuristics-pending` ("ships in a future release"), not the new active line.

- [ ] **Step 3: Add the new locale key in en.toml**

Edit `crates/savvagent/locales/en.toml`. Inside the existing `[routing]` table (currently at lines 352-365), insert one new key just below `show-heuristics-pending` and before `show-last`:

```toml
show-heuristics-active  = "heuristics: enabled — short questions (≤200 chars + '?') route to cheap models; coding-flavored prompts (refactor/implement/debug/...) route to premium models. Substring keyword match."
```

(Leave `show-heuristics-pending` in place for now — backward-compat; cleanup is a future commit.)

- [ ] **Step 4: Add placeholders in the other locales**

Edit `crates/savvagent/locales/es.toml`, `pt.toml`, and `hi.toml`. In each file, insert the same key under `[routing]` with a TODO placeholder. Pattern matches Phase 5 (rust_i18n falls back to en automatically):

```toml
show-heuristics-active  = "TODO: translate — heuristics enabled, short questions route to cheap models; coding prompts route to premium models"
```

- [ ] **Step 5: Swap the render branch in main.rs**

Edit `crates/savvagent/src/main.rs::render_routing_show` (around line 1349). Replace:

```rust
    if rules.heuristics {
        app.push_note(rust_i18n::t!("routing.show-heuristics-pending").to_string());
    }
```

with:

```rust
    if rules.heuristics {
        app.push_note(rust_i18n::t!("routing.show-heuristics-active").to_string());
    }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p savvagent --lib render_routing_show_tests`

Expected: all `render_routing_show_tests` cases (pre-existing + 2 new) pass.

- [ ] **Step 7: Commit**

```bash
git add crates/savvagent/src/main.rs crates/savvagent/locales/en.toml crates/savvagent/locales/es.toml crates/savvagent/locales/pt.toml crates/savvagent/locales/hi.toml
git commit -m "feat(tui): /route show describes active classifier when heuristics=true (Phase 6)"
```

---

## Task 7: Workspace version bump + CHANGELOG + README

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `CHANGELOG.md`
- Modify: `README.md`

Per-phase scaffolding. The tagged release rolls up all phases later per [[project_multi_provider_release.md]].

- [ ] **Step 1: Bump workspace version**

Edit `Cargo.toml` at the repository root. Update `[workspace.package].version`:

```toml
[workspace.package]
version = "0.20.0"
```

Then update every `version = "0.19.0"` literal in `[workspace.dependencies]` to `"0.20.0"`. There are 11 of them — find them with:

```bash
grep -n 'version = "0.19.0"' Cargo.toml
```

Replace each with `"0.20.0"`. (Per [[feedback_semver]], the dependency literals must mirror the `[workspace.package].version`.)

- [ ] **Step 2: Add the CHANGELOG entry**

Edit `CHANGELOG.md`. Insert a new section between the existing header lines (above `## 0.19.0`):

```markdown
## 0.20.0 - 2026-05-19

### Added

- **Heuristic classifier (Layer 4 of the router)**. Opt-in via `heuristics = true` in `~/.savvagent/routing.toml`. Short questions (≤200 chars + `?`) route to a cheap model (`CostTier::Free` or `Cheap`); coding-flavored prompts (substring match against `refactor`, `implement`, `debug`, `fix bug`, `compile`, `stack trace`, `function`, `class`, `error`) route to a premium model (`Premium` or `Standard`). Same-provider preferred; sibling providers are walked only when the active provider has no matching model. Off by default.
- **`RoutingReason::Heuristic { kind }`** variant on the existing `#[non_exhaustive]` enum. Transcript badge renders `Heuristic(short)` / `Heuristic(coding)`.
- **`/route show`** now describes the active classifier (categories + triggers) when `heuristics = true`. When `heuristics = false`, no heuristic line is printed.

### Changed

- `Router::pick` runs a new Layer-4 step between rules and default. Layered precedence is unchanged: `@`-override, Modality, and matching user Rules all still beat the heuristic when they apply.

### Notes

- Coding-keyword matching is **substring-based** in v1 — `function` matches `functional`, `error` matches `terror`. Users who want stricter matching write explicit `[[rule]]` entries; rules run earlier (Layer 3) and beat the heuristic.
- The `routing.show-heuristics-pending` locale key remains in the catalog for backward compat but is no longer emitted by any code path.
```

- [ ] **Step 3: Add the README "Heuristic classifier" subsection**

Edit `README.md`. Find the existing routing-rules section (added in Phase 5; grep for "routing.toml"). Append a new "Heuristic classifier" subsection at the end:

```markdown
### Heuristic classifier (opt-in)

Add `heuristics = true` to `~/.savvagent/routing.toml` to turn on Layer 4 of the router — a hardcoded classifier that picks a cheaper or stronger model based on the shape of the user input:

- **Short question** (≤200 chars + a `?`) → cheapest connected model (`CostTier::Free` or `Cheap`).
- **Coding-flavored prompt** (contains any of `refactor`, `implement`, `debug`, `fix bug`, `compile`, `stack trace`, `function`, `class`, `error`) → strongest connected model (`Premium` or `Standard`).

The classifier prefers models on the **active provider** first, then walks the rest of the connected pool. If no connected model matches the desired tier — or the active model is already in that tier — the classifier yields nothing and the request falls through to your `/model` selection. Override (`@provider:model`), modality redirects (e.g. images → vision models), and explicit `[[rule]]` entries in `routing.toml` all beat the classifier when they apply.

**Caveats.** Coding keyword matching is **substring-based** in v1 — `function` matches `functional`, `error` matches `terror`. If you need stricter matching (whole-word only, custom keyword list, custom thresholds), use explicit `[[rule]]` entries instead; rules run earlier and beat the classifier.

Disable any time by setting `heuristics = false` (or removing the line) and running `/route reload`.
```

- [ ] **Step 4: Confirm the workspace builds + lints clean**

Run:

```bash
cargo build --workspace
cargo test --workspace
rustup run stable cargo fmt --all -- --check
rustup run stable cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all four succeed. (Per [[feedback_match_ci_toolchain_locally]], run `rustup run stable` for fmt/clippy so local runs match CI's stable toolchain.)

If clippy flags anything new, fix it inline (most likely candidates: unused import, missing-docs on the new `pub` items, redundant clone in the picker). Re-run.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml CHANGELOG.md README.md
git commit -m "release(0.20.0): heuristic classifier (Phase 6)"
```

---

## Task 8: Final workspace verification

**Files:** none (read-only verification).

- [ ] **Step 1: Re-run the full test matrix**

```bash
cargo test --workspace
```

Expected: 0 failures.

- [ ] **Step 2: Confirm CI parity**

```bash
rustup run stable cargo fmt --all -- --check
rustup run stable cargo clippy --workspace --all-targets -- -D warnings
```

Expected: clean. Per [[feedback_dead_code_in_binary_crate.md]], any new `pub` item in the `savvagent` binary crate must be consumed by non-test code — verify nothing in the new code path triggers a `dead_code` error. (Everything in this plan flows through `Router::pick` and `render_routing_show`, both reached from production code, so no `#[allow(dead_code)]` should be needed.)

- [ ] **Step 3: Spot-check `/route show` manually (optional but recommended)**

Build and launch the TUI:

```bash
cargo run -p savvagent
```

Inside the TUI:
1. Create or edit `~/.savvagent/routing.toml` with `heuristics = true` and run `/route reload`.
2. Run `/route show` — confirm the new active-classifier line appears (`heuristics: enabled — short questions...`).
3. Send a turn `"what is 2+2?"` — confirm the transcript badge reads `Heuristic(short)` and the chosen model is the cheapest connected one.
4. Send `"refactor this function"` — confirm badge reads `Heuristic(coding)` and the chosen model is the premium one.
5. Remove `heuristics = true`, run `/route reload`, send the same turns — confirm badges read `Default` and the model is the active selection.

This is **best-effort verification**, not a hard gate; if no provider keys are available in the dev environment, skip and rely on the e2e tests.

- [ ] **Step 4: Final commit-log check**

```bash
git log --oneline origin/master..HEAD
```

Expected: 7 commits in order (Task 1 through Task 7). Each is small, focused, and reverts cleanly.

---

## Self-review notes

- **Spec coverage:** every "In" scope item is owned by a task — heuristics.rs by Tasks 1-2; `Router::pick` Layer 4 by Tasks 3-4; e2e tests by Task 5; TUI render + locales by Task 6; version + CHANGELOG + README by Task 7; verification by Task 8.
- **Type consistency:** `HeuristicKind` is defined once in Task 1 (with Display impl and `#[non_exhaustive]`) and used identically in Tasks 2-8. `DefaultPick` is reused as-is from Phase 5 — no shape change.
- **Placeholders:** no `TODO`, `TBD`, or "implement later" in the plan. The only deferred work is captured under "Out of scope" in the spec and called out explicitly in CHANGELOG / README (substring matching is a v1 contract, `[heuristics]` TOML tuning is future).
- **`models()` accessor caveat:** Task 2 calls out that `ProviderCapabilities::models()` may not yet exist and asks the implementer to add a one-line accessor if missing. If the implementer finds the accessor is already present (Phase 4 may have added it), they can skip the addition; no other task depends on this.
- **Locale isolation:** Task 6 tests reset to `"en"` inside `HOME_LOCK` per [[feedback_test_locale_isolation.md]] so parallel test runs don't poison the lock.
- **CI parity:** Task 7 step 4 + Task 8 step 2 both run `rustup run stable cargo fmt/clippy` per [[feedback_match_ci_toolchain_locally]].
- **No tag push.** This plan ends with a `release(0.20.0)` commit on `master`; per [[feedback_phase_release_rollup.md]] and [[project_multi_provider_release.md]], the tag for the rollup release is pushed only when the entire multi-provider initiative is complete and CI green across every phase commit. Do not run `git tag` or `git push --tags` as part of this plan.
