# Multi-provider pool — Phase 3 implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lift Phase 1's "one active provider per conversation" constraint. Users can route individual turns to a specific provider/model via an `@provider:model` (or `@provider`, or `@alias`) prefix on the message; everything else still defaults to the active provider. Conversation history survives cross-provider turns because the host now namespaces `tool_use_id`s with the issuing provider at insertion time, and strips the receiver's own prefix back off before sending each request. The Phase 2 gate (`v0.16.0`) is the safety net that lets this ship: it already proved every receiver's translator accepts foreign-prefixed ids on the wire.

**Architecture:**
- **Router lives in `savvagent-host`.** A new `router/router.rs` module exports `Router::pick(req, ctx, override) -> RoutingDecision` plus the supporting types (`RoutingDecision`, `RoutingReason`, `RoutingOverride`). Phase 3 activates only two of the five planned layers: Layer 1 (`@`-prefix override) and Layer 5 (default). Layers 2-4 (modality, user rules, heuristics) are reserved for Phases 4-6; the `RoutingReason` enum is `#[non_exhaustive]` so adding them later isn't a breaking change.
- **`@`-prefix parser is pure.** `router/prefix.rs` exposes `parse_at_prefix(input, providers, aliases) -> ParsedPrefix` returning `(Option<RoutingOverride>, body)`. The function never touches I/O; the caller (host) is responsible for catching unresolved `@`-tokens (which fall through with `Default` reason) and surfacing the resulting reason in the routing decision. `@@<rest>` strips one leading `@` and treats the body as literal text. Unknown `@token`s do NOT consume the prefix — the message passes through verbatim and the router records `reason = Default` so the user can see their `@token` wasn't recognized.
- **Cross-provider history is keyed by namespaced ids.** The host owns one canonical `Vec<Message>` per conversation. On the way INTO history (after `provider.complete` returns), every `ContentBlock::ToolUse.id` and matching `ContentBlock::ToolResult.tool_use_id` is rewritten to `<provider_id>:<original_id>` — the provider id is the one the router picked for that turn. On the way OUT (before the next `provider.complete` call, for whichever provider is handling the next turn), any id prefixed with the *receiver's own* provider id is stripped back to its raw form; foreign-prefixed ids flow through verbatim. The Phase 2 gate's mocked matrix is the proof that step works.
- **The active provider becomes the default, not a constraint.** `Host::set_active_provider` keeps the conceptual role of "what `/use` and the router's Layer-5 default point at," but no longer clears history. Cross-provider history is now safe; clearing on every `/use` would actually defeat the new capability. The Phase 1 test that locks in the old behavior is updated as part of this plan.
- **`TurnEvent::RouteSelected` is a new variant.** Emitted once per turn, right after `Router::pick` resolves. The TUI receives it on its existing per-turn worker channel and renders a muted `[provider/model — reason]` line above the assistant's response. Implemented as a separate `Entry::RouteBadge(String)` row in the TUI's transcript so existing `Entry::Assistant(_)` call sites stay untouched.
- **Aliases land too.** Phase 3 populates a small list of well-known short names per built-in provider (`opus`/`sonnet`/`haiku` for Anthropic, `flash`/`pro` for Gemini, `gpt-5`/`gpt-4o` for OpenAI). They flow through `ProviderRegistration::aliases` (which already exists, currently empty) into a new `PoolEntry::aliases` field that the router walks at decision time. The router's alias resolution is "scan every connected provider's aliases for an exact match" — multiple matches surface as ambiguous (no override applied, message goes through with `Default`).

**Tech Stack:** Rust 2024, Tokio, `async-trait`. No new workspace dependencies.

**Spec:** `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`. This plan covers **Phase 3 only** — the "`@provider:model` override + cross-provider conversations" entry under "Phasing", with its supporting "Routing layers" / "@provider:model override syntax" / "History and tool_use ID namespacing" / "Data flow" sections. Phase 2 (`v0.16.0`) is already shipped; Phases 4-6 each get their own plan.

---

## File structure (Phase 3)

**New files:**
- `crates/savvagent-host/src/router/prefix.rs` — pure `@`-prefix parser.
- `crates/savvagent-host/src/router/router.rs` — `Router`, `RoutingDecision`, `RoutingReason`, `RoutingOverride`.
- `crates/savvagent-host/src/router/namespace.rs` — pure ID namespacing/stripping helpers used by `Host::run_turn_inner`.
- `crates/savvagent-host/tests/cross_provider_history.rs` — integration test that runs a two-turn conversation across two providers and asserts namespaced ids round-trip.

**Modified files:**
- `crates/savvagent-host/src/router/mod.rs` — declare and re-export the new submodules.
- `crates/savvagent-host/src/lib.rs` — re-export `Router`, `RoutingDecision`, `RoutingReason`, `RoutingOverride`.
- `crates/savvagent-host/src/session.rs` — `TurnEvent::RouteSelected` variant; `run_turn_inner` parses `@`-prefix, invokes router, namespaces ids on append, strips own-prefix on egress; `set_active_provider` no longer calls `clear_history`. Currently 2828 lines; the changes are localized and don't warrant splitting the file.
- `crates/savvagent-host/src/pool.rs` — `PoolEntry::aliases` field; getter; constructor takes aliases.
- `crates/savvagent-host/src/config.rs` — already has `ProviderRegistration::aliases`; no schema change, just flows through to `PoolEntry` now.
- `crates/savvagent-host/tests/pool_lifecycle.rs` — flip `set_active_provider_clears_history_before_swap` to `set_active_provider_preserves_history` (Phase 3 inverts this contract).
- `crates/savvagent/src/app.rs` — `Entry::RouteBadge(String)` variant; `apply_turn_event` handles `TurnEvent::RouteSelected`.
- `crates/savvagent/src/ui.rs` — render `Entry::RouteBadge` as a muted single line.
- `crates/savvagent/src/main.rs` — `/use` no longer clears `app.entries`; `/model` picker no longer filters to the active provider's catalog (picker shows every connected provider's models, qualified as `provider/model`).
- `crates/savvagent/src/plugin/builtin/provider_anthropic/mod.rs` — populate `ModelAlias` entries (`opus`, `sonnet`, `haiku`).
- `crates/savvagent/src/plugin/builtin/provider_gemini/mod.rs` — populate `ModelAlias` entries (`flash`, `pro`).
- `crates/savvagent/src/plugin/builtin/provider_openai/mod.rs` — populate `ModelAlias` entries (`gpt-5`, `gpt-4o`).
- `Cargo.toml` (workspace) — bump `[workspace.package].version` to `0.17.0` and every `version = "0.16.0"` literal in `[workspace.dependencies]` to `0.17.0`.
- `CHANGELOG.md` — add `## 0.17.0 - 2026-05-16` entry.
- `README.md` — short note in the user-facing slash-command section about the `@provider[:model]` prefix.

---

## Task 1: Add `RoutingOverride` + `RoutingReason` + `RoutingDecision` types

**Files:**
- Create: `crates/savvagent-host/src/router/router.rs`

These are pure data types — no I/O, no async. The full `Router::pick` function lands in Task 4; this task just stands up the types and `RoutingReason`'s exhaustive Phase 3 variants. The enum is `#[non_exhaustive]` so Phases 4-6 can add `Modality`, `Rule`, `Heuristic` without breaking downstream `match` arms.

- [ ] **Step 1: Write the failing test**

Create `crates/savvagent-host/src/router/router.rs`:

```rust
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
```

- [ ] **Step 2: Declare the module and re-export the types**

Edit `crates/savvagent-host/src/router/mod.rs`. Replace its contents with:

```rust
//! Routing layers. Phase 3 ships the router skeleton plus `@`-prefix
//! parsing; modality / rules / heuristics arrive in Phases 4-6.

pub mod legacy_model;
pub mod prefix;
pub mod router;

pub use legacy_model::{LegacyModelResolution, ProviderView, resolve_legacy_model};
pub use router::{RoutingDecision, RoutingOverride, RoutingReason};
```

Edit `crates/savvagent-host/src/lib.rs`. Below the existing `pub use router::…` line, extend the list so the new types are re-exported at the crate root:

```rust
pub use router::{
    LegacyModelResolution, ProviderView, RoutingDecision, RoutingOverride,
    RoutingReason, resolve_legacy_model,
};
```

- [ ] **Step 3: Run the unit tests**

Run: `cargo test -p savvagent-host router::router::tests -- --nocapture`
Expected: three tests pass (`routing_reason_displays`, `routing_override_constructs`, `routing_decision_constructs`).

**Heads-up — the `prefix` module declared in `mod.rs` doesn't exist yet.** `cargo test` will fail to compile until Task 2 creates the file. To verify Task 1 in isolation before committing, temporarily comment the `pub mod prefix;` line, run the tests, then uncomment before the next task. Alternatively, defer running these tests until Task 2 lands and run the whole `router::` module in one shot.

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent-host/src/router/mod.rs \
        crates/savvagent-host/src/router/router.rs \
        crates/savvagent-host/src/lib.rs
git commit -m "feat(host): add RoutingDecision/Reason/Override types (Phase 3 skeleton)"
```

---

## Task 2: Pure `@`-prefix parser

**Files:**
- Create: `crates/savvagent-host/src/router/prefix.rs`

Pure function. Walks the connected pool's providers and aliases to resolve a leading `@`-token. Handles `@provider:model`, `@provider`, `@alias`, `@@<rest>` escape, and unknown-token fallthrough. Recognises that slash commands (`/connect …`) bypass `@`-parsing entirely — but slash-command interception lives in the TUI's command palette long before user input reaches the host, so the parser doesn't have to know about it.

- [ ] **Step 1: Write the failing tests**

Create `crates/savvagent-host/src/router/prefix.rs`:

```rust
//! `@provider[:model]` / `@alias` prefix parser.
//!
//! Pure function — given the raw first line of a user turn plus a
//! snapshot of the connected pool's providers and aliases, returns the
//! resolved override (if any) plus the body of the message with the
//! prefix stripped.
//!
//! See spec section "`@provider:model` override syntax" for the full
//! contract, including `@@`-escape and unknown-token fallthrough.

use savvagent_protocol::ProviderId;

use crate::capabilities::ModelAlias;
use crate::router::router::RoutingOverride;
use crate::router::ProviderView;

/// Result of parsing one user message for an `@`-prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPrefix {
    /// The override the parser resolved, if any. `None` means "no
    /// override applies; route normally."
    pub override_: Option<RoutingOverride>,
    /// The message body with the prefix stripped (or the original
    /// message if no prefix applied).
    pub body: String,
}

