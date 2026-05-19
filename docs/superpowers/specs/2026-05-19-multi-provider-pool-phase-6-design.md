# Multi-provider pool — Phase 6 (heuristic classifier) — design

Date: 2026-05-19
Status: pending review
Roadmap: ships as part of the multi-provider rollup (see [[project_multi_provider_release.md]] in user memory); per-phase `release(0.X.0)` commits are scaffolding, not tags
Parent spec: `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`
Predecessor phase: `docs/superpowers/plans/2026-05-18-multi-provider-pool-phase-5.md` (shipped as `release(0.19.0)`)
Successor phase: none (Phase 6 closes the multi-provider routing initiative)
Related memories: [[feedback_release_notes]] · [[feedback_release_docs]] · [[feedback_ui_via_plugins]] · [[feedback_streaming_test_permissions]] · [[feedback_test_locale_isolation]] · [[feedback_drive_pr_series_to_completion]] · [[feedback_verify_ci_after_push]] · [[feedback_dead_code_in_binary_crate.md]] · [[project_multi_provider_release.md]] · [[feedback_phase_release_rollup.md]] · [[feedback_semver.md]]

## Brief

> Phase 6 of `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`.

The parent spec's "Phasing" list defines Phase 6 as:

> **Heuristic classifier.** Opt-in via `heuristics = true` in routing.toml. Riskiest UX (opaque boundary cases), shipped last so we can see real usage from phases 3-5 before guessing keyword lists.

And the parent spec's "Approach" section defines the classifier shape:

> **Heuristic classifier** (opt-in). Short factoid → cheap fast model; keyword "refactor"/"implement"/"debug" → coding-strong model; else default. Off by default in v1; user enables via routing.toml.

Layer 4 of the router stack (between Rules and Default). `RoutingReason::Heuristic { kind }` is the new variant on the existing `#[non_exhaustive]` enum. The `heuristics: bool` field on `RoutingRules` is already parsed/stored by Phase 5 — Phase 6 wires it through `Router::pick`.

## Assumptions

Every choice made without asking lives here. The developer's review of this spec is the review of these assumptions.

Verbatim from the parent spec (listed only so reviewers can spot drift): heuristic is **Layer 4**, sits between Rules (Phase 5) and Default; gated on `routing.toml#heuristics = true`; `RoutingReason::Heuristic` is additive on the `#[non_exhaustive]` enum; transcript badge renders `Heuristic(<kind>)` matching Phase 4's `Modality(image)` Display style; `@`-overrides, Modality, and matching user Rules all still beat Heuristic when they apply. Phase 6 does not adjust any of these.

The substantive choices the developer is being asked to confirm:

1. **Classifier module location: `crates/savvagent-host/src/router/heuristics.rs`.** Exactly where the parent spec's "Modules" section places it. Single new file, pure functions, no async, no I/O — symmetric with `modality.rs` and `prefix.rs`.

2. **Two `HeuristicKind` variants only — `ShortFactoid` and `Coding`.** The parent spec lists exactly these two categories. Both are extensible behind `#[non_exhaustive]` so a future `Translation`, `Summarization`, etc. can land additively without a breaking change. Anything that matches neither category falls through to Default — there is no `HeuristicKind::Default`; the absence of a match *is* the default path.

3. **`ShortFactoid` detector: `user_text.chars().count() <= 200 && user_text.contains('?')`.** The parent spec says "short factoid"; the question-mark constraint is the cheapest signal that a turn is a question vs. an open-ended instruction. Threshold of 200 chars picks the lower-half of typical chat-style input; the `?` filter keeps a 30-char "implement this function" turn from being misrouted to a cheap model. Both numbers are baked in for v1; the `Risks & Open Questions` section captures the "user wants to tune these" follow-up. (Alternative: ≤400 chars with no question marker — too aggressive; misclassifies short instructions as factoids. Rejected.)

4. **`Coding` detector: case-insensitive substring match against a hardcoded keyword list.** Initial list: `refactor`, `implement`, `debug`, `fix bug`, `compile`, `stack trace`, `function`, `class`, `error`. Substring (not whole-word) match — matches `implementation`, `refactored`, `debugger`, etc. The parent spec calls out "refactor/implement/debug" verbatim; the rest are the conservative defaults expanded from Phase 5's keyword-rule usage patterns. List is **hardcoded in v1**; users who want to add/remove keywords write explicit `[[rule]]` entries (Phase 5 already supports `keywords = [...]` predicates that beat the heuristic since rules run earlier). Future iteration can expose this in a `[heuristics]` TOML table; out of scope here.

5. **Model selection leans on `CostTier`, not a new flag.** Every model already carries `CostTier::{Free, Cheap, Standard, Premium}` (verified in `provider_anthropic`, `provider_gemini`, `provider_openai`, `provider_local`). Heuristic picks:
   - **`ShortFactoid` → first `CostTier::Free` or `CostTier::Cheap` model**, in that order; among same-tier candidates the active provider's models are tried first, then iterated in pool stable order.
   - **`Coding` → first `CostTier::Premium` or `CostTier::Standard` model**, same active-provider-first preference. (Coding-strong is approximated by `Premium`; if no `Premium` model is connected, `Standard` is the conservative fallback rather than punting to Default.)
   No new capability flag is introduced. *Why:* a new bool would force every provider plugin to add a default, and the cost tier already encodes this signal well enough for v1.

6. **Same-provider preference, not same-provider only.** Active-provider models are tried first; if none of the active provider's models match the required tier set, the classifier walks the rest of the pool in stable order. This is **looser** than Phase 4 Modality's "no silent cross-provider hop" — and deliberately so. *Why:* the user explicitly opted into the classifier (`heuristics = true`); cross-provider hops here are no more surprising than what an explicit `[[rule]]` does, and the classifier would be useless if it could only ever pick the active provider's `Premium` model (which on Anthropic is the same as the default anyway). Modality stayed conservative because modality fires *implicitly* on every image-bearing turn whether the user knows it or not.

7. **No effect if `ShortFactoid` and `Coding` both match.** `Coding` wins. A turn like "can you debug this?" has length ≤ 200 and a question mark and a coding keyword — `Coding` is the more specific signal. Tested explicitly.

8. **Heuristic does not fire when the active provider's default-tier model already matches the desired tier.** If `ShortFactoid` selects `CostTier::Cheap` and the active provider's *active model* is already `Cheap` (e.g. user is on Haiku), the classifier returns no decision — falls through to Default. This avoids a no-op routing badge ("Heuristic(short)" for a model the user was going to use anyway). Same logic for `Coding` + Premium-active.

9. **`RoutingReason::Heuristic { kind: HeuristicKind }`** is the new variant. Display: `Heuristic(short)`, `Heuristic(coding)` — lowercase shortnames matching `Modality(image)`'s convention. `HeuristicKind` is `#[non_exhaustive]` so adding `Translation` later is additive.

10. **Gated on `rules.heuristics == true`.** The field is already parsed and stored by Phase 5. The wiring change in `Router::pick` is one branch: skip the heuristic layer entirely when the flag is false. No new config knob.

11. **`Router::pick` gains zero new parameters.** All inputs the heuristic needs are already in scope: `providers: &[ProviderView]`, `active_provider`, `active_model`, `user_text: &str`, `rules: &RoutingRules` (for the `heuristics` flag). One new internal call between the rules layer and the Default return.

12. **`/route show` updates to surface the active classifier state.** When `heuristics = true`, the existing `routing.show-heuristics-pending` locale ("classifier ships in a future release") swaps to a new `routing.show-heuristics-active` line that summarizes the categories and triggers (e.g. "heuristics: enabled — short-factoid (≤200 chars + '?') routes to cheap models; coding (refactor/implement/debug/…) routes to premium models"). When `heuristics = false`, the line is omitted entirely (current Phase 5 behavior). *Why:* without surfacing the rules, debugging "why did the classifier pick that?" requires reading source.