/// Parse the leading `@`-prefix on `input` against `providers` + `aliases`.
///
/// Rules (in priority order):
///
/// 1. `@@<rest>` — escape. Strip exactly one leading `@`; emit no
///    override; body becomes `@<rest>`.
/// 2. `@provider:model <rest>` — explicit pair. Both `provider` and
///    `model` must exist (provider is connected; model is in that
///    provider's capabilities). Body becomes `<rest>`.
/// 3. `@provider <rest>` — bare provider. The provider must be
///    connected; model becomes `None` (router uses provider's default).
///    Body becomes `<rest>`.
/// 4. `@alias <rest>` — short name. Looks up `alias` across every
///    connected provider's `aliases` list. Exactly one match → resolved
///    override (with that match's provider and model); zero matches →
///    unrecognised, fall through; multiple matches → ambiguous, fall
///    through.
/// 5. Unrecognised `@token` (no provider, no alias matches) — fall
///    through. Override is `None`, body is the **original input**
///    (prefix NOT consumed) so the user's message goes through verbatim
///    and the router records `reason = Default`.
///
/// Inputs that don't start with `@` short-circuit to "no override; body
/// = input".
pub fn parse_at_prefix(
    input: &str,
    providers: &[ProviderView<'_>],
    aliases: &[ModelAlias],
) -> ParsedPrefix {
    let trimmed = input.trim_start();
    let leading_ws_len = input.len() - trimmed.len();

    if !trimmed.starts_with('@') {
        return ParsedPrefix {
            override_: None,
            body: input.to_string(),
        };
    }

    // `@@<rest>` escape.
    if let Some(after_at_at) = trimmed.strip_prefix("@@") {
        // The body keeps one literal '@' plus whatever followed.
        let mut body = String::with_capacity(input.len() - 1);
        body.push_str(&input[..leading_ws_len]);
        body.push('@');
        body.push_str(after_at_at);
        return ParsedPrefix {
            override_: None,
            body,
        };
    }

    // Split off the first whitespace-bounded token (still including the
    // leading '@') and the remainder.
    let after_at = &trimmed[1..]; // skip the leading '@'
    let (token, rest) = match after_at.split_once(char::is_whitespace) {
        Some((t, r)) => (t, r),
        None => (after_at, ""),
    };

    // Empty `@` followed by whitespace (`"@ hi"`) — no token, fall through.
    if token.is_empty() {
        return ParsedPrefix {
            override_: None,
            body: input.to_string(),
        };
    }

    // Try `provider:model` form.
    if let Some((provider_part, model_part)) = token.split_once(':') {
        return resolve_provider_model(input, provider_part, model_part, rest, providers);
    }

    // Try bare provider id.
    if let Ok(pid) = ProviderId::new(token) {
        if providers.iter().any(|p| p.id == &pid) {
            return ParsedPrefix {
                override_: Some(RoutingOverride {
                    provider: pid,
                    model: None,
                }),
                body: rest.to_string(),
            };
        }
    }

    // Try alias lookup.
    let alias_hits: Vec<&ModelAlias> = aliases.iter().filter(|a| a.alias == token).collect();
    match alias_hits.len() {
        1 => {
            let hit = alias_hits[0];
            ParsedPrefix {
                override_: Some(RoutingOverride {
                    provider: hit.provider.clone(),
                    model: Some(hit.model.clone()),
                }),
                body: rest.to_string(),
            }
        }
        // Zero matches → unknown token. Multiple matches → ambiguous.
        // Both fall through with the original input (prefix not consumed).
        _ => ParsedPrefix {
            override_: None,
            body: input.to_string(),
        },
    }
}

fn resolve_provider_model(
    original_input: &str,
    provider_part: &str,
    model_part: &str,
    rest: &str,
    providers: &[ProviderView<'_>],
) -> ParsedPrefix {
    // Reject `@:model` and `@provider:` forms — both are typos.
    if provider_part.is_empty() || model_part.is_empty() {
        return ParsedPrefix {
            override_: None,
            body: original_input.to_string(),
        };
    }
    let Ok(pid) = ProviderId::new(provider_part) else {
        return ParsedPrefix {
            override_: None,
            body: original_input.to_string(),
        };
    };
    let Some(view) = providers.iter().find(|p| p.id == &pid) else {
        return ParsedPrefix {
            override_: None,
            body: original_input.to_string(),
        };
    };
    if view.capabilities.model(model_part).is_none() {
        return ParsedPrefix {
            override_: None,
            body: original_input.to_string(),
        };
    }
    ParsedPrefix {
        override_: Some(RoutingOverride {
            provider: pid,
            model: Some(model_part.into()),
        }),
        body: rest.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{CostTier, ModelAlias, ModelCapabilities, ProviderCapabilities};

    fn anth_caps() -> ProviderCapabilities {
        ProviderCapabilities::new(
            vec![ModelCapabilities {
                id: "claude-opus-4-7".into(),
                display_name: "Claude Opus 4.7".into(),
                supports_vision: true,
                supports_audio: false,
                context_window: 200_000,
                cost_tier: CostTier::Premium,
            }],
            "claude-opus-4-7".into(),
        )
        .expect("valid caps")
    }

    fn gem_caps() -> ProviderCapabilities {
        ProviderCapabilities::new(
            vec![ModelCapabilities {
                id: "gemini-2.0-flash".into(),
                display_name: "Gemini 2.0 Flash".into(),
                supports_vision: true,
                supports_audio: false,
                context_window: 1_000_000,
                cost_tier: CostTier::Cheap,
            }],
            "gemini-2.0-flash".into(),
        )
        .expect("valid caps")
    }

    fn views<'a>(
        anth_id: &'a ProviderId,
        anth: &'a ProviderCapabilities,
        gem_id: &'a ProviderId,
        gem: &'a ProviderCapabilities,
    ) -> Vec<ProviderView<'a>> {
        vec![
            ProviderView {
                id: anth_id,
                capabilities: anth,
            },
            ProviderView {
                id: gem_id,
                capabilities: gem,
            },
        ]
    }

    #[test]
    fn plain_text_no_prefix() {
        let r = parse_at_prefix("hello world", &[], &[]);
        assert_eq!(r.override_, None);
        assert_eq!(r.body, "hello world");
    }

    #[test]
    fn escape_double_at() {
        let r = parse_at_prefix("@@team look at this", &[], &[]);
        assert_eq!(r.override_, None);
        assert_eq!(r.body, "@team look at this");
    }

    #[test]
    fn escape_preserves_leading_whitespace() {
        // Leading whitespace before `@@` is preserved verbatim.
        let r = parse_at_prefix("   @@team", &[], &[]);
        assert_eq!(r.override_, None);
        assert_eq!(r.body, "   @team");
    }

    #[test]
    fn provider_model_form_resolves() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = anth_caps();
        let g_id = ProviderId::new("gemini").unwrap();
        let g_caps = gem_caps();
        let v = views(&a_id, &a_caps, &g_id, &g_caps);

        let r = parse_at_prefix("@anthropic:claude-opus-4-7 refactor this", &v, &[]);
        assert_eq!(
            r.override_,
            Some(RoutingOverride {
                provider: a_id,
                model: Some("claude-opus-4-7".into()),
            })
        );
        assert_eq!(r.body, "refactor this");
    }

    #[test]
    fn bare_provider_form_resolves() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = anth_caps();
        let g_id = ProviderId::new("gemini").unwrap();
        let g_caps = gem_caps();
        let v = views(&a_id, &a_caps, &g_id, &g_caps);

        let r = parse_at_prefix("@gemini summarise this", &v, &[]);
        assert_eq!(
            r.override_,
            Some(RoutingOverride {
                provider: g_id,
                model: None,
            })
        );
        assert_eq!(r.body, "summarise this");
    }

    #[test]
    fn alias_form_resolves_when_unique() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = anth_caps();
        let g_id = ProviderId::new("gemini").unwrap();
        let g_caps = gem_caps();
        let v = views(&a_id, &a_caps, &g_id, &g_caps);
        let aliases = vec![ModelAlias {
            alias: "opus".into(),
            provider: a_id.clone(),
            model: "claude-opus-4-7".into(),
        }];

        let r = parse_at_prefix("@opus design this", &v, &aliases);
        assert_eq!(
            r.override_,
            Some(RoutingOverride {
                provider: a_id,
                model: Some("claude-opus-4-7".into()),
            })
        );
        assert_eq!(r.body, "design this");
    }

    #[test]
    fn alias_ambiguous_falls_through() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = anth_caps();
        let g_id = ProviderId::new("gemini").unwrap();
        let g_caps = gem_caps();
        let v = views(&a_id, &a_caps, &g_id, &g_caps);
        let aliases = vec![
            ModelAlias {
                alias: "flash".into(),
                provider: a_id,
                model: "claude-opus-4-7".into(),
            },
            ModelAlias {
                alias: "flash".into(),
                provider: g_id,
                model: "gemini-2.0-flash".into(),
            },
        ];

        let r = parse_at_prefix("@flash explain", &v, &aliases);
        assert_eq!(r.override_, None);
        // Ambiguous tokens are NOT consumed — full input passes through.
        assert_eq!(r.body, "@flash explain");
    }

    #[test]
    fn unknown_token_not_consumed() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = anth_caps();
        let g_id = ProviderId::new("gemini").unwrap();
        let g_caps = gem_caps();
        let v = views(&a_id, &a_caps, &g_id, &g_caps);

        let r = parse_at_prefix("@nonexistent hi", &v, &[]);
        assert_eq!(r.override_, None);
        assert_eq!(r.body, "@nonexistent hi");
    }

    #[test]
    fn provider_model_unknown_model_not_consumed() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = anth_caps();
        let g_id = ProviderId::new("gemini").unwrap();
        let g_caps = gem_caps();
        let v = views(&a_id, &a_caps, &g_id, &g_caps);

        let r = parse_at_prefix("@anthropic:no-such-model hi", &v, &[]);
        assert_eq!(r.override_, None);
        assert_eq!(r.body, "@anthropic:no-such-model hi");
    }

    #[test]
    fn provider_model_unknown_provider_not_consumed() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = anth_caps();
        let g_id = ProviderId::new("gemini").unwrap();
        let g_caps = gem_caps();
        let v = views(&a_id, &a_caps, &g_id, &g_caps);

        let r = parse_at_prefix("@openai:gpt-5 hi", &v, &[]);
        assert_eq!(r.override_, None);
        assert_eq!(r.body, "@openai:gpt-5 hi");
    }

    #[test]
    fn empty_after_at_falls_through() {
        let r = parse_at_prefix("@ hi", &[], &[]);
        assert_eq!(r.override_, None);
        assert_eq!(r.body, "@ hi");
    }

    #[test]
    fn prefix_with_no_body() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = anth_caps();
        let g_id = ProviderId::new("gemini").unwrap();
        let g_caps = gem_caps();
        let v = views(&a_id, &a_caps, &g_id, &g_caps);

        // `@anthropic` alone (no message after it) is a valid override
        // with an empty body — the host can decide what to do with that.
        let r = parse_at_prefix("@anthropic", &v, &[]);
        assert_eq!(
            r.override_,
            Some(RoutingOverride {
                provider: ProviderId::new("anthropic").unwrap(),
                model: None,
            })
        );
        assert_eq!(r.body, "");
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p savvagent-host router::prefix -- --nocapture`
Expected: every test passes (11 tests). If any fail, debug the parser before moving on — bugs here corrupt every subsequent task.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent-host/src/router/prefix.rs
git commit -m "feat(host): @-prefix parser with @@-escape + unknown-token fallthrough"
```