13. **No new `routing.toml` keys.** Phase 6 ships the implementation; tuning surface (custom keyword lists, per-kind thresholds, custom `provider/model` per kind) is explicit follow-up work tracked under "Out of scope". Adding a `[heuristics]` table now would commit to a shape before there's any usage data — the parent spec specifically defers the classifier *because* "an ML-based router is not in scope" and the keyword lists are "guesses we want to see real usage from phases 3-5 before refining."

14. **Telemetry: `tracing::info!` on every heuristic match** with `kind`, chosen `provider`, `model`. Mirrors Phase 5's rule-skipped tracing. No new metrics surface.

15. **Workspace version bump: `release(0.20.0)`** in-tree (per-phase scaffolding pattern); the actual tagged release rolls up Phases 1-6 once the initiative is done — per [[feedback_phase_release_rollup.md]] and [[project_multi_provider_release.md]].

16. **CHANGELOG, README, release notes** updated in the same commit as the version bump per [[feedback_release_notes]] and [[feedback_release_docs]]. README adds a one-paragraph "Heuristic classifier" subsection under the existing routing-rules section with the exact triggers and the active/cheap/premium mapping.

17. **i18n strings** (`routing.show-heuristics-active`, error/info notes) are added to en.toml as canonical; es/pt/hi get TODO placeholders. rust_i18n falls back to en automatically — same convention as Phases 4-5.

18. **The dead-code rule** ([[feedback_dead_code_in_binary_crate.md]]): every new public TUI item is consumed by non-test code, no `#[allow(dead_code)]` introduced. The classifier lives entirely in `savvagent-host`; the TUI changes are limited to one new locale line and the `render_routing_show` branch.

19. **No new plugin.** `/route show` already exists (Phase 5). `RoutePlugin` does not need a new subcommand because the classifier is gated entirely by the existing `heuristics = true` toggle in `routing.toml`. Users disable by editing the file and running `/route reload`.

20. **Locale-isolation test discipline** ([[feedback_test_locale_isolation.md]]): any new tests in `render_routing_show_tests` or the TUI that assert English text must reset to `"en"` inside `HOME_LOCK`. The pure host-side unit tests (classifier, picker) never read locale state and are safe to run in parallel.

## Goal & Success Criteria

Ship Layer 4 of the parent spec's router stack: a hardcoded heuristic classifier that, when the user opts in via `heuristics = true` in `~/.savvagent/routing.toml`, routes short factoid turns to cheap models and coding-keyword turns to premium models. Override, Modality, and matching Rules all still win when they apply. The classifier is observable via the existing transcript badge (`Heuristic(short)` / `Heuristic(coding)`) and discoverable via the existing `/route show` (which now describes the active categories instead of the Phase 5 "ships in a future release" placeholder).

Measurable success criteria:

1. With `heuristics = true` and a connected Anthropic provider that exposes both `claude-haiku-4-5` (Cheap) and `claude-opus-4-7` (Premium), a turn `"what is 2+2?"` (≤200 chars + `?`) routes to `claude-haiku-4-5` with badge `Heuristic(short)`, even when the active model is `claude-opus-4-7`.
2. With the same setup, a turn `"please refactor this function"` routes to `claude-opus-4-7` (or whichever Premium is active) with badge `Heuristic(coding)`. A turn like `"debug the crash"` produces the same.
3. With `heuristics = false` (default), both the above turns route to Default (active provider's active model); no badge.
4. A turn `"can you debug this?"` (matches both categories) routes via `Coding` (more specific signal), badge `Heuristic(coding)`.
5. When the active model is already in the target tier (e.g. user is on Haiku and asks `"what is 2+2?"`), the classifier returns no decision — Default fires.
6. Layered precedence holds: `@override`, Modality (image-bearing turn that requires vision), and a matching user `[[rule]]` all still win when they apply — verified by integration tests covering each pair (`override > heuristic`, `modality > heuristic`, `rule > heuristic`).
7. `/route show` with `heuristics = true` prints a one-line summary describing both categories and their triggers; with `heuristics = false`, no heuristic line is printed (today's behavior).
8. Workspace + per-crate `cargo test --workspace` is green, including the new `crates/savvagent-host/src/router/heuristics.rs` unit tests, two new scenarios in `crates/savvagent-host/tests/route_rules_e2e.rs` (or a new `heuristic_e2e.rs`), and the updated `render_routing_show_tests` cases.
9. README and CHANGELOG updated in the version-bump commit; `release(0.20.0)` commit message follows the project's existing `release(0.X.0): <one-line>` convention.

## Scope

### In

- New `crates/savvagent-host/src/router/heuristics.rs` module: `HeuristicKind` enum (variants `ShortFactoid`, `Coding`; `#[non_exhaustive]`); `classify(user_text: &str) -> Option<HeuristicKind>`; `pick_for_kind(kind, active_provider, active_model, providers) -> Option<DefaultPick>` returning the chosen `(provider, model)` or `None` when no connected model satisfies the desired tier or when the active model is already in-tier.
- `Router::pick` gains a Layer-4 step between Rules and Default that calls `heuristics::classify` + `heuristics::pick_for_kind` when `rules.heuristics == true`. Same signature as today (zero new parameters).
- `RoutingReason::Heuristic { kind: HeuristicKind }` variant on the existing `#[non_exhaustive]` enum; Display impl renders `Heuristic(short)` / `Heuristic(coding)`.
- `crates/savvagent-host/src/router/mod.rs` declares and re-exports the new module + `HeuristicKind`.
- `crates/savvagent-host/src/lib.rs` re-exports `HeuristicKind` for the TUI's `/route show` formatting (parallels existing `RequiredModalityKind` re-export).
- TUI: extend `render_routing_show` in `crates/savvagent/src/main.rs` to branch on `rules.heuristics` and emit the new active-classifier line via `routing.show-heuristics-active` (replacing the existing `routing.show-heuristics-pending` line when active).
- New i18n key `routing.show-heuristics-active` in en.toml (canonical) + TODO placeholders in es/pt/hi/de.
- Integration coverage: extend `crates/savvagent-host/tests/route_rules_e2e.rs` with at least four scenarios (short-factoid + cheap connected; coding + premium connected; heuristic off → default; coding beats short-factoid for ambiguous input). Add a `render_routing_show` test for the new active-heuristic line.
- README user-facing section: add a "Heuristic classifier" paragraph under the routing-rules section, with the exact triggers, the cheap/premium mapping, and an explicit note that keyword matching is **substring-based** (e.g. `function` matches `functional`) so users aren't surprised by false positives.
- CHANGELOG `## 0.20.0 - 2026-05-19` entry.
- Workspace version bump to `0.20.0` (mirrored in `[workspace.dependencies]` literals per [[feedback_semver]]).

### Out (deferred)

- `[heuristics]` TOML table for tuning keyword lists, length thresholds, or per-kind `provider/model` targets. Tracked as follow-up; v1 ships hardcoded defaults so we can collect real-usage data first (parent spec rationale).
- New `HeuristicKind` variants beyond `ShortFactoid` / `Coding` — `Translation`, `Summarization`, etc. The `#[non_exhaustive]` annotation makes them additive when usage data motivates one.
- ML-based intent classifier — explicitly out of scope per parent spec's "Non-goals" section.
- Cross-conversation learning ("user thumbs-downed last 3 heuristic picks → disable for this conversation") — separate UX problem.
- `/route` subcommand to test the classifier against synthetic input without sending a turn. Useful debugging but not blocking.
- Surfacing the classifier kind in the transcript-export JSON. Today the `RouteSelected` event carries the reason; transcript serialization currently drops it.

## Architecture

### Data types

`crates/savvagent-host/src/router/heuristics.rs` — new module. Public items:

```rust
//! Layer 4 of the router stack — hardcoded heuristic classifier.
//!
//! Gated on `RoutingRules::heuristics == true`. Pure functions, no I/O,
//! no async. Adding new `HeuristicKind` variants is additive thanks to
//! `#[non_exhaustive]`.

use savvagent_protocol::ProviderId;

use crate::capabilities::CostTier;
use crate::router::ProviderView;
use crate::router::rules::DefaultPick;

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
pub fn classify(user_text: &str) -> Option<HeuristicKind> { /* … */ }

/// Pick a `(provider, model)` for a classified turn. Returns `None` when:
/// - No connected model matches the desired tier; OR
/// - The active model is already in the desired tier (no-op routing).
///
/// Tier preferences:
/// - `ShortFactoid` → `[CostTier::Free, CostTier::Cheap]`, first match wins.
/// - `Coding` → `[CostTier::Premium, CostTier::Standard]`, first match wins.
///
/// Per-tier candidate ordering:
/// - Active provider's models first (preserving the provider's `models`
///   declaration order).
/// - Then the rest of the pool in `providers` order.
pub fn pick_for_kind(
    kind: HeuristicKind,
    active_provider: &ProviderId,
    active_model: &str,
    providers: &[ProviderView<'_>],
) -> Option<DefaultPick> { /* … */ }
```

`crates/savvagent-host/src/router/router.rs` gains one new `RoutingReason` variant:

```rust
#[non_exhaustive]
pub enum RoutingReason {
    Override,
    Modality { kind: RequiredModalityKind },
    Rule { name: String },
    /// Layer 4 — heuristic classifier matched. (NEW)
    Heuristic { kind: HeuristicKind },
    Default,
}

impl std::fmt::Display for RoutingReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // …
            RoutingReason::Heuristic { kind } => write!(f, "Heuristic({kind})"),
            // …
        }
    }
}
```

And `Router::pick` gains the layer between Rules and Default:

```rust
// Layer 4 — heuristic classifier (opt-in).
if rules.heuristics
    && let Some(kind) = heuristics::classify(user_text)
    && let Some(pick) = heuristics::pick_for_kind(kind, active_provider, active_model, providers)
{
    return RoutingDecision {
        provider_id: pick.provider,
        model_id: pick.model,
        reason: RoutingReason::Heuristic { kind },
    };
}
```

### Detector contract

`classify(user_text)`:

1. Lowercase `user_text` once.
2. If any of the keyword list is a substring of the lowercased text → `Some(HeuristicKind::Coding)`.
3. Else if `user_text.chars().count() <= 200` AND `user_text.contains('?')` → `Some(HeuristicKind::ShortFactoid)`.
4. Else `None`.

Keyword list (v1, hardcoded): `refactor`, `implement`, `debug`, `fix bug`, `compile`, `stack trace`, `function`, `class`, `error`.

### Picker contract

`pick_for_kind(kind, active_provider, active_model, providers)`:

1. Compute tier preference list: `[Free, Cheap]` for `ShortFactoid`; `[Premium, Standard]` for `Coding`.
2. Look up `active_provider`'s entry in `providers` (linear scan by `ProviderId` equality); within that entry's `ProviderCapabilities.models`, find the model with `id == active_model` by exact-string match. If found and its `cost_tier` is in the preference list → return `None` (no-op routing). If the active model is *not* found in the active provider's catalog (transient registration mismatch), treat as not-in-tier and proceed to step 3 — never panic.
3. For each tier in the preference list:
   - Try active provider's models first (in declaration order); first model with `cost_tier == tier` → return `Some(DefaultPick { provider, model })`.
   - Then iterate the rest of `providers` in input order; first model with `cost_tier == tier` → return.
4. If no tier matches anywhere → return `None`.

`DefaultPick` is reused **as-is** from `rules.rs` — no new fields, no new constructor. Phase 5 already finalized the type; Phase 6 just constructs more of them.

### Data flow

```
User text "please refactor this fn" → run_turn_inner
        │
        ▼