---

## Task 3: Pure ID namespacing helpers

**Files:**
- Create: `crates/savvagent-host/src/router/namespace.rs`

Two pure functions plus a small struct used by `Host::run_turn_inner`:

- `namespace_assistant_content(blocks, provider_id) -> Vec<ContentBlock>` — rewrites `ToolUse.id` to `<provider_id>:<id>` (idempotent: if a block is already namespaced for this provider, it's left alone).
- `strip_own_prefix_in_history(messages, receiver_id) -> Vec<Message>` — for each `ToolUse.id` and `ToolResult.tool_use_id`, strips the `<receiver_id>:` prefix if present; leaves foreign-prefixed ids alone.

Both functions are pure and unit-tested in isolation. The host wires them into the turn loop in Task 5.

- [ ] **Step 1: Write the failing tests**

Create `crates/savvagent-host/src/router/namespace.rs`:

```rust
//! Pure helpers for cross-provider `tool_use_id` namespacing.
//!
//! The host owns one canonical `Vec<Message>` per conversation. On the
//! way INTO history (after `provider.complete` returns), tool-use ids
//! get a `<provider_id>:` prefix so future turns can tell which
//! provider issued them. On the way OUT (before the next request), the
//! receiver's own prefix is stripped back off so the vendor sees
//! "raw" ids for its own history; foreign-prefixed ids pass through
//! verbatim and rely on the Phase 2 gate's "translators accept opaque
//! string ids" invariant.
//!
//! Both functions are pure — they take ownership / borrow only of the
//! values they need, never touch I/O.

use savvagent_protocol::{ContentBlock, Message, ProviderId};

/// Rewrite every `ContentBlock::ToolUse.id` in `blocks` to
/// `<provider_id>:<id>`. Idempotent: if a block's id is already prefixed
/// with this provider id, it's left unchanged. Foreign-prefixed ids
/// (`other_provider:foo`) are also left unchanged — they shouldn't be
/// possible at this code path (these are blocks the *current* provider
/// just emitted), but defending against a misbehaving provider is cheap.
pub fn namespace_assistant_content(
    blocks: Vec<ContentBlock>,
    provider_id: &ProviderId,
) -> Vec<ContentBlock> {
    let prefix = format!("{}:", provider_id.as_str());
    blocks
        .into_iter()
        .map(|b| namespace_block(b, &prefix))
        .collect()
}

fn namespace_block(block: ContentBlock, prefix: &str) -> ContentBlock {
    match block {
        ContentBlock::ToolUse { id, name, input } => {
            let new_id = if id.contains(':') {
                // Already has a provider prefix (this one or another).
                // Leave it alone.
                id
            } else {
                format!("{prefix}{id}")
            };
            ContentBlock::ToolUse {
                id: new_id,
                name,
                input,
            }
        }
        // Other blocks pass through unchanged. ToolResult is only ever
        // emitted by the host itself (synthesised after running a tool),
        // and the host emits already-namespaced ids — see
        // `run_turn_inner` in `session.rs`.
        other => other,
    }
}

/// Build a fresh `Vec<Message>` where every `ContentBlock::ToolUse.id`
/// and every `ContentBlock::ToolResult.tool_use_id` has the
/// `<receiver_id>:` prefix stripped (if present). Foreign-prefixed ids
/// (different provider's prefix, or no prefix at all) pass through
/// verbatim.
pub fn strip_own_prefix_in_history(messages: &[Message], receiver_id: &ProviderId) -> Vec<Message> {
    let prefix = format!("{}:", receiver_id.as_str());
    messages
        .iter()
        .map(|m| Message {
            role: m.role,
            content: m.content.iter().map(|b| strip_block(b, &prefix)).collect(),
        })
        .collect()
}

fn strip_block(block: &ContentBlock, prefix: &str) -> ContentBlock {
    match block {
        ContentBlock::ToolUse { id, name, input } => ContentBlock::ToolUse {
            id: id.strip_prefix(prefix).unwrap_or(id).to_string(),
            name: name.clone(),
            input: input.clone(),
        },
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => ContentBlock::ToolResult {
            tool_use_id: tool_use_id.strip_prefix(prefix).unwrap_or(tool_use_id).to_string(),
            content: content.iter().map(|b| strip_block(b, prefix)).collect(),
            is_error: *is_error,
        },
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_protocol::Role;
    use serde_json::json;

    fn anth() -> ProviderId {
        ProviderId::new("anthropic").unwrap()
    }
    fn gem() -> ProviderId {
        ProviderId::new("gemini").unwrap()
    }

    #[test]
    fn namespace_prefixes_tool_use_ids() {
        let blocks = vec![
            ContentBlock::Text {
                text: "ok".into(),
            },
            ContentBlock::ToolUse {
                id: "toolu_abc".into(),
                name: "list_dir".into(),
                input: json!({ "path": "." }),
            },
        ];
        let out = namespace_assistant_content(blocks, &anth());
        assert!(matches!(out[0], ContentBlock::Text { .. }));
        let ContentBlock::ToolUse { id, .. } = &out[1] else {
            panic!("expected ToolUse");
        };
        assert_eq!(id, "anthropic:toolu_abc");
    }

    #[test]
    fn namespace_is_idempotent_for_own_prefix() {
        let blocks = vec![ContentBlock::ToolUse {
            id: "anthropic:toolu_abc".into(),
            name: "list_dir".into(),
            input: json!({}),
        }];
        let out = namespace_assistant_content(blocks, &anth());
        let ContentBlock::ToolUse { id, .. } = &out[0] else {
            panic!("expected ToolUse");
        };
        assert_eq!(id, "anthropic:toolu_abc");
    }

    #[test]
    fn namespace_leaves_foreign_prefix_alone() {
        let blocks = vec![ContentBlock::ToolUse {
            id: "gemini:toolu_abc".into(),
            name: "list_dir".into(),
            input: json!({}),
        }];
        let out = namespace_assistant_content(blocks, &anth());
        let ContentBlock::ToolUse { id, .. } = &out[0] else {
            panic!("expected ToolUse");
        };
        assert_eq!(id, "gemini:toolu_abc");
    }

    #[test]
    fn strip_own_prefix_removes_matching() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "anthropic:toolu_abc".into(),
                name: "list_dir".into(),
                input: json!({}),
            }],
        }];
        let out = strip_own_prefix_in_history(&messages, &anth());
        let ContentBlock::ToolUse { id, .. } = &out[0].content[0] else {
            panic!("expected ToolUse");
        };
        assert_eq!(id, "toolu_abc");
    }

    #[test]
    fn strip_own_prefix_leaves_foreign() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "gemini:toolu_abc".into(),
                name: "list_dir".into(),
                input: json!({}),
            }],
        }];
        let out = strip_own_prefix_in_history(&messages, &anth());
        let ContentBlock::ToolUse { id, .. } = &out[0].content[0] else {
            panic!("expected ToolUse");
        };
        // Gemini-prefixed id flows through Anthropic unchanged.
        assert_eq!(id, "gemini:toolu_abc");
    }

    #[test]
    fn strip_own_prefix_handles_tool_result() {
        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "anthropic:toolu_abc".into(),
                content: vec![ContentBlock::Text { text: "ok".into() }],
                is_error: false,
            }],
        }];
        let out = strip_own_prefix_in_history(&messages, &anth());
        let ContentBlock::ToolResult { tool_use_id, .. } = &out[0].content[0] else {
            panic!("expected ToolResult");
        };
        assert_eq!(tool_use_id, "toolu_abc");
    }
}
```

- [ ] **Step 2: Declare the module**

Edit `crates/savvagent-host/src/router/mod.rs` to add the new submodule. After the existing `pub mod router;` line:

```rust
pub mod namespace;
```

The functions are crate-internal — they're only used by `session.rs` — so no `pub use` needs to land in `lib.rs`.

- [ ] **Step 3: Run the tests**

Run: `cargo test -p savvagent-host router::namespace -- --nocapture`
Expected: six tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent-host/src/router/namespace.rs \
        crates/savvagent-host/src/router/mod.rs
git commit -m "feat(host): pure helpers for cross-provider tool_use_id namespacing"
```

---

## Task 4: `PoolEntry::aliases` + propagate through `add_provider`

**Files:**
- Modify: `crates/savvagent-host/src/pool.rs`
- Modify: `crates/savvagent-host/src/session.rs` (the `Host::add_provider` body)

The router needs access to every connected provider's aliases at decision time. `ProviderRegistration::aliases` already exists in `config.rs`; this task plumbs it into a new `PoolEntry` field and exposes a getter so the host can collect aliases when invoking the router.

- [ ] **Step 1: Extend `PoolEntry`**

Edit `crates/savvagent-host/src/pool.rs`. In the `PoolEntry` struct, add an `aliases` field. In `PoolEntry::new`, take `aliases` as the third constructor argument (between `capabilities` and `display_name` for symmetry with `ProviderRegistration`), and store it.

Locate the struct (around line 53):

```rust
pub struct PoolEntry {
    client: Arc<dyn ProviderClient + Send + Sync>,
    capabilities: ProviderCapabilities,
    display_name: String,
    /// Number of [`ProviderLease`]s currently outstanding for this entry.
    /// Used by drain-disconnect to wait until in-flight turns finish.
    active_turns: Arc<AtomicUsize>,
}
```

Replace with:

```rust
pub struct PoolEntry {
    client: Arc<dyn ProviderClient + Send + Sync>,
    capabilities: ProviderCapabilities,
    aliases: Vec<crate::capabilities::ModelAlias>,
    display_name: String,
    /// Number of [`ProviderLease`]s currently outstanding for this entry.
    /// Used by drain-disconnect to wait until in-flight turns finish.
    active_turns: Arc<AtomicUsize>,
}
```

Locate `PoolEntry::new` (around line 64) and replace it with:

```rust
impl PoolEntry {
    /// Construct a new entry with no active leases.
    pub fn new(
        client: Arc<dyn ProviderClient + Send + Sync>,
        capabilities: ProviderCapabilities,
        aliases: Vec<crate::capabilities::ModelAlias>,
        display_name: String,
    ) -> Self {
        Self {
            client,
            capabilities,
            aliases,
            display_name,
            active_turns: Arc::new(AtomicUsize::new(0)),
        }
    }
```

Add a getter below `capabilities()`:

```rust
    /// The model aliases advertised by this provider.
    pub fn aliases(&self) -> &[crate::capabilities::ModelAlias] {
        &self.aliases
    }
```

Update the existing `PoolEntry::new(...)` call inside the `tests` module at the bottom of `pool.rs`. Locate the `fn entry()` helper (around line 130) and pass `Vec::new()` for the new aliases parameter:

```rust
    fn entry() -> PoolEntry {
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
        PoolEntry::new(Arc::new(StubClient), caps, Vec::new(), "Stub".into())
    }
```

- [ ] **Step 2: Update every `PoolEntry::new` call site outside `pool.rs`**

Run: `grep -rn 'PoolEntry::new' crates/`
Expected hits: `crates/savvagent-host/src/session.rs` (three call sites — `Host::start`'s pool-build path, `Host::start`'s legacy fallback, and `Host::with_components`), plus the `add_provider` body. Update each to pass the new `aliases` argument:

In `Host::start`'s pool-build path (around line 336), the call is currently:

```rust
PoolEntry::new(
    Arc::clone(&reg.client),
    reg.capabilities.clone(),
    reg.display_name.clone(),
),
```

Change to:

```rust
PoolEntry::new(
    Arc::clone(&reg.client),
    reg.capabilities.clone(),
    reg.aliases.clone(),
    reg.display_name.clone(),
),
```

In `Host::start`'s legacy fallback (around line 376):

```rust
let entry = PoolEntry::new(provider_arc, caps, "Default".into());
```

Change to:

```rust
let entry = PoolEntry::new(provider_arc, caps, Vec::new(), "Default".into());
```

In `Host::with_components` (around line 469):

```rust
let entry = PoolEntry::new(provider_arc, caps, "Default".into());
```

Change to:

```rust
let entry = PoolEntry::new(provider_arc, caps, Vec::new(), "Default".into());
```

In `Host::add_provider` (around line 1231):

```rust
pool.insert(
    reg.id.clone(),
    PoolEntry::new(reg.client, reg.capabilities, reg.display_name),
);
```

Change to:

```rust
pool.insert(
    reg.id.clone(),
    PoolEntry::new(reg.client, reg.capabilities, reg.aliases, reg.display_name),
);
```

- [ ] **Step 3: Verify the changes compile**

Run: `cargo check -p savvagent-host --tests`
Expected: clean build. If any `PoolEntry::new` call site is missing the new argument, the compiler points at it.

- [ ] **Step 4: Run the pool tests**

Run: `cargo test -p savvagent-host pool::tests -- --nocapture`
Expected: both `lease_increments_and_drop_decrements` and `lease_keeps_client_alive_after_entry_drop` still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/pool.rs crates/savvagent-host/src/session.rs
git commit -m "feat(host): thread ModelAlias into PoolEntry"
```

---

## Task 5: `Router::pick` + `TurnEvent::RouteSelected`

**Files:**
- Modify: `crates/savvagent-host/src/router/router.rs` (add `Router::pick`)
- Modify: `crates/savvagent-host/src/session.rs` (add `TurnEvent::RouteSelected`)

The router takes the parsed `@`-prefix override (if any), the snapshot of connected providers, and the active provider + model. Phase 3 layers: if `override_` is `Some` → `Override`; else → `Default` (active provider + active model).

- [ ] **Step 1: Write the failing test**

Append to `crates/savvagent-host/src/router/router.rs`, below the existing `#[cfg(test)] mod tests {` block (extend the same module, don't open a new one). Replace the existing `tests` module with:

```rust
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
```

Above that test block, add the `Router` struct and `pick` function. Insert this AFTER the `RoutingDecision` struct definition and BEFORE the `#[cfg(test)]` line:

```rust
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
```

- [ ] **Step 2: Run the router tests**

Run: `cargo test -p savvagent-host router::router -- --nocapture`
Expected: seven tests pass (the original three from Task 1, plus four new `pick_*` tests).

- [ ] **Step 3: Add `TurnEvent::RouteSelected`**

Edit `crates/savvagent-host/src/session.rs`. Locate the `TurnEvent` enum (around line 169) and insert a new variant before `IterationStarted`:

```rust
    /// The router picked a `(provider, model)` for this turn. Emitted
    /// once, before any `IterationStarted`, so the TUI can render a
    /// per-turn routing badge above the assistant's response.
    RouteSelected {
        /// The chosen provider for this turn.
        provider_id: savvagent_protocol::ProviderId,
        /// The chosen model for this turn.
        model_id: String,
        /// Why the router picked it (rendered as "Override" / "Default" today).
        reason: crate::router::RoutingReason,
    },
```

Adding the variant doesn't break callers that consume `TurnEvent` via `match` because most existing matches use `_` for tail branches; we'll handle the TUI's match arms explicitly in Task 8.

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p savvagent-host`
Expected: clean. The new variant is not yet emitted, so no runtime behavior changes — Task 6 wires it in.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/router/router.rs \
        crates/savvagent-host/src/session.rs
git commit -m "feat(host): Router::pick + TurnEvent::RouteSelected (Phase 3 layers 1+5)"
```

---

## Task 6: Wire prefix parser + router + ID namespacing into `Host::run_turn_inner`

**Files:**
- Modify: `crates/savvagent-host/src/session.rs`

The big integration task. `run_turn_inner` already lives at around line 547-887. Phase 3 adds:

1. Parse the `@`-prefix from `user_input` against the pool snapshot. Push the *body* (not the original input) as the user message.
2. Build a `[ProviderView]` and a flat alias list from the pool, invoke `Router::pick`.
3. Emit `TurnEvent::RouteSelected` if `events` is `Some`.
4. **Pin** the routed `(provider_id, model_id)` for the whole turn (replace the per-iteration `active_provider.read()` and `current_model.read()` calls).
5. Before each `provider.complete`, strip the receiver's own prefix from a working copy of `messages`.
6. After each `provider.complete`, namespace the returned `resp.content` to `<provider_id>:` before extracting tool_uses and before appending to `messages`.

- [ ] **Step 1: Read the current `run_turn_inner` body so the diff below is unambiguous**

Run: `sed -n '547,887p' crates/savvagent-host/src/session.rs`
Expected: the function body from "`async fn run_turn_inner`" to its closing brace. Skim it once before editing — the changes below replace specific spans, not the whole function.

- [ ] **Step 2: Parse the `@`-prefix and run the router at the start of `run_turn_inner`**

In `run_turn_inner`, locate the block that pushes the user message (around line 559-566):

```rust
        let mut messages = {
            let s = self.state.lock().await;
            s.messages.clone()
        };
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: user_input }],
        });
```

Replace with:

```rust
        let mut messages = {
            let s = self.state.lock().await;
            s.messages.clone()
        };

        // Parse the `@`-prefix against the currently-connected pool.
        // Aliases are flattened across every connected provider so
        // `@opus` works even if the active provider is Gemini.
        let parsed = {
            let pool = self.pool.read().await;
            let views: Vec<crate::router::ProviderView<'_>> = pool
                .iter()
                .map(|(id, entry)| crate::router::ProviderView {
                    id,
                    capabilities: entry.capabilities(),
                })
                .collect();
            let aliases: Vec<crate::capabilities::ModelAlias> = pool
                .values()
                .flat_map(|entry| entry.aliases().to_vec())
                .collect();
            crate::router::prefix::parse_at_prefix(&user_input, &views, &aliases)
            // Pool read guard dropped at end of this block.
        };

        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: parsed.body,
            }],
        });

        // Run the router. The decision pins the provider + model for the
        // entire turn, including every tool-use iteration.
        let decision = {
            let pool = self.pool.read().await;
            let views: Vec<crate::router::ProviderView<'_>> = pool
                .iter()
                .map(|(id, entry)| crate::router::ProviderView {
                    id,
                    capabilities: entry.capabilities(),
                })
                .collect();
            let active_id: ProviderId = self.active_provider.read().await.clone();
            let active_model: String = self.current_model.read().await.clone();
            crate::router::Router::pick(parsed.override_, &views, &active_id, &active_model)
        };

        if let Some(tx) = &events {
            let _ = tx
                .send(TurnEvent::RouteSelected {
                    provider_id: decision.provider_id.clone(),
                    model_id: decision.model_id.clone(),
                    reason: decision.reason.clone(),
                })
                .await;
        }
```

- [ ] **Step 3: Replace per-iteration `active_provider` + `current_model` reads with the pinned decision**

Inside the same `run_turn_inner` body, find the section that reads `self.current_model` and the section that reads `self.active_provider` (currently at around line 591 and line 636 respectively).

The `CompleteRequest` literal (around line 591) currently begins:

```rust
            let req = CompleteRequest {
                model: self.current_model.read().await.clone(),
                messages: messages.clone(),
                system: self.system_prompt.clone(),
                ...
            };
```

Replace with:

```rust
            // History sent to the provider has the receiver's own prefix
            // stripped from every tool_use_id. Foreign-prefixed ids pass
            // through verbatim — the Phase 2 gate proved each translator
            // accepts them. See router::namespace docs for the contract.
            let req_messages = crate::router::namespace::strip_own_prefix_in_history(
                &messages,
                &decision.provider_id,
            );

            let req = CompleteRequest {
                model: decision.model_id.clone(),
                messages: req_messages,
                system: self.system_prompt.clone(),
                ...
            };
```

(Keep the rest of the `CompleteRequest` literal unchanged.)

And the `active_id` snapshot (around line 636):

```rust
            let active_id: ProviderId = self.active_provider.read().await.clone();
```

Replace with:

```rust
            let active_id: ProviderId = decision.provider_id.clone();
```

The lease acquisition and cancel-signal wiring (around line 638-666) continues to use `active_id` — no further changes needed there.

- [ ] **Step 4: Namespace the assistant's tool_use ids on the way INTO history**

Find the block that appends the assistant turn (around line 740-745):

```rust
            messages.push(Message {
                role: Role::Assistant,
                content: resp.content.clone(),
            });

            let mut tool_uses: Vec<(String, String, Value)> = Vec::new();
            let mut text_buf = String::new();
            for block in &resp.content {
                ...
            }
```

Replace with:

```rust
            // Namespace every ToolUse.id in the returned assistant
            // content. Future turns that route to a different provider
            // see `<this-provider>:<id>` in history; the receiving
            // provider's adapter accepts that as an opaque string
            // (Phase 2 gate), and `strip_own_prefix_in_history` strips
            // the prefix back off if the *next* turn lands on the same
            // provider.
            let namespaced_content = crate::router::namespace::namespace_assistant_content(
                resp.content.clone(),
                &decision.provider_id,
            );
            messages.push(Message {
                role: Role::Assistant,
                content: namespaced_content.clone(),
            });

            let mut tool_uses: Vec<(String, String, Value)> = Vec::new();
            let mut text_buf = String::new();
            for block in &namespaced_content {
                ...
            }
```

The `tool_uses` vec now carries already-namespaced `tool_use_id` strings; the subsequent loop that synthesises `ContentBlock::ToolResult { tool_use_id, … }` uses those namespaced ids verbatim, so history stays consistent.

- [ ] **Step 5: Verify the change compiles**

Run: `cargo check -p savvagent-host`
Expected: clean. The new code paths use only types already in scope (`ProviderView`, `ModelAlias`, `Router`, `prefix::parse_at_prefix`, `namespace::strip_own_prefix_in_history`, `namespace::namespace_assistant_content`).

- [ ] **Step 6: Run the host's existing test suite to confirm no regressions**

Run: `cargo test -p savvagent-host --no-fail-fast`
Expected: every test passes except `set_active_provider_clears_history_before_swap` in `tests/pool_lifecycle.rs` — that's Phase 1's contract, which Task 7 flips. (If anything else fails, debug before moving on.)

- [ ] **Step 7: Commit**

```bash
git add crates/savvagent-host/src/session.rs
git commit -m "feat(host): route turns via Router + namespace tool_use_ids on history append"
```

---

## Task 7: `Host::set_active_provider` no longer clears history; flip the Phase 1 test

**Files:**
- Modify: `crates/savvagent-host/src/session.rs`
- Modify: `crates/savvagent-host/tests/pool_lifecycle.rs`

Phase 3 makes cross-provider history safe — clearing it on every `/use` defeats the new capability. `set_active_provider` now just swaps the active id; the user keeps their conversation.

- [ ] **Step 1: Remove `clear_history` from `set_active_provider`**

Edit `crates/savvagent-host/src/session.rs`. Locate `set_active_provider` (around line 1361-1378) and update both the doc comment and the body:

```rust
    /// Switch the active provider. The active provider is the default the
    /// router falls through to when no `@`-prefix override applies.
    ///
    /// Phase 3+: conversation history is **preserved** across this switch.
    /// The next turn's `tool_use_id`s will be prefixed with the new active
    /// provider's id; older history blocks keep their original prefixes,
    /// which the receiving provider's translator accepts as opaque strings
    /// (Phase 2 cross-vendor gate).
    ///
    /// Returns [`PoolError::NotRegistered`] if `id` is not in the pool.
    pub async fn set_active_provider(&self, id: &ProviderId) -> Result<(), PoolError> {
        // Validate first, before mutating any state.
        {
            let pool = self.pool.read().await;
            if !pool.contains_key(id) {
                return Err(PoolError::NotRegistered(id.clone()));
            }
        }
        *self.active_provider.write().await = id.clone();
        Ok(())
    }
```

- [ ] **Step 2: Flip the Phase 1 history-clear test**

Edit `crates/savvagent-host/tests/pool_lifecycle.rs`. Locate `set_active_provider_clears_history_before_swap` (around line 686-733). Replace the entire function — including its doc comment — with:

```rust
/// Verifies that `set_active_provider` **preserves** the conversation
/// history when swapping the active id. Phase 3 makes cross-provider
/// history safe (the Phase 2 gate proved every receiver accepts
/// foreign-prefixed `tool_use_id`s); clearing on `/use` would defeat the
/// new capability.
///
/// Set-up: a host with two providers in the pool. Drop a synthetic
/// assistant message into history (simulating a completed turn), call
/// `set_active_provider(gemini)`, and assert the message is still there.
#[tokio::test]
async fn set_active_provider_preserves_history() {
    let (host, _temp) = build_host_with_two_providers().await;

    // Inject a synthetic assistant turn into the host's state so we can
    // observe whether the swap preserves it.
    host.append_message_for_test(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "anthropic said hi".into(),
        }],
    })
    .await;
    assert_eq!(host.messages().await.len(), 1);

    let gemini = ProviderId::new("gemini").unwrap();
    host.set_active_provider(&gemini).await.unwrap();
    assert_eq!(
        host.active_provider().await,
        gemini,
        "active provider should be gemini after set_active_provider"
    );
    assert_eq!(
        host.messages().await.len(),
        1,
        "set_active_provider must NOT clear history in Phase 3"
    );
}
```

**Heads-up:** This test uses two helpers that may not exist yet: `build_host_with_two_providers()` and `host.append_message_for_test(...)`. Check whether they're already defined elsewhere in this test file or in `crates/savvagent-host/tests/support/mod.rs`:

Run: `grep -n 'build_host_with_two_providers\|append_message_for_test' crates/savvagent-host/`
- If `build_host_with_two_providers` is missing, copy the setup from the old `set_active_provider_clears_history_before_swap` body (it built exactly this) into a fresh helper at the top of `pool_lifecycle.rs`.
- If `append_message_for_test` is missing, add a `#[doc(hidden)] pub async fn append_message_for_test(&self, msg: Message)` method on `Host` that locks `state` and pushes the message. That's the minimum surgery to keep the test honest; a fuller fix is "expose a `Host` constructor that takes initial history" but YAGNI for Phase 3.

- [ ] **Step 3: Run the affected test**

Run: `cargo test -p savvagent-host --test pool_lifecycle set_active_provider_preserves_history -- --nocapture`
Expected: passes.

- [ ] **Step 4: Re-run the whole crate's tests to confirm no other regression**

Run: `cargo test -p savvagent-host --no-fail-fast`
Expected: every test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/session.rs \
        crates/savvagent-host/tests/pool_lifecycle.rs
git commit -m "feat(host): set_active_provider preserves history; cross-provider safe in Phase 3"
```

---

## Task 8: TUI handles `TurnEvent::RouteSelected`; render route badge

**Files:**
- Modify: `crates/savvagent/src/app.rs`
- Modify: `crates/savvagent/src/ui.rs`

A new `Entry::RouteBadge(String)` variant captures the muted single-line badge that renders above each assistant response. Pushing it as a separate entry (rather than threading state into `Entry::Assistant`) keeps every existing `Entry::Assistant(_)` call site — including transcript persistence — untouched. The badge text format mirrors the spec: `provider/model — Reason`.

- [ ] **Step 1: Add the new entry variant**

Edit `crates/savvagent/src/app.rs`. Locate the `Entry` enum (around line 173) and add a variant. Replace the enum with:

```rust
/// One row in the conversation log.
#[derive(Debug, Clone)]
pub enum Entry {
    /// Submitted user prompt.
    User(String),
    /// Finalized assistant text.
    Assistant(String),
    /// Tool the model is calling (or just called). `status = None` means in-flight.
    Tool {
        /// Tool name.
        name: String,
        /// One-line summary of the JSON arguments.
        arguments: String,
        /// Outcome (None while running).
        status: Option<ToolCallStatus>,
        /// Truncated payload (only set after completion).
        result_preview: Option<String>,
    },
    /// Per-turn routing badge — rendered as a muted single line above
    /// the assistant entry that follows it. Source: `TurnEvent::RouteSelected`.
    /// Format: `"provider/model — Reason"` (e.g. `"anthropic/claude-opus-4-7 — Override"`).
    RouteBadge(String),
    /// Local notice — file ops, errors, transcript notifications.
    Note(String),
}
```

- [ ] **Step 2: Handle `TurnEvent::RouteSelected` in `apply_turn_event`**

Edit `crates/savvagent/src/app.rs`. Locate `apply_turn_event` (around line 531) and add a new match arm. Add it right before the `TurnEvent::IterationStarted` arm so the badge lands before any tool / text rendering for the turn:

```rust
            TurnEvent::RouteSelected {
                provider_id,
                model_id,
                reason,
            } => {
                self.flush_live_text();
                self.entries.push(Entry::RouteBadge(format!(
                    "{}/{} — {}",
                    provider_id.as_str(),
                    model_id,
                    reason
                )));
            }
```

- [ ] **Step 3: Update other `Entry::*` match sites for the new variant**

Search for existing exhaustive matches on `Entry`. The compiler will flag any missing arms.

Run: `cargo check -p savvagent`
Expected: any "non-exhaustive patterns" error points at sites that need an `Entry::RouteBadge(_) => …` arm. Likely call sites (per the grep done during plan write-up):
  - `crates/savvagent/src/app.rs:655` — `update_metrics` byte-count tally. Treat as the same length category as `Note`:

    ```rust
    Entry::User(t) | Entry::Assistant(t) | Entry::Note(t) | Entry::RouteBadge(t) => t.len(),
    ```

  - `crates/savvagent/src/app.rs:1309` — transcript export. Add:

    ```rust
    Entry::RouteBadge(t) => format!("route: {t}"),
    ```

    above the existing `Entry::Note(t)` arm.
  - `crates/savvagent/src/app.rs:1765` — entries → notes filter. Decide: badges are NOT notes; leave them out of the filter (`Entry::RouteBadge(_) => None`).

Add an arm to every match found by `cargo check`. None of them should panic on the new variant; each treats the badge as either a short string or skips it.

- [ ] **Step 4: Render `Entry::RouteBadge` in the transcript**

Edit `crates/savvagent/src/ui.rs`. The per-entry rendering switch already handles `Entry::User` / `Entry::Assistant` / `Entry::Tool` / `Entry::Note` — `cargo check` from Step 3 pointed at it as a non-exhaustive match site.

Render `Entry::RouteBadge` as a single muted line prefixed with `"▸ "`, copying the styling pattern `Entry::Note` already uses (both are short, secondary messages). The difference is the prefix glyph (`▸` for routing, no prefix for notes). Concrete snippet, slotted alongside the existing `Entry::Note` arm:

```rust
Entry::RouteBadge(text) => {
    // Muted single line. Style matches Entry::Note; the leading glyph
    // distinguishes routing decisions from generic notices.
    Line::from(vec![
        Span::styled(format!("▸ {text}"), palette.muted_style()),
    ])
}
```

If `Palette` exposes its muted helper under a different name (`palette.muted` field, `Style::default().fg(palette.muted_fg)`, etc.), match whatever `Entry::Note` already uses — the goal is "same shade as a note, just with the glyph."

- [ ] **Step 5: Verify compilation + tests**

Run: `cargo check -p savvagent`
Expected: clean — every previously-missing arm now exists.

Run: `cargo test -p savvagent --no-fail-fast`
Expected: any TUI tests that snapshot the entries vector may need an additional `Entry::RouteBadge` row (or filter it out). Fix as the failures point them out.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/src/app.rs crates/savvagent/src/ui.rs
git commit -m "feat(tui): render per-turn routing badge from TurnEvent::RouteSelected"
```

---

## Task 9: TUI `/use` no longer clears `app.entries`; `/model` picker no longer filters to active provider

**Files:**
- Modify: `crates/savvagent/src/main.rs`

Two small TUI changes that match the host's new contract.

- [ ] **Step 1: `/use` keeps `app.entries`**

Edit `crates/savvagent/src/main.rs`. Locate `handle_use_command` (around line 1329). The success branch currently clears the TUI's entries:

```rust
        Ok(()) => {
            // History is already cleared on the host side; reset the
            // TUI's transcript view to match.
            app.entries.clear();
            app.live_text.clear();
            app.update_metrics();
            ...
```

Replace with:

```rust
        Ok(()) => {
            // Phase 3+: history is preserved across `/use`. The TUI
            // entries vector keeps every prior turn so the user can
            // continue the conversation on the new active provider.
            app.update_metrics();
            ...
```

(Leave the rest of the success block — the `active_provider_id` and `model` sync — unchanged.)

- [ ] **Step 2: Update the `/use` user-facing note copy**

The localized message at `notes.use-switched` (used at the end of the success branch) currently reads like a fresh-conversation announcement. Locate where the message body for `notes.use-switched` lives. Run:

Run: `grep -rn 'use-switched' crates/savvagent/locales/ crates/savvagent/src/`
Expected: a `.toml` file with `[notes] use-switched = "..."`. Update the message to something like:

```toml
use-switched = "Switched active provider to %{name}. History preserved; new turns route here by default."
```

(Translate the same change in every locale file; English is the source of truth.)

- [ ] **Step 3: `/model` picker shows every connected provider's models**

Locate `handle_model_command` / `refresh_cached_models` in `crates/savvagent/src/main.rs` (lines 862-1067 area). Currently it filters the model catalog to the active provider's capabilities by calling `host.active_provider()` and then `host.active_capabilities()`. Phase 3 wants the picker to show every connected provider's models — both because the user can now route to any of them, and because `/model anthropic/claude-opus-4-7` should set both the default model AND the default provider in one step.

**Sub-step 3a — add `Host::pool_snapshot`.** Edit `crates/savvagent-host/src/session.rs`, alongside `Host::active_capabilities` (around line 1198):

```rust
    /// Snapshot every connected provider's `(id, capabilities)`. Used by
    /// the TUI's `/model` picker to show models across the whole pool,
    /// not just the active provider's catalog.
    pub async fn pool_snapshot(&self) -> Vec<(ProviderId, ProviderCapabilities)> {
        let pool = self.pool.read().await;
        pool.iter()
            .map(|(id, entry)| (id.clone(), entry.capabilities().clone()))
            .collect()
    }
```

The return type is already public — no `lib.rs` re-export needed.

**Sub-step 3b — change `refresh_cached_models` to call `pool_snapshot`.** In `crates/savvagent/src/main.rs`, replace every site that pulls the model catalog from `host.active_capabilities()` (specifically inside `refresh_cached_models`) with a flat enumeration:

```rust
let snapshot = host.pool_snapshot().await;
let mut rows: Vec<(savvagent_protocol::ProviderId, String, String)> = Vec::new();
for (pid, caps) in snapshot {
    for model in caps.models() {
        rows.push((pid.clone(), model.id.clone(), model.display_name.clone()));
    }
}
rows.sort_by(|a, b| (a.0.as_str(), &a.1).cmp(&(b.0.as_str(), &b.1)));
app.cached_models = rows; // adjust to whatever field stores the picker rows
```

**Sub-step 3c — selection sets both the model and the active provider.** Wherever the picker's "user picked row K" handler currently calls `host.set_model(...)`, add `host.set_active_provider(&pid).await?` immediately after so the default provider follows the picked model:

```rust
host.set_model(model.clone()).await;
if let Err(e) = host.set_active_provider(&pid).await {
    app.push_note(format!("/model: {e}"));
} else {
    app.active_provider_id = matching_static_str_for(&pid);
    app.model = model;
}
```

**Sub-step 3d — picker label format.** The picker shows each row as `provider/model — display_name`. Existing user-facing localization keys for the model picker (search `locales/` for `model-picker-row` or similar) need a one-time update to accept the provider qualifier; if no such key exists, build the row label inline. The leading "provider/" qualifier is what tells the user which destination they're picking.

If `Host::pool_snapshot` doesn't exist yet, add it next to `Host::active_capabilities` (around line 1198) in `crates/savvagent-host/src/session.rs`:

```rust
    /// Snapshot every connected provider's `(id, capabilities)`. Used by
    /// the TUI's `/model` picker to show models across the whole pool,
    /// not just the active provider's catalog.
    pub async fn pool_snapshot(&self) -> Vec<(ProviderId, ProviderCapabilities)> {
        let pool = self.pool.read().await;
        pool.iter()
            .map(|(id, entry)| (id.clone(), entry.capabilities().clone()))
            .collect()
    }
```

Re-export it through `lib.rs` if needed (it returns already-public types, so nothing new to export).

- [ ] **Step 4: Verify the changes compile and tests still pass**

Run: `cargo test -p savvagent --no-fail-fast`
Expected: passes. Update any test that asserted "active-provider-only models" — flip its assertion to "every connected provider's models."

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/main.rs crates/savvagent-host/src/session.rs \
        crates/savvagent/locales/
git commit -m "feat(tui): /use preserves history; /model lists every connected provider's models"
```

---

## Task 10: Populate `ModelAlias` for built-in providers

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/provider_anthropic/mod.rs`
- Modify: `crates/savvagent/src/plugin/builtin/provider_gemini/mod.rs`
- Modify: `crates/savvagent/src/plugin/builtin/provider_openai/mod.rs`

`ProviderRegistration` already carries a `Vec<ModelAlias>` field, currently empty for every built-in. This task adds the well-known short names so users can type `@opus`, `@haiku`, `@flash`, etc.

- [ ] **Step 1: Add aliases for Anthropic**

Edit `crates/savvagent/src/plugin/builtin/provider_anthropic/mod.rs`. Locate `try_build_registration` (around line 143). Find the `ProviderRegistration::new(...)` call and follow it with `.with_aliases(...)`. Concrete aliases:

```rust
        Ok(Some(
            ProviderRegistration::new(
                id,
                display_name,
                client,
                capabilities,
            )
            .with_aliases(vec![
                ModelAlias {
                    alias: "opus".into(),
                    provider: ProviderId::new("anthropic").unwrap(),
                    model: "claude-opus-4-7".into(),
                },
                ModelAlias {
                    alias: "sonnet".into(),
                    provider: ProviderId::new("anthropic").unwrap(),
                    model: "claude-sonnet-4-6".into(),
                },
                ModelAlias {
                    alias: "haiku".into(),
                    provider: ProviderId::new("anthropic").unwrap(),
                    model: "claude-haiku-4-5".into(),
                },
            ]),
        ))
```

Ensure `ModelAlias` and `ProviderId` are imported at the top of the file:

```rust
use savvagent_host::{
    CostTier, ModelAlias, ModelCapabilities, ProviderCapabilities, ProviderRegistration,
};
use savvagent_protocol::ProviderId;
```

The exact model ids must match what `provider_anthropic`'s `capabilities()` block declares. Run `grep -n 'claude-' crates/savvagent/src/plugin/builtin/provider_anthropic/mod.rs` to confirm the model ids before pasting.

- [ ] **Step 2: Add aliases for Gemini**

Edit `crates/savvagent/src/plugin/builtin/provider_gemini/mod.rs` the same way. Aliases:

```rust
.with_aliases(vec![
    ModelAlias {
        alias: "flash".into(),
        provider: ProviderId::new("gemini").unwrap(),
        model: "gemini-2.0-flash".into(),  // confirm against capabilities()
    },
    ModelAlias {
        alias: "pro".into(),
        provider: ProviderId::new("gemini").unwrap(),
        model: "gemini-2.5-pro".into(),    // confirm against capabilities()
    },
])
```

- [ ] **Step 3: Add aliases for OpenAI**

Edit `crates/savvagent/src/plugin/builtin/provider_openai/mod.rs` the same way. Aliases:

```rust
.with_aliases(vec![
    ModelAlias {
        alias: "gpt".into(),
        provider: ProviderId::new("openai").unwrap(),
        model: "gpt-5".into(),  // confirm against capabilities()
    },
    ModelAlias {
        alias: "gpt-4o".into(),
        provider: ProviderId::new("openai").unwrap(),
        model: "gpt-4o".into(),  // identity alias for users who type the long name
    },
])
```

Local provider gets no aliases — model names are user-defined and unpredictable.

- [ ] **Step 4: Verify the builds**

Run: `cargo check -p savvagent`
Expected: clean.

- [ ] **Step 5: Verify alias lookup works through the parser**

Add (or extend) a test in `crates/savvagent-host/src/router/prefix.rs` that uses a `ModelAlias` whose model is in `ProviderCapabilities`. Already covered by `alias_form_resolves_when_unique` from Task 2 — re-run it to confirm:

Run: `cargo test -p savvagent-host router::prefix::tests::alias_form_resolves_when_unique`
Expected: passes.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/provider_anthropic/mod.rs \
        crates/savvagent/src/plugin/builtin/provider_gemini/mod.rs \
        crates/savvagent/src/plugin/builtin/provider_openai/mod.rs
git commit -m "feat(tui): populate ModelAlias for builtin providers (opus/haiku/flash/...)"
```

---

## Task 11: Cross-provider history integration test

**Files:**
- Create: `crates/savvagent-host/tests/cross_provider_history.rs`

The Phase 2 gate validated translators accept foreign-prefixed ids. This test validates the integration: a two-turn conversation where turn 1 routes to one provider and emits a `tool_use`, and turn 2 routes to a different provider. Assert that the second provider receives history containing the first provider's namespaced id.

This is an end-to-end host test that uses the Phase 2 plan's `tests/support/mod.rs` helpers (the axum fake-vendor servers). Reusing them keeps the surface small.

- [ ] **Step 1: Read what the Phase 2 support module exposes**

Run: `cat crates/savvagent-host/tests/support/mod.rs | head -100`
Expected: confirm the `FakeState`, `spawn_fake_*`, `*_success_response`, `*_body_has_foreign_id`, and inspector helpers are still present (they shipped in v0.16.0).

- [ ] **Step 2: Write the failing test**

Create `crates/savvagent-host/tests/cross_provider_history.rs`:

```rust
//! End-to-end test for Phase 3 cross-provider history.
//!
//! Set-up: a `Host` with two registered providers (Anthropic + Gemini),
//! each pointing at the same axum fake-vendor servers used by the
//! Phase 2 cross-vendor gate. Turn 1 routes to Gemini (via `@gemini`
//! prefix) and returns a synthetic `tool_use`; turn 2 routes to
//! Anthropic (via `@anthropic`) and we assert the request body Anthropic
//! receives contains a `tool_use` block whose `id` is
//! `"gemini:toolu_abc_123"` — i.e. Gemini-issued ids round-trip through
//! Anthropic's translator as opaque strings.
//!
//! The test is end-to-end against the host's actual `run_turn_streaming`
//! path: routing, prefix parsing, ID namespacing, and history transit
//! all run as they would in production.

mod support;

use savvagent_host::{
    Host, HostConfig, ProviderEndpoint, ProviderRegistration, ProviderId,
    capabilities::{CostTier, ModelCapabilities, ProviderCapabilities},
};
use std::sync::Arc;

use support::{
    FakeState, anthropic_success_response, spawn_fake_anthropic, spawn_fake_gemini,
};

// Reuses Phase 2's `spawn_fake_*` helpers + `provider_for_tests` factories
// (already shipped in v0.16.0). The only new helper added inline below is
// `gemini_success_with_tool_use`, which builds a Gemini response body
// whose first candidate has a `functionCall` part so the translator
// surfaces it as a ToolUse block.

#[tokio::test]
async fn cross_provider_history_namespaces_tool_use_id() {
    // -- Set up Gemini fake (turn 1) that returns a tool_use, then a
    //    plain text response after the tool result comes back. Phase 2's
    //    helper only emits a text-only response; extend or fork it.
    let gemini_state = FakeState::new(gemini_success_with_tool_use("toolu_abc_123", "list_dir"));
    let gemini_base = spawn_fake_gemini(&gemini_state).await;

    // -- Set up Anthropic fake (turn 2) that returns plain text.
    let anth_state = FakeState::new(anthropic_success_response());
    let anth_base = spawn_fake_anthropic(&anth_state).await;

    // -- Build a Host with both providers in the pool. Anthropic is the
    //    initial active provider; Gemini is reached by `@gemini` prefix.
    let host = build_two_provider_host(anth_base, gemini_base).await;

    // -- Turn 1: `@gemini list the cwd` -> tool_use list_dir -> tool
    //    result -> Gemini sees the result and returns plain text.
    //    (We pre-register Allow for list_dir per the streaming-test
    //    permissions feedback.)
    register_allow_for_list_dir(&host).await;

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let _outcome = host
        .run_turn_streaming("@gemini list the cwd", tx)
        .await
        .expect("turn 1 completes");
    drain_events(&mut rx).await;

    // Sanity: history now contains a Gemini-namespaced tool_use id.
    let history = host.messages().await;
    let found = history.iter().any(|m| {
        m.content.iter().any(|b| match b {
            savvagent_protocol::ContentBlock::ToolUse { id, .. } => id == "gemini:toolu_abc_123",
            _ => false,
        })
    });
    assert!(
        found,
        "history must contain `gemini:toolu_abc_123` after turn 1; got {history:#?}"
    );

    // -- Turn 2: `@anthropic what did you find?` -> Anthropic returns
    //    plain text. The assertion is what Anthropic *received*: its
    //    request body must include the foreign-prefixed id verbatim.
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let _outcome = host
        .run_turn_streaming("@anthropic what did you find?", tx)
        .await
        .expect("turn 2 completes");
    drain_events(&mut rx).await;

    let body = anth_state
        .captured_body()
        .await
        .expect("anthropic received a request in turn 2");
    assert!(
        support::anthropic_body_has_foreign_id(&body, "gemini:toolu_abc_123"),
        "anthropic must see the gemini-prefixed tool_use_id in history; body was {body:#?}"
    );
}

// ---------------------------------------------------------------------------
// Local helpers — wire-format details.
// ---------------------------------------------------------------------------

/// Gemini success response with a single `functionCall` part. Modeled on
/// `gemini_success_response` from support; differs in that the candidate's
/// content has a function-call part the host will surface as ToolUse.
fn gemini_success_with_tool_use(tool_id: &str, tool_name: &str) -> serde_json::Value {
    serde_json::json!({
        "responseId": "gem_test_tool_0",
        "modelVersion": "gemini-test",
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{
                    "functionCall": {
                        "name": tool_name,
                        "args": { "path": "." }
                    }
                }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 1,
            "totalTokenCount": 11
        }
    })
}

async fn build_two_provider_host(anth_base: String, gemini_base: String) -> Host {
    let anth_id = ProviderId::new("anthropic").unwrap();
    let gem_id = ProviderId::new("gemini").unwrap();

    let anth_caps = ProviderCapabilities::new(
        vec![ModelCapabilities {
            id: "claude-test".into(),
            display_name: "Claude Test".into(),
            supports_vision: false,
            supports_audio: false,
            context_window: 0,
            cost_tier: CostTier::Standard,
        }],
        "claude-test".into(),
    )
    .expect("valid anth caps");

    let gem_caps = ProviderCapabilities::new(
        vec![ModelCapabilities {
            id: "gemini-test".into(),
            display_name: "Gemini Test".into(),
            supports_vision: false,
            supports_audio: false,
            context_window: 0,
            cost_tier: CostTier::Standard,
        }],
        "gemini-test".into(),
    )
    .expect("valid gem caps");

    let anth_client: Arc<dyn savvagent_mcp::ProviderClient + Send + Sync> =
        Arc::new(provider_anthropic::provider_for_tests(anth_base));
    let gem_client: Arc<dyn savvagent_mcp::ProviderClient + Send + Sync> =
        Arc::new(provider_gemini::provider_for_tests(gemini_base));

    let mut cfg = HostConfig::default();
    cfg.providers = vec![
        ProviderRegistration::new(anth_id, "Anthropic".into(), anth_client, anth_caps),
        ProviderRegistration::new(gem_id, "Gemini".into(), gem_client, gem_caps),
    ];
    cfg.startup_connect = savvagent_host::StartupConnectPolicy::All;
    cfg.provider = ProviderEndpoint::StreamableHttp {
        url: "http://unused".into(),
    };
    cfg.model = "claude-test".into();

    Host::start(cfg).await.expect("host starts")
}

async fn register_allow_for_list_dir(host: &Host) {
    host.add_session_rule(
        "list_dir",
        &serde_json::json!({ "path": "." }),
        savvagent_host::PermissionDecision::Allow,
    )
    .await;
}

async fn drain_events(rx: &mut tokio::sync::mpsc::Receiver<savvagent_host::TurnEvent>) {
    while let Ok(_) = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await {
        // Drop the event; tests assert against history + captured body.
    }
}
```

- [ ] **Step 3: Wire the test's deps into the host crate's dev-dependencies if needed**

`provider-anthropic` and `provider-gemini` are already dev-deps after Phase 2's Cargo work. `provider-openai` is too, though this test doesn't need it. Run:

Run: `cargo check -p savvagent-host --tests`
Expected: clean. If a dep is missing, add it to `crates/savvagent-host/Cargo.toml`'s `[dev-dependencies]`.

- [ ] **Step 4: Decide on tool execution**

The test exercises a real `run_turn_streaming` end-to-end, which means the host runs the `list_dir` tool. Two options:

(a) **Use the actual `tool-fs` MCP server** the TUI uses, configured to a `tempfile::TempDir`. Adds binary spawn latency but covers the realistic path.

(b) **Spawn a synthetic `list_dir` tool** that returns a canned `"Cargo.toml\nsrc\n"` payload — cheaper, faster, but doesn't exercise the real tool-fs.

Recommended: (b). Add a tiny stub-tool helper in `tests/support/mod.rs` modeled on the existing `MockProvider` shapes in `provider-*` test modules. The test only needs to verify the namespacing contract, not tool-fs behavior — that's covered elsewhere.

Document whichever choice the implementer makes inline in the test header.

- [ ] **Step 5: Run the test**

Run: `cargo test -p savvagent-host --test cross_provider_history -- --nocapture`
Expected: passes. If the body assertion fails, dump `body` and trace which transformation dropped the prefix.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/tests/cross_provider_history.rs \
        crates/savvagent-host/tests/support/mod.rs  # only if you added the stub-tool helper
git commit -m "test(host): cross-provider history namespacing E2E test"
```

---

## Task 12: Version bump to 0.17.0 + CHANGELOG + README

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.package].version` + every literal in `[workspace.dependencies]`).
- Modify: `CHANGELOG.md`
- Modify: `README.md`

Per `[[feedback_semver]]`: pre-1.0, MINOR bump for new capabilities. Phase 3 lifts the "one active provider per conversation" constraint and adds the `@`-prefix syntax — a new user-visible capability, MINOR bump. Per `[[feedback_release_notes]]` and `[[feedback_release_docs]]`: every release ships with release notes + README/PRD update in the same commit.

- [ ] **Step 1: Bump the workspace version**

Run: `grep -c 'version = "0.16.0"' Cargo.toml`
Expected: at least 12 hits (workspace.package + every literal in workspace.dependencies).

Edit `Cargo.toml`. In `[workspace.package]` change `version = "0.16.0"` to `version = "0.17.0"`. Then for `[workspace.dependencies]`, use the Edit tool's `replace_all` to flip every `version = "0.16.0"` literal to `version = "0.17.0"`.

Run: `grep -c 'version = "0.17.0"' Cargo.toml && grep -c 'version = "0.16.0"' Cargo.toml`
Expected: non-zero `0.17.0` count; zero `0.16.0` count.

- [ ] **Step 2: Add the CHANGELOG entry**

Edit `CHANGELOG.md`. Insert at the top (after any header, before the `## 0.16.0` entry):

```markdown
## 0.17.0 - 2026-05-16

### Added

- **`@provider:model` (and `@provider`, `@alias`) prefix.** Users can now
  route an individual turn to a specific provider/model by prefixing
  their message with `@anthropic:claude-opus-4-7`, `@gemini`, `@opus`,
  etc. Unknown `@`-tokens are NOT consumed — the message goes through
  verbatim and the next turn routes to the active provider as usual. To
  start a message with a literal `@`, prefix with `@@`.
- **Per-turn routing badge.** Each assistant turn now renders a muted
  `▸ provider/model — Reason` line above its response so it's always
  obvious which provider handled the turn and why
  (Override / Default; modality / rules / heuristics arrive in later
  phases).
- **Built-in model aliases.** `@opus`, `@sonnet`, `@haiku` map to
  Anthropic; `@flash`, `@pro` map to Gemini; `@gpt`, `@gpt-4o` map to
  OpenAI. Ambiguous aliases (same short name across providers) fall
  through with a styled note rather than picking one silently.

### Changed

- **Cross-provider history is now safe.** `/use <provider>` no longer
  clears the conversation when switching the active provider. The host
  namespaces every `tool_use_id` with the issuing provider at insertion
  time (`<provider_id>:<original_id>`) and strips the receiver's own
  prefix back off before each request; foreign-prefixed ids flow through
  every translator as opaque strings, validated by the Phase 2
  cross-vendor gate (v0.16.0).
- **`/model` picker shows every connected provider's models.** Selecting
  a model from a different provider updates both the active provider and
  the default model in one step.

### Internal

- Phase 3 of the multi-provider-pool roadmap (see
  `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`).
  New `crates/savvagent-host/src/router/{prefix,router,namespace}.rs`
  modules; `Host::run_turn_inner` now parses the `@`-prefix, invokes
  `Router::pick`, emits `TurnEvent::RouteSelected`, and namespaces ids
  on append / strips on egress.
- `TurnEvent::RouteSelected { provider_id, model_id, reason }` added.
  Existing `TurnEvent` consumers that match the enum exhaustively need a
  new arm (the TUI's `apply_turn_event` handles it; downstream consumers
  outside this repo may need to update).
- `PoolEntry` gains an `aliases` field carrying every `ModelAlias` the
  provider's `ProviderRegistration` declared; `PoolEntry::new` takes a
  new `aliases: Vec<ModelAlias>` argument.
```

- [ ] **Step 3: Update the README**

Edit `README.md`. Find the slash-command / user-facing-features section (search for `/connect` or `/use` to land on it). Add a subsection:

```markdown
### Routing turns to a specific provider/model

Prefix any message with `@<provider>:<model>` (or `@<provider>`, or
`@<alias>`) to route that single turn to a specific destination
regardless of the active provider:

- `@anthropic:claude-opus-4-7 design this` — explicit provider + model
- `@gemini explain this` — bare provider, picks Gemini's default model
- `@opus refactor this` — alias, resolves to Anthropic's claude-opus-4-7
- `@@team look here` — literal `@team` (strips one `@`)

Unknown `@`-tokens are not consumed: the message goes through verbatim
and the next turn routes to whichever provider `/use` last selected.
Each assistant turn shows a muted `▸ provider/model — Reason` line above
its response so the routing decision is always visible.
```

- [ ] **Step 4: Build + test to confirm the bump didn't break anything**

Run: `cargo build --workspace --all-targets`
Expected: clean build with the new version literals.

Run: `cargo test --workspace --no-fail-fast`
Expected: every test passes. The cross-vendor gate from v0.16.0 still passes (no host code change touches its assumptions). The new `cross_provider_history` integration test passes. Existing TUI tests pass (with the `Entry::RouteBadge` updates from Task 8).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml CHANGELOG.md README.md
git commit -m "release(0.17.0): @provider:model override + cross-provider conversations"
```

---

## Task 13: Final verification + open PR

**Files:** none — verification + git operations only.

- [ ] **Step 1: Match CI's stable toolchain locally**

Per `[[feedback_match_ci_toolchain_locally]]`:

Run: `rustup run stable cargo fmt --all -- --check`
Expected: no output.

Run: `rustup run stable cargo clippy --workspace --all-targets -- -D warnings`
Expected: no output. Watch especially for `dead_code` on the new `Entry::RouteBadge` variant — per `[[feedback_dead_code_in_binary_crate]]`, items in the `savvagent` binary crate must be consumed by non-test code; this variant IS consumed in `ui.rs` (Task 8), so it should pass.

- [ ] **Step 2: Re-run the full workspace tests as a final integrity check**

Run: `cargo test --workspace --no-fail-fast`
Expected: every test passes; the `cross-vendor-gate` job's nine pair tests still green; the new `cross_provider_history` integration green; flipped Phase 1 history-clear test green under its new name.

- [ ] **Step 3: Push the branch + open the PR**

Use the `git-expert` agent per the global instruction `Use git-expert when working with git or GitHub.`. Branch name: `phase-3-cross-provider-routing`. The PR body should:

- Reference the spec section ("Phasing" → Phase 3).
- Summarise: `@provider:model` prefix; per-turn routing badge; cross-provider history with namespaced `tool_use_id`s; `/use` preserves history; `/model` picker shows every connected provider.
- Call out the dependency on Phase 2 (v0.16.0) being green.
- List the cross-provider history integration test as the new safety check.
- Note that `TurnEvent::RouteSelected` is a new variant — call out the user-visible match-arm work in the TUI.
- Per the global instruction: include NO Claude self-attribution. The git-expert agent already knows; the explicit reminder belongs in the instructions for that subagent.

- [ ] **Step 4: Confirm CI is green for the pushed SHA**

Per `[[feedback_verify_ci_after_push]]`:

Run: `gh pr checks`
Expected: every job green — `lint`, `test`, `cross-vendor-gate` (Phase 2's matrix, still passing), `dist-plan`.

- [ ] **Step 5: Post a status comment on the multi-provider roadmap tracking issue (if one exists)**

Per `[[feedback_keep_issue_updated]]`. If there is no tracking issue for the multi-provider roadmap, skip this step. If there is one, post:

> Phase 3 (`@provider:model` override + cross-provider conversations) merged in PR #N (v0.17.0). Remaining: Phase 4 (modality routing), Phase 5 (user rules from `routing.toml`), Phase 6 (heuristic classifier).

- [ ] **Step 6: Do NOT publish a GitHub release manually**

Per `[[feedback_cargo_dist_release]]`: cargo-dist owns the release lifecycle on tag push. Once the version-bump commit lands on master, the existing `Release` workflow picks it up automatically.

---

## Spec coverage check

Mapping each Phase 3 requirement in the spec to a task above.

| Spec requirement | Plan task |
|---|---|
| Add the `@`-prefix parser with `@@`-escape rules and unknown-token fallthrough | Task 2 |
| Add the `Router` skeleton (layered, but only Override + Default active in Phase 3) | Tasks 1, 5 |
| Add `RoutingDecision { provider_id, model_id, reason }` | Task 1 |
| Add `RoutingReason` enum (`Override` / `Default` populated; others `#[non_exhaustive]`) | Task 1 |
| Add the per-turn transcript badge | Task 8 |
| Lift Phase 1's "one active per conversation" constraint | Tasks 6, 7 |
| Host owns one canonical `Vec<Message>`; namespaces `<provider_id>:<id>` on insertion | Tasks 3, 6 |
| Receiver's translator strips own prefix on the way out, treats foreign as opaque | Task 6 (host-side strip — see Architecture note about why this lives in the host, not the translator, in Phase 3) |
| `/use <provider>` graduates: no longer clears history | Tasks 7, 9 |
| Built-in model aliases (`@opus`, `@haiku`, etc.) | Task 10 |
| Cross-provider history integration test (turn 1 on A, turn 2 on B, assert namespacing) | Task 11 |
| Phase 2 gate dependency: ships only after v0.16.0 is green | Acknowledged in plan header; verified via `cargo test --workspace` in Tasks 12-13 |
| `RoutingDecision.reason` rendered as `Override` / `Default` text in the transcript | Task 8 |
| `@@<rest>` escape stripping exactly one leading `@` | Task 2 (test `escape_double_at`) |
| Unknown `@token` not consumed (message passes through, `reason = Default`) | Task 2 (tests `unknown_token_not_consumed`, `alias_ambiguous_falls_through`) + Task 5 (router stale-override fallthrough) |
| Slash commands take precedence over `@`-overrides | **No code in this plan.** Slash commands are intercepted by the TUI's command palette long before user input reaches `Host::run_turn_inner`; `@`-parsing only ever sees non-slash input. The plan acknowledges this rather than testing it — verifying "slash beats @-prefix" would require a TUI integration test that has no failure mode in current code. |
| Cross-provider streaming of a single turn (model A drafts, B refines) | **Out of scope per spec** ("Non-goals"). The router pins one provider for the duration of a user turn including all tool-use iterations. |
| ML-based intent classifier | **Out of scope per spec** ("Non-goals"). Phase 6 ships only the opt-in keyword/length heuristic. |

### Explicit scope cuts vs. spec text

Two spec details warrant calling out as **deliberate scope cuts** rather than gaps:

1. **Host-side own-prefix stripping vs. translator-side.** The spec text in "History and tool_use ID namespacing" says: *"each provider adapter strips its own prefix on the way out."* This plan strips the own-prefix **in the host** (Task 6's `strip_own_prefix_in_history` call before each `provider.complete`) rather than in each translator, because (a) translators stay unchanged — no per-vendor surgery in three crates; (b) the Phase 2 gate already proved translators accept prefixed ids as opaque strings, so the host's strip-on-egress is purely a wire-cosmetics nicety, not a correctness requirement; (c) keeping the namespacing logic in one place makes it easier to extend (e.g. the spec's "hash-substitution fallback" if a vendor ever rejects prefixed ids) without touching every translator. If a reviewer prefers the spec's literal "in the translator" design, the equivalent refactor is a one-line addition to each `complete` implementation; this plan defers it.

2. **`/route show` and `/route reload` slash commands.** The spec mentions these as part of the broader routing UX, but they are explicitly listed under Phase 5 ("User rules from `routing.toml`"). Phase 3 ships neither — the only debugging affordance for "why did it pick that?" is the per-turn badge from Task 8.