Router::pick(@override=None, providers, active, active_model,
             modality=None, &rules{heuristics=true}, user_text)
  ├─ Layer 1 Override     — None, skip
  ├─ Layer 2 Modality     — no image, skip
  ├─ Layer 3 Rules        — no rule matches "refactor", skip
  ├─ Layer 4 Heuristic    — rules.heuristics = true:
  │     classify("…refactor…") = Some(Coding)
  │     pick_for_kind(Coding, anthropic, haiku, [anthropic{haiku,opus,…}])
  │       active = haiku (Cheap); not in {Premium, Standard} → continue
  │       Premium: anthropic's opus is Premium → DefaultPick{anthropic, opus}
  │     → return RoutingDecision{anthropic, opus, Heuristic(coding)}
  └─ Layer 5 Default      — (not reached)
        │
        ▼
TurnEvent::RouteSelected { provider=anthropic, model=opus, reason=Heuristic(coding) }
        │
        ▼
Provider executes the turn end-to-end on `claude-opus-4-7`.
        │
        ▼
TUI badge renders "▸ anthropic/claude-opus-4-7 — Heuristic(coding)"
```

### TUI changes

One file: `crates/savvagent/src/main.rs::render_routing_show`. Replace:

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

The `routing.show-heuristics-pending` key stays in the catalog (no removal — never broken backward compat on locale keys mid-stream), but is no longer emitted by any code path; cleanup can land in a future commit.

### i18n changes

`crates/savvagent/locales/en.toml` — extend the existing `[routing]` table:

```toml
[routing]
# … existing keys unchanged …
show-heuristics-active = "heuristics: enabled — short-factoid (≤200 chars + '?') routes to cheap models; coding (refactor/implement/debug/…) routes to premium models."
```

`es.toml`, `pt.toml`, `hi.toml`, `de.toml` — same key added with TODO placeholders; rust_i18n falls back to en. Pattern matches Phase 5's locale rollout.

## Error Handling & Edge Cases

- **Empty `user_text`** — `classify("")` returns `None` (no `?`, no keywords). Default fires. No panic.
- **All-whitespace user text** — same as empty; Default fires.
- **Mixed-case keyword input** — `"REFACTOR THIS"` matches via lowercased comparison. Tested.
- **Keyword as a substring of a non-coding word** — `"function"` matches `"functional"`. This is the chosen v1 behavior (parent spec calls out keyword matching; whole-word matching would miss `"refactored"` etc.). Reasoned tradeoff; documented in README. Risk captured under "Risks & Open Questions" as the "noisy classifier" failure mode.
- **Heuristic on but no Premium/Standard model connected for a Coding turn** — `pick_for_kind` returns `None`; Default fires. No styled note; users see the lack of a `Heuristic(coding)` badge as the signal. Symmetric for ShortFactoid + no Cheap/Free connected.
- **Active model already in target tier** — `pick_for_kind` returns `None`; Default fires. Avoids no-op `Heuristic(short)` badges on a Haiku-active session.
- **Heuristic on + matching Rule** — Rule wins because it runs at Layer 3 before Layer 4. Verified by integration test `rule_beats_heuristic`.
- **Heuristic on + image attached + active model lacks vision** — Modality (Layer 2) wins; image routes to the vision-capable sibling. Heuristic never runs for that turn. Verified.
- **Heuristic on + `@gemini:flash` override** — Override (Layer 1) wins. Heuristic never runs. Verified.
- **`heuristics = true` with empty pool** — only the active provider's models (if any) are considered; if pool is empty `Router::pick` is not called (host short-circuits via `NoActiveProvider`). Out of scope here.
- **Tool-use loop iterations within a turn** — heuristic runs *once* at turn start. Subsequent iterations use the same `RoutingDecision` (pinned by the host). Matches Phase 3/4/5 behavior; no special handling.
- **`/route reload` racing an in-flight turn** — the host snapshots `RoutingRules` by `.clone()` before any `.await` (see `crates/savvagent-host/src/session.rs:769`); the classifier in `Router::pick` reads from that snapshot, not from the live `RwLock`. A reload landing mid-turn does not flip the turn's classifier decision. Phase 6 inherits Phase 5's snapshot discipline unchanged.
- **Heuristic chose a provider that gets disconnected mid-turn** — host's existing `ProviderLease` invariants apply; the lease holds the `Arc<dyn ProviderClient>` until the turn completes. Same as Override or Rule scenarios.
- **Locale fallback** — non-en locale missing `routing.show-heuristics-active`: rust_i18n auto-falls-back to en. Tested per [[feedback_test_locale_isolation]] by adding the placeholder to all locales.

## Testing Approach

**Unit tests** (`crates/savvagent-host/src/router/heuristics.rs` `#[cfg(test)] mod tests`):

1. `classify_returns_none_for_empty_input` — `""`, `"  "`, `"hello"`.
2. `classify_short_factoid_requires_question_mark` — `"what is 2+2?"` → `Some(ShortFactoid)`; `"what is 2+2"` → `None`.
3. `classify_short_factoid_respects_200_char_threshold` — `"is this short?"` (≤200) → `Some`; `"is " + "x".repeat(220) + "?"` (>200) → `None`.
4. `classify_coding_matches_each_keyword_case_insensitive` — `"REFACTOR"`, `"please IMPLEMENT", "stack TRACE"` → `Some(Coding)`.
5. `classify_coding_beats_short_factoid_when_both_match` — `"can you debug this?"` (≤200 + `?` + `debug`) → `Some(Coding)`.
6. `classify_substring_match_documented` — `"functional programming"` → `Some(Coding)`; pin the contract.
7. `pick_for_kind_short_factoid_prefers_cheap_then_free` — pool with `Premium`/`Cheap`/`Free` returns the first hit in `[Free, Cheap]` priority (active-provider preferred).
8. `pick_for_kind_coding_prefers_premium_then_standard` — same shape, `[Premium, Standard]` priority.
9. `pick_for_kind_returns_none_when_active_already_in_tier` — active is `Cheap`; `ShortFactoid` returns `None`.
10. `pick_for_kind_returns_none_when_no_tier_matches` — pool has only `Standard` models; `ShortFactoid` returns `None`.
11. `pick_for_kind_walks_pool_when_active_provider_has_no_match` — active provider exposes only `Standard`; `ShortFactoid` finds a `Cheap` on a sibling provider.
12. `pick_for_kind_prefers_active_provider_over_sibling_at_same_tier` — both active and sibling expose a `Premium`; active wins. Pins the same-provider-first guarantee.

**Router-integration tests** (existing `crates/savvagent-host/src/router/router.rs` `#[cfg(test)] mod tests`):

13. `pick_heuristic_short_factoid_routes_to_cheap` — heuristics on, short factoid, active is Premium, pool has Cheap → routes to Cheap with `RoutingReason::Heuristic { ShortFactoid }`.
14. `pick_heuristic_coding_routes_to_premium` — heuristics on, coding keyword, active is Cheap, pool has Premium → routes to Premium.
15. `pick_heuristic_off_falls_through_to_default` — heuristics false, same input as #13 → Default.
16. `pick_rule_beats_heuristic` — both fire; rule wins because Layer 3 runs first.
17. `pick_modality_beats_heuristic` — image attached + coding keyword; Modality wins.
18. `pick_override_beats_heuristic` — `@`-override + coding keyword; Override wins.
19. `pick_heuristic_returns_default_when_active_already_in_tier` — confirms the no-op short-circuit.
20. `routing_reason_heuristic_displays` — Display impl renders `Heuristic(short)` / `Heuristic(coding)`.

**End-to-end** (new file `crates/savvagent-host/tests/heuristic_e2e.rs`, or extending `route_rules_e2e.rs`):

21. `heuristic_short_factoid_e2e` — synthetic `Host` with two providers (Anthropic `haiku`+`opus`, Gemini `flash`), `heuristics=true`, active = opus. Send `"what is 2+2?"`. Assert `RouteSelected` event carries `Heuristic(ShortFactoid)` and provider=anthropic, model=haiku.
22. `heuristic_coding_e2e` — same setup, send `"refactor this function"`. Assert routed to opus (Premium). Active was haiku.
23. `heuristic_off_e2e` — `heuristics=false`, same input as #21 → default; no Heuristic badge.

Per [[feedback_streaming_test_permissions]], pre-register `Allow` via `host.add_session_rule(...)` so synthetic tool-use turns don't hang.

**TUI render test** (`crates/savvagent/src/main.rs::render_routing_show_tests`):

24. `render_routing_show_shows_heuristic_active_line_when_enabled` — Build `RoutingRules { heuristics: true, …Default::default() }`; render; assert one log line contains the localized active-heuristic text.
25. `render_routing_show_omits_heuristic_line_when_disabled` — `heuristics: false`; assert no heuristic line appears.

Per [[feedback_test_locale_isolation.md]], lock locale to `"en"` inside `HOME_LOCK` before asserting English text.

**Soak / fuzz**: no fuzz coverage required for v1; the classifier is a hardcoded substring match, not a regex or parser. Future predicate tuning may motivate a small proptest suite.

## Risks & Open Questions

- **Substring keyword matching is noisy.** `"function"` matches `"functional"`, `"class"` matches `"classroom"`, `"error"` matches `"terror"`. We deliberately ship this in v1 because (a) whole-word matching misses inflections (`refactored`, `debugger`), and (b) the user *chose* to enable heuristics with `heuristics = true`. If post-release feedback says the false-positive rate is too high, the fix is either a whole-word boundary check (one-line change) or moving the list into a `[heuristics] keywords = [...]` TOML field where the user can curate. Tracked in the "Out of scope" follow-up.
- **CostTier as a proxy for "coding-strong" is a heuristic on a heuristic.** A future Premium "Voice" model would be picked by `Coding` even if it's not particularly coding-strong. The parent spec lives with this — the real fix is a `coding_strong: bool` capability flag, which would force every provider plugin to declare it. Deferred until usage data motivates it.
- **`ShortFactoid` threshold of 200 chars is a guess.** First-week dogfooding may show 200 catches too much (long preambles + a single `?`) or too little (concise instructions like `"explain Y combinator"` without a `?`). The fix is a config knob; v1 is the conservative "start tight."
- **No transcript-export coverage of heuristic reason.** Currently the transcript JSON does not serialize `RoutingReason`, so post-mortem "why did it pick that?" requires the TUI log scrollback. Out of scope here; tracked in the parent spec's open question on "transcript export".
- **What happens when the user enables heuristics with a pool that has neither cheap nor premium models?** All inputs fall through to Default. The `/route show` line still says "heuristics: enabled — …routes to cheap/premium models" which is misleading for that pool shape. Phase 6 ships the truthful "the classifier is opt-in and best-effort" framing in README; a future iteration could add a `[no cheap models connected]` annotation to the `/route show` line. Cheap to fix later; not blocking.
- **Should heuristic fire on tool-call-result iterations?** No — heuristic runs once per turn (same as override / rule / modality). A long tool-use loop on a `Coding` turn stays on the chosen `Premium` model for every iteration. This is the current behavior, documented for completeness.
- **Localization of keyword list.** The keyword list is English-only. A user typing in French or Spanish will not trigger `Coding`. Out of scope for v1 since the rest of the UI's i18n surface stops at strings, not user-content NLP.
- **Coding keyword `error` is suspicious.** A user pasting an exception stack trace and asking "what is this error?" will hit *both* `error` (Coding) and the `?` short-factoid path — but Coding wins per assumption #7. That seems right; a stack trace is more useful to route to a Premium model than a Cheap one. Pinned by test #5.

## Phasing notes

This is the last phase of the multi-provider routing initiative. After Phase 6 lands and CI is green, the rollup tag (next version after the most recent real tag — likely `v0.15.0` per [[project_multi_provider_release.md]] in user memory — verify before tagging) is pushed and cargo-dist's Release workflow publishes binaries per [[feedback_cargo_dist_release.md]]. The per-phase `release(0.20.0)` commit in this PR is in-tree scaffolding only, *not* a tag.
