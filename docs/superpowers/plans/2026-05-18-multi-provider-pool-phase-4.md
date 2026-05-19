# Multi-provider pool — Phase 4 implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add modality-aware routing. When a user message contains an `Image` content block and the routing decision's model has `supports_vision = false`, the router auto-redirects the turn to a vision-capable model — preferring another model on the active provider first, then falling back to other connected providers in deterministic order. This is the "Gemini Vision for multimodal tasks" capability called out in the spec.

**Architecture:**
- **New routing layer (Layer 2 — Modality)** in `Router::pick`. It sits between Layer 1 (`@`-prefix override) and Layer 5 (default), so an explicit `@`-override always wins. If the user pinned `@o3` and attached an image, the router honors the override and the provider returns whatever error it returns; that's the user's call, not the router's.
- **`required_modalities(&[Message]) -> RequiredModalities`** in a new pure `router/modality.rs` module. It scans the most recent user message's content blocks (image attachments live on the user turn, not in history) and returns a small struct with `has_image: bool`, `has_pdf: bool`, `has_audio: bool` fields. Phase 4 only ever sets `has_image` (the protocol has no PDF or audio content blocks yet); the other two fields exist so Phase 5's `routing.toml` predicates (`has_image` / `has_pdf` / `has_audio`) can map straight onto this struct without a rename. Field names match the Phase 5 spec exactly so the user-rules layer doesn't have to retouch this code.
- **`pick_vision_capable(...)` helper** in the same module. Given the current decision (provider + model), the snapshot of connected providers, and the active provider id, it returns `Some((ProviderId, model_id))` only when a redirect can be made **within the active provider's own models**, or `None` otherwise. Selection rule: scan the active provider's `ModelCapabilities` list for the first `supports_vision = true` model and use it. **Cross-provider fallback is intentionally NOT performed automatically** — the user picked a billing relationship by choosing a provider, and a silent hop to another provider crosses that boundary. When no same-provider redirect is possible, the layer returns `None`, the router falls through to `Default`, and the warning event fires so the user knows their image-bearing turn likely won't work. Phase 5's user-rules system (`routing.toml`) is the explicit opt-in for cross-provider redirects.
- **`RoutingReason::Modality { kind }`** new variant on the `#[non_exhaustive]` enum. `kind` is a small `RequiredModalityKind::Image` enum (today's only value) so the Display impl can render `Modality(image)` per the spec. Adding the variant is additive — `match`es on `RoutingReason` already needed a wildcard arm because the enum is `#[non_exhaustive]`.
- **New `Host::run_turn_streaming_with_blocks(content, events)` entrypoint.** Phase 4 needs an API that accepts a user turn with arbitrary content blocks (text + image), not just a string. Today's `run_turn_streaming(text, events)` always wraps its string in a single `Text` block — there's no way for a future image-upload UX (or this phase's integration test) to deliver an image. The refactor: `run_turn_inner` is changed to take `Vec<ContentBlock>` instead of `String`. Existing entrypoints (`run_turn`, `run_turn_streaming`) wrap their input as `vec![ContentBlock::Text { text }]` before calling the inner method. A new public `run_turn_streaming_with_blocks` accepts blocks directly. `@`-prefix parsing still applies only to a leading `Text` block; non-text leading blocks skip parsing. The TUI keeps calling the string-form entrypoint; no UI change in this phase.
- **TUI gets a styled note when a vision-required input lands on a model that can't handle it.** Covers two cases: (a) `@`-override pinned a vision-incapable model — request goes through, provider may refuse; (b) the active provider has no vision-capable model and cross-provider fallback was declined by policy — request falls through to Default with the same warning. Implementation: a `TurnEvent::ModalityWarning { message }` variant emitted from `run_turn_inner` when `required.has_image` is set but `decision.reason` is not `Modality` AND the chosen `(provider, model)` has `supports_vision = false`. Rendered as a one-line muted entry above the assistant response.
- **No new content-block types, no PDF, no audio in the detector.** `ContentBlock::Image` already exists; `supports_vision` already exists; this phase wires them together. PDF/audio fields on `RequiredModalities` are reserved (always `false` in Phase 4) so Phase 5's `has_pdf`/`has_audio` predicates compile against the same struct. The TUI does not yet expose an image-attachment UI; Phase 4 ships the host-side machinery so a future TUI feature can land without touching routing.

**Tech Stack:** Rust 2024, Tokio, `async-trait`. No new workspace dependencies.

**Spec:** `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`. This plan covers **Phase 4 only** — the "Modality routing" entry under "Phasing", with its supporting "Modality match" routing-layer description. Phase 3 (`v0.17.0`) is already shipped; Phases 5 (user rules) and 6 (heuristic classifier) each get their own plan.

---

## File structure (Phase 4)

**New files:**
- `crates/savvagent-host/src/router/modality.rs` — pure `RequiredModalities` detection + `pick_vision_capable` helper.
- `crates/savvagent-host/tests/modality_routing.rs` — end-to-end test that runs a turn with an attached image through a host whose active provider's default model lacks vision, asserting the router redirects to a vision-capable provider/model.

**Modified files:**
- `crates/savvagent-host/src/router/mod.rs` — declare and re-export the new `modality` submodule.
- `crates/savvagent-host/src/router/router.rs` — add `RoutingReason::Modality { kind: RequiredModalityKind }` variant; extend `Router::pick` signature with a new `required: RequiredModalities` parameter so Layer 2 can evaluate. Update Display impl. Add a `caps_with_vision(model, vision)` test helper alongside the existing `caps(model)` helper.
- `crates/savvagent-host/src/lib.rs` — re-export `RequiredModalities`, `RequiredModalityKind`, `pick_vision_capable`.
- `crates/savvagent-host/src/session.rs` — `TurnEvent::ModalityWarning { message }` variant; `run_turn_inner` is refactored to take `Vec<ContentBlock>` instead of `String`; `@`-prefix parsing now operates on a leading `Text` block when present; both existing entrypoints (`run_turn`, `run_turn_streaming`) wrap their string input; new public `run_turn_streaming_with_blocks(content, events)`. `run_turn_inner` computes `required_modalities` from the messages built for this turn and threads it into `Router::pick`; emits the warning event when the layer can't redirect a vision-required turn. Currently 2927 lines; the changes are localized.
- `crates/savvagent/src/app.rs` — handle `TurnEvent::ModalityWarning` by pushing a muted `Entry::Note(...)`. No new entry variant needed; `Note` already renders muted.
- `Cargo.toml` (workspace) — bump `[workspace.package].version` to `0.18.0` and every `version = "0.17.0"` literal in `[workspace.dependencies]` to `0.18.0`.
- `CHANGELOG.md` — add `## 0.18.0 - 2026-05-18` entry.
- `README.md` — short note in the user-facing routing section explaining "an image attached to a turn auto-routes to a vision-capable model if your active one doesn't support vision".

---

## Task 1: Add `RequiredModalities` types + detection in `modality.rs` + lib.rs re-export

**Files:**
- Create: `crates/savvagent-host/src/router/modality.rs`
- Modify: `crates/savvagent-host/src/router/mod.rs`
- Modify: `crates/savvagent-host/src/lib.rs`

Pure data + detection. No async, no I/O. The detection function reads only the latest user message's content blocks because image attachments live on the inbound user turn — historical images are already in the conversation and don't change which model handles *this* turn.

**Field naming aligns with Phase 5 spec.** `RequiredModalities { has_image, has_pdf, has_audio }` lets the user-rules layer in Phase 5 (`routing.toml`'s `match = { has_image = true }` predicate) bind directly to this struct's fields. Phase 4 only ever populates `has_image`; the other two are reserved (always `false`) so adding their detection later is additive.

**Cross-provider fallback is intentionally off.** `pick_vision_capable` returns `Some` only when a redirect can be done within the active provider's models. A silent jump to another provider crosses a billing boundary the user picked when they chose their active provider, and the spec's "highest-priority connected model that does" was insufficiently scoped on that point. Phase 5's user rules are the explicit cross-provider opt-in.

- [ ] **Step 1: Write the failing tests**

Create `crates/savvagent-host/src/router/modality.rs`:

```rust
//! Detect which modalities the latest user message requires, and pick a
//! same-provider replacement model when the current pick doesn't
//! support them.
//!
//! Phase 4 only ever sets `has_image`. `has_pdf` / `has_audio` are
//! reserved on [`RequiredModalities`] so Phase 5's `routing.toml`
//! predicates (`match = { has_image = true }`, etc.) can bind directly
//! to the same struct without a rename.
//!
//! **Same-provider only.** `pick_vision_capable` never crosses
//! provider boundaries silently — the user picked a billing
//! relationship when they chose their active provider. When the active
//! provider has no vision-capable model, this function returns `None`,
//! the router falls through to Default, and the host emits a
//! `TurnEvent::ModalityWarning` so the user can see why their
//! image-bearing turn likely won't succeed. Cross-provider routing is
//! the explicit opt-in via Phase 5's user rules.

use savvagent_protocol::{ContentBlock, Message, ProviderId, Role};

use crate::capabilities::ModelCapabilities;
use crate::router::ProviderView;

/// A single required-input modality. Phase 4 only ever produces
/// `Image`; `Pdf` / `Audio` are reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RequiredModalityKind {
    /// At least one `ContentBlock::Image` is present on the latest user
    /// message.
    Image,
}

impl std::fmt::Display for RequiredModalityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequiredModalityKind::Image => f.write_str("image"),
        }
    }
}

/// Per-modality flags for the current turn. Field names align with
/// Phase 5's `routing.toml` predicates (`has_image`, `has_pdf`,
/// `has_audio`) so the user-rules layer can bind to this struct
/// directly. Phase 4 only ever sets `has_image`; the other two flags
/// are always `false`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequiredModalities {
    /// Whether the latest user message contains at least one image.
    pub has_image: bool,
    /// Reserved for Phase 5; never set by Phase 4's detector because
    /// the protocol has no PDF content block yet.
    pub has_pdf: bool,
    /// Reserved for Phase 5; never set by Phase 4's detector because
    /// the protocol has no audio content block yet.
    pub has_audio: bool,
}

impl RequiredModalities {
    /// `true` when no modality flags are set.
    pub fn is_empty(&self) -> bool {
        !self.has_image && !self.has_pdf && !self.has_audio
    }

    /// Whether the given model can satisfy every set flag. Phase 4
    /// only checks `supports_vision`; PDF/audio capability flags
    /// aren't on `ModelCapabilities` yet, so any model "satisfies"
    /// `has_pdf` / `has_audio` until Phase 5 extends both sides.
    pub fn satisfied_by(&self, model: &ModelCapabilities) -> bool {
        !self.has_image || model.supports_vision
    }

    /// Return the single kind the bitset represents, if any. Used by
    /// the router to build `RoutingReason::Modality { kind }`. Phase 4
    /// only ever returns `Some(Image)` because only `has_image` is
    /// ever set.
    pub fn primary_kind(&self) -> Option<RequiredModalityKind> {
        if self.has_image {
            Some(RequiredModalityKind::Image)
        } else {
            None
        }
    }
}

/// Scan the latest user message in `messages` for content blocks that
/// require special model capabilities. Returns
/// `RequiredModalities::default()` when no user message exists or the
/// latest user message has no modality-bearing blocks.
///
/// Only the **latest** user message matters: historical images are
/// already baked into the conversation; routing decisions are per-turn.
pub fn required_modalities(messages: &[Message]) -> RequiredModalities {
    let last_user = messages.iter().rev().find(|m| matches!(m.role, Role::User));
    let Some(msg) = last_user else {
        return RequiredModalities::default();
    };
    let mut required = RequiredModalities::default();
    for block in &msg.content {
        if matches!(block, ContentBlock::Image { .. }) {
            required.has_image = true;
        }
    }
    required
}

/// Given the current routing pick (`provider_id` + `model_id`), the
/// set of connected providers, and the active provider, return
/// `Some((provider, model))` only when a vision-capable redirect is
/// available **on the same provider**. Cross-provider redirects are
/// intentionally not performed here; the active provider was the
/// user's billing choice and silently jumping to another vendor on
/// the back of an image attachment is not consent.
///
/// Selection rule: when `required.has_image` is set and the current
/// `(provider_id, model_id)` lacks vision, scan the current
/// provider's `ModelCapabilities` list for the first
/// `supports_vision = true` entry and use it. Returns `None` when
/// (a) `required.has_image` is `false`, or (b) the current model
/// already supports vision, or (c) the current provider has no
/// vision-capable model at all.
pub fn pick_vision_capable<'a>(
    required: RequiredModalities,
    provider_id: &ProviderId,
    model_id: &str,
    providers: &'a [ProviderView<'a>],
) -> Option<(ProviderId, String)> {
    if !required.has_image {
        return None;
    }

    let view = providers.iter().find(|p| p.id == provider_id)?;

    // Does the current pick already satisfy?
    if let Some(m) = view.capabilities.model(model_id)
        && m.supports_vision
    {
        return None;
    }

    // Same-provider sibling: first vision-capable model in capability
    // list order. Cross-provider fallback is intentionally NOT done.
    let m = view
        .capabilities
        .models()
        .iter()
        .find(|m| m.supports_vision)?;
    Some((provider_id.clone(), m.id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};

    /// Build a provider with N models, each tagged with its
    /// `supports_vision` value. `default` must appear in the list.
    fn caps_with_vision(models: Vec<(&str, bool)>, default: &str) -> ProviderCapabilities {
        let models = models
            .into_iter()
            .map(|(id, vision)| ModelCapabilities {
                id: id.into(),
                display_name: id.into(),
                supports_vision: vision,
                supports_audio: false,
                context_window: 0,
                cost_tier: CostTier::Standard,
            })
            .collect();
        ProviderCapabilities::new(models, default.into()).expect("valid caps")
    }

    #[test]
    fn required_modalities_empty_when_no_image() {
        let msgs = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "hi".into() }],
        }];
        assert!(required_modalities(&msgs).is_empty());
    }

    #[test]
    fn required_modalities_detects_image() {
        let msgs = vec![Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "what is this?".into(),
                },
                ContentBlock::Image {
                    source: savvagent_protocol::ImageSource::Base64 {
                        media_type: savvagent_protocol::MediaType::Png,
                        data: "AAAA".into(),
                    },
                },
            ],
        }];
        let r = required_modalities(&msgs);
        assert!(r.has_image);
        assert!(!r.has_pdf);
        assert!(!r.has_audio);
        assert_eq!(r.primary_kind(), Some(RequiredModalityKind::Image));
    }

    #[test]
    fn required_modalities_only_inspects_latest_user_message() {
        // Old user message with image — should NOT trip the bit.
        // Latest user message has no image.
        let msgs = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Image {
                    source: savvagent_protocol::ImageSource::Base64 {
                        media_type: savvagent_protocol::MediaType::Png,
                        data: "AAAA".into(),
                    },
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "ok".into() }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "now what?".into(),
                }],
            },
        ];
        assert!(required_modalities(&msgs).is_empty());
    }

    #[test]
    fn pick_returns_none_when_no_image_required() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = caps_with_vision(vec![("m", false)], "m");
        let views = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];
        let r = pick_vision_capable(RequiredModalities::default(), &a_id, "m", &views);
        assert!(r.is_none());
    }

    #[test]
    fn pick_returns_none_when_current_model_already_supports_vision() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = caps_with_vision(vec![("opus", true)], "opus");
        let views = vec![ProviderView {
            id: &a_id,
            capabilities: &a_caps,
        }];
        let r = pick_vision_capable(
            RequiredModalities {
                has_image: true,
                ..Default::default()
            },
            &a_id,
            "opus",
            &views,
        );
        assert!(r.is_none());
    }

    #[test]
    fn pick_returns_same_provider_sibling_model() {
        // Active = anthropic default haiku (no vision), but anthropic also
        // has opus (vision). Pick should stay on anthropic, switch to opus.
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps_with_vision(vec![("haiku", false), ("opus", true)], "haiku");
        let g_caps = caps_with_vision(vec![("flash", true)], "flash");
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
        let r = pick_vision_capable(
            RequiredModalities {
                has_image: true,
                ..Default::default()
            },
            &a_id,
            "haiku",
            &views,
        );
        assert_eq!(r, Some((a_id, "opus".to_string())));
    }

    #[test]
    fn pick_returns_none_when_active_provider_has_no_vision_model() {
        // Active = anthropic, has no vision-capable model. Another
        // connected provider (gemini) DOES have one, but Phase 4's
        // same-provider-only policy forbids the silent cross-provider
        // jump. Return None; the router falls through to Default.
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps_with_vision(vec![("o3", false)], "o3");
        let g_caps = caps_with_vision(vec![("flash", true)], "flash");
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
        let r = pick_vision_capable(
            RequiredModalities {
                has_image: true,
                ..Default::default()
            },
            &a_id,
            "o3",
            &views,
        );
        assert!(r.is_none(), "no silent cross-provider redirect");
    }

    #[test]
    fn pick_returns_none_when_active_provider_unknown() {
        // Defensive: a stale provider_id that isn't in the pool.
        // Returns None so the router falls through.
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let g_caps = caps_with_vision(vec![("flash", true)], "flash");
        let views = vec![ProviderView {
            id: &g_id,
            capabilities: &g_caps,
        }];
        let r = pick_vision_capable(
            RequiredModalities {
                has_image: true,
                ..Default::default()
            },
            &a_id,
            "o3",
            &views,
        );
        assert!(r.is_none());
    }
}
```

- [ ] **Step 2: Wire the new module into `router/mod.rs`**

Edit `crates/savvagent-host/src/router/mod.rs`. Add `pub mod modality;` next to the other submodule declarations, and append to the `pub use` block:

```rust
pub use modality::{
    RequiredModalities, RequiredModalityKind, pick_vision_capable, required_modalities,
};
```

- [ ] **Step 3: Re-export from `lib.rs`**

Edit `crates/savvagent-host/src/lib.rs`. Find the existing `pub use router::{...};` block and replace with:

```rust
pub use router::{
    LegacyModelResolution, ProviderView, RequiredModalities, RequiredModalityKind, Router,
    RoutingDecision, RoutingOverride, RoutingReason, pick_vision_capable, required_modalities,
    resolve_legacy_model,
};
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p savvagent-host router::modality::tests`
Expected: 7 tests pass (3 detection + 4 pick).

If any fail, the most likely issues are:
- `ProviderId::as_str()` doesn't exist (it does — used in `router/legacy_model.rs`; double-check the exact method name).
- `ProviderCapabilities::model(id)` returns `Option<&ModelCapabilities>` — confirm via `crates/savvagent-host/src/capabilities.rs:132`.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/router/modality.rs \
        crates/savvagent-host/src/router/mod.rs \
        crates/savvagent-host/src/lib.rs
git commit -m "feat(host): RequiredModalities detection + same-provider vision picker (Phase 4)"
```

---

## Task 2: Add `RoutingReason::Modality` variant + Display

**Files:**
- Modify: `crates/savvagent-host/src/router/router.rs`

The `RoutingReason` enum is `#[non_exhaustive]`, so adding a variant is additive. Display needs to render `Modality(image)` per the spec's transcript-badge format.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module at the bottom of `crates/savvagent-host/src/router/router.rs`:

```rust
#[test]
fn routing_reason_modality_displays() {
    use crate::router::modality::RequiredModalityKind;
    let r = RoutingReason::Modality {
        kind: RequiredModalityKind::Image,
    };
    assert_eq!(format!("{r}"), "Modality(image)");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p savvagent-host router::router::tests::routing_reason_modality_displays`
Expected: FAIL — `RoutingReason::Modality` doesn't exist.

- [ ] **Step 3: Add the variant**

Edit `crates/savvagent-host/src/router/router.rs`. Replace the `RoutingReason` enum and its Display impl:

```rust
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
        kind: crate::router::modality::RequiredModalityKind,
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
            RoutingReason::Default => f.write_str("Default"),
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p savvagent-host router::router::tests::routing_reason_modality_displays`
Expected: PASS.

Also rerun the existing reason tests to confirm no regressions:

Run: `cargo test -p savvagent-host router::router::tests::routing_reason_displays`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/router/router.rs
git commit -m "feat(host): RoutingReason::Modality variant + Display"
```

---

## Task 3: Extend `Router::pick` to evaluate the Modality layer

**Files:**
- Modify: `crates/savvagent-host/src/router/router.rs`

Layer order is fixed by the spec: Override (Layer 1) wins absolutely. If the user pinned `@o3` and attached an image, the router does NOT redirect — the user opted in to that provider's limits. Modality (Layer 2) runs only when no override applied. Default (Layer 5) is the fallthrough.

The signature change is additive (new parameter), but every existing caller will need to pass the new arg. Phase 4 has exactly one production caller (`run_turn_inner` in `session.rs`); Task 4 introduces a placeholder `RequiredModalities::default()` during the refactor, and Task 5 replaces it with the real value plumbed in from `required_modalities(&messages)`.

- [ ] **Step 1: Add a `caps_with_vision` helper to the existing tests module**

Edit `crates/savvagent-host/src/router/router.rs`. The existing `tests` module has a `caps(model: &str) -> ProviderCapabilities` helper that hard-codes `supports_vision: false`. Add a sibling helper next to it that takes a vision flag:

```rust
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
```

This avoids the `let g_caps = ...; let g_caps = ...;` shadowing pattern in the new tests.

- [ ] **Step 2: Write the failing tests**

Append to the `tests` module at the bottom of `crates/savvagent-host/src/router/router.rs` (under the existing pick tests):

```rust
#[test]
fn pick_modality_redirects_to_same_provider_sibling_model() {
    use crate::router::modality::{RequiredModalities, RequiredModalityKind};
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
    use crate::router::modality::RequiredModalities;
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
    );
    assert_eq!(r.provider_id, o_id);
    assert_eq!(r.model_id, "o3");
    assert_eq!(r.reason, RoutingReason::Override);
}

#[test]
fn pick_modality_no_op_when_default_already_supports_vision() {
    use crate::router::modality::RequiredModalities;
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
    );
    assert_eq!(r.provider_id, a_id);
    assert_eq!(r.model_id, "opus");
    assert_eq!(r.reason, RoutingReason::Default);
}

#[test]
fn pick_modality_does_not_silently_cross_provider() {
    // Active = anthropic with no vision-capable model; gemini connected
    // with a vision model. Phase 4's same-provider-only policy refuses
    // the silent cross-provider jump — falls through to Default. The
    // host emits a ModalityWarning so the user sees why.
    use crate::router::modality::RequiredModalities;
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
    );
    assert_eq!(r.provider_id, a_id);
    assert_eq!(r.model_id, "haiku");
    assert_eq!(r.reason, RoutingReason::Default);
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p savvagent-host router::router::tests::pick_modality`
Expected: FAIL — `Router::pick` doesn't accept a `RequiredModalities` arg yet.

- [ ] **Step 4: Update `Router::pick` to take and apply the modality layer**

Edit `crates/savvagent-host/src/router/router.rs`. Replace the `Router::pick` impl:

```rust
impl Router {
    /// Pick a `(provider, model, reason)` triple for a turn.
    ///
    /// Phase 4 active layers:
    /// - **Override** — if `override_` is `Some` and resolves to a
    ///   connected provider, use it. The model is the override's model
    ///   if specified, else the provider's default model. **An override
    ///   always wins, even if the user attached an image and the chosen
    ///   model lacks vision** — that's the user's explicit call.
    /// - **Modality** — if no override, and `required` is non-empty, and
    ///   the (active_provider, active_model) pair lacks the required
    ///   modality, redirect to a sibling model **on the same provider**.
    ///   Cross-provider redirects are not done silently — when the active
    ///   provider has no vision-capable model, this layer falls through
    ///   to Default and the host emits `TurnEvent::ModalityWarning` so
    ///   the user sees why their image-bearing turn likely won't succeed.
    /// - **Default** — otherwise, use `active_provider` + `active_model`.
    ///
    /// A stale override that points at a now-disconnected provider falls
    /// through to Modality / Default (defensive — `parse_at_prefix`
    /// already filters these, but defending against a TOCTOU window
    /// between parse and pick is cheap).
    pub fn pick(
        override_: Option<RoutingOverride>,
        providers: &[crate::router::ProviderView<'_>],
        active_provider: &ProviderId,
        active_model: &str,
        required: crate::router::modality::RequiredModalities,
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

        // Modality layer.
        if !required.is_empty() {
            if let Some((p, m)) = crate::router::modality::pick_vision_capable(
                required,
                active_provider,
                active_model,
                providers,
            ) {
                let kind = required
                    .primary_kind()
                    .expect("required.is_empty() == false implies primary_kind() = Some");
                return RoutingDecision {
                    provider_id: p,
                    model_id: m,
                    reason: RoutingReason::Modality { kind },
                };
            }
        }

        RoutingDecision {
            provider_id: active_provider.clone(),
            model_id: active_model.to_string(),
            reason: RoutingReason::Default,
        }
    }
}
```

- [ ] **Step 5: Update existing tests to pass the new arg**

Every existing call to `Router::pick` in `router.rs`'s tests module needs `RequiredModalities::default()` appended. Run:

Run: `grep -n 'Router::pick(' crates/savvagent-host/src/router/router.rs`
Expected: 5+ hits (4 pre-existing test calls + the impl + the new test calls).

For each of the 4 pre-existing test calls (`pick_default_when_no_override`, `pick_override_with_model`, `pick_override_without_model_uses_provider_default`, `pick_override_for_disconnected_provider_falls_through`), append `, RequiredModalities::default()` as the fifth argument. Use the Edit tool individually per call; `replace_all` won't work because each call has different earlier args.

Add a `use` at the top of the existing `tests` module:

```rust
use crate::router::modality::RequiredModalities;
```

- [ ] **Step 6: Run all router tests**

Run: `cargo test -p savvagent-host router::router::tests`
Expected: all tests pass (existing 4 + 4 new = 8 pick tests, plus the two display tests).

- [ ] **Step 7: Commit**

```bash
git add crates/savvagent-host/src/router/router.rs
git commit -m "feat(host): Router::pick modality layer (Phase 4 Layer 2)"
```

---

## Task 4: Refactor `run_turn_inner` to accept `Vec<ContentBlock>`

**Files:**
- Modify: `crates/savvagent-host/src/session.rs`

`run_turn_inner` currently takes a `String` and wraps it in a single `ContentBlock::Text` before pushing to messages. Phase 4 needs the inner method to accept arbitrary content blocks so the modality layer can detect image inputs. Existing string-based entrypoints (`run_turn`, `run_turn_streaming`) keep their public signatures but wrap their input in a one-block `Vec<ContentBlock>` before calling the inner method.

The `@`-prefix parser currently runs on the user-input string. After this refactor, it runs on the leading `Text` block's text (if any). Non-text leading blocks (an image-first message) skip `@`-parsing entirely.

- [ ] **Step 1: Locate the inner method and its callers**

Run: `grep -n 'fn run_turn_inner\|run_turn_inner(' crates/savvagent-host/src/session.rs`
Expected: 3 hits — definition + 2 callers (`run_turn`, `run_turn_streaming`).

Read the current definition (around line 559) and both call sites (around lines 519 and 530).

- [ ] **Step 2: Change `run_turn_inner` to take blocks**

Edit `crates/savvagent-host/src/session.rs`. Replace the signature:

```rust
async fn run_turn_inner(
    &self,
    user_input: String,
    events: Option<mpsc::Sender<TurnEvent>>,
) -> Result<TurnOutcome, HostError> {
```

with:

```rust
async fn run_turn_inner(
    &self,
    user_content: Vec<savvagent_protocol::ContentBlock>,
    events: Option<mpsc::Sender<TurnEvent>>,
) -> Result<TurnOutcome, HostError> {
```

Then inside the body, replace the `@`-prefix parse + message-push block. The current code is:

```rust
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
};

messages.push(Message {
    role: Role::User,
    content: vec![ContentBlock::Text { text: parsed.body }],
});
```

Replace with:

```rust
// Phase 4: if the leading block is text, run the @-prefix parser on
// it and replace it with the stripped body. Non-text leading blocks
// (e.g. image-first turns) skip @-parsing.
let (override_, user_content) = {
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

    let mut blocks = user_content;
    let mut override_ = None;
    if let Some(ContentBlock::Text { text }) = blocks.first() {
        let parsed = crate::router::prefix::parse_at_prefix(text, &views, &aliases);
        override_ = parsed.override_;
        if let Some(ContentBlock::Text { text }) = blocks.first_mut() {
            *text = parsed.body;
        }
    }
    (override_, blocks)
    // pool guard dropped at end of this block
};

messages.push(Message {
    role: Role::User,
    content: user_content,
});
```

Then in the existing `Router::pick` call site, replace `parsed.override_` with `override_` (the local variable).

- [ ] **Step 3: Update the two string-form entrypoints**

Edit `crates/savvagent-host/src/session.rs` around lines 519 and 530. Replace:

```rust
pub async fn run_turn(&self, user_input: impl Into<String>) -> Result<TurnOutcome, HostError> {
    self.run_turn_inner(user_input.into(), None).await
}
```

with:

```rust
pub async fn run_turn(&self, user_input: impl Into<String>) -> Result<TurnOutcome, HostError> {
    let text = user_input.into();
    self.run_turn_inner(
        vec![savvagent_protocol::ContentBlock::Text { text }],
        None,
    )
    .await
}
```

And the streaming entrypoint:

```rust
pub async fn run_turn_streaming(
    &self,
    user_input: impl Into<String>,
    events: mpsc::Sender<TurnEvent>,
) -> Result<TurnOutcome, HostError> {
    let text = user_input.into();
    self.run_turn_inner(
        vec![savvagent_protocol::ContentBlock::Text { text }],
        Some(events),
    )
    .await
}
```

- [ ] **Step 4: Confirm host crate compiles**

Run: `cargo check -p savvagent-host`
Expected: clean.

If `Router::pick` complains about the missing `required` arg, that's expected — Task 5 wires it. For now, temporarily pass `crate::router::RequiredModalities::default()` so the crate compiles between tasks; Task 5 replaces that with the real value.

- [ ] **Step 5: Run all host-crate tests**

Run: `cargo test -p savvagent-host --lib`
Expected: clean. Phase 1/2/3 tests still cover the same behavior; the new block path is exercised in Task 8.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/session.rs
git commit -m "refactor(host): run_turn_inner takes Vec<ContentBlock>; string entrypoints wrap"
```

---

## Task 5: Wire `required_modalities` into `run_turn_inner` + emit `ModalityWarning`

**Files:**
- Modify: `crates/savvagent-host/src/session.rs`

The host computes `required_modalities` from the just-built `messages` Vec (which now contains a user message that may include image blocks), then threads it into `Router::pick`. Also adds the `TurnEvent::ModalityWarning { message }` variant and emits it when a vision-required input lands on a non-vision-capable provider+model.

- [ ] **Step 1: Add the `TurnEvent::ModalityWarning` variant**

Find the `TurnEvent` enum in `crates/savvagent-host/src/session.rs` (around line 155-250; search for `pub enum TurnEvent`). Add a variant just below `RouteSelected`:

```rust
/// The router could not redirect to a vision-capable model for an
/// image-bearing turn (no connected provider has vision, OR an
/// `@`-override pinned a vision-incapable model). Emitted at most
/// once per turn, right after [`TurnEvent::RouteSelected`].
ModalityWarning {
    /// User-facing message describing why the routing decision may
    /// not satisfy the input.
    message: String,
},
```

- [ ] **Step 2: Compute `required` and thread it into `Router::pick`**

In `run_turn_inner`, just after the user message is pushed onto `messages` (after the Task 4 block), insert:

```rust
// Phase 4: detect modality requirements on the just-built `messages`.
// Reading the latest user message is sufficient — historical images
// are already baked into the conversation; routing is per-turn.
let required = crate::router::required_modalities(&messages);
```

Then update the existing `Router::pick` call (around line 619) to pass `required`. The previous task left a placeholder `RequiredModalities::default()`; replace it with the local `required`:

```rust
crate::router::Router::pick(override_, &views, &active_id, &active_model, required)
```

- [ ] **Step 3: Emit `ModalityWarning` when applicable**

Immediately after the `if let Some(tx) = &events { ... TurnEvent::RouteSelected ... }` block (around line 623-631), add:

```rust
// If an image was required but the router didn't redirect (because no
// connected model supports vision, OR because an override pinned a
// model that lacks vision), surface a styled note so the user can
// see why the next call may fail.
if required.has_image
    && !matches!(decision.reason, crate::router::RoutingReason::Modality { .. })
{
    let lacks_vision = {
        let pool = self.pool.read().await;
        pool.get(&decision.provider_id)
            .and_then(|e| e.capabilities().model(&decision.model_id))
            .map(|m| !m.supports_vision)
            .unwrap_or(false)
    };
    if lacks_vision
        && let Some(tx) = &events
    {
        let message = format!(
            "{}/{} doesn't support image input; the request may fail. \
             Connect a vision-capable model or use @<provider:model> \
             to override.",
            decision.provider_id.as_str(),
            decision.model_id
        );
        let _ = tx.send(TurnEvent::ModalityWarning { message }).await;
    }
}
```

The lock guard does not span an `.await` (the `tx.send` is outside the inner block). Same pattern the rest of the function uses.

- [ ] **Step 4: Update exhaustive matches on `TurnEvent`**

Within the same crate, exhaustive matches on the `#[non_exhaustive]` enum must still cover every variant. Find them:

Run: `grep -rn 'match .*TurnEvent\b\|TurnEvent::' crates/savvagent-host --include='*.rs' | grep -v "^.*//"`

For any exhaustive match without a wildcard arm, add:

```rust
TurnEvent::ModalityWarning { .. } => {}
```

The TUI's `apply_turn_event` in `crates/savvagent/src/app.rs` is the consumer that needs a meaningful arm; that's Task 6.

- [ ] **Step 5: Run all host tests**

Run: `cargo test -p savvagent-host`
Expected: clean. The router unit tests already pass (Task 3); the new wiring is integration-only — full E2E coverage lands in Task 8.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/session.rs
git commit -m "feat(host): wire modality layer + emit ModalityWarning event"
```

---

## Task 6: TUI renders the modality warning as a muted note

**Files:**
- Modify: `crates/savvagent/src/app.rs`

The existing `Entry::Note(String)` variant already renders as a muted line in `ui.rs`. No new entry type needed — just route `TurnEvent::ModalityWarning { message }` into a `Note`.

- [ ] **Step 1: Find the `apply_turn_event` site**

Run: `grep -n 'apply_turn_event\|TurnEvent::RouteSelected' crates/savvagent/src/app.rs`
Expected: definition + RouteSelected arm.

- [ ] **Step 2: Handle the new variant**

Edit `crates/savvagent/src/app.rs`. Inside `apply_turn_event`, add an arm right after the `RouteSelected` arm:

```rust
TurnEvent::ModalityWarning { message } => {
    self.flush_live_text();
    self.entries.push(Entry::Note(message));
}
```

If you added the temporary `_ => {}` arm in Task 5, remove it here and rely on the explicit arm.

- [ ] **Step 3: Run the TUI crate's tests**

Run: `cargo test -p savvagent`
Expected: clean (no test exercises ModalityWarning yet; Task 8 adds the E2E tests).

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent/src/app.rs
git commit -m "feat(tui): render modality warning as muted note"
```

---

## Task 7: Add public `Host::run_turn_streaming_with_blocks` entrypoint

**Files:**
- Modify: `crates/savvagent-host/src/session.rs`

The string-form entrypoints `run_turn` and `run_turn_streaming` already wrap their input in a single `Text` block (Task 4). Phase 4 needs a public entrypoint that accepts arbitrary blocks so callers (today: the integration test; future: an image-upload TUI) can submit image-bearing turns.

- [ ] **Step 1: Write the new public method**

Edit `crates/savvagent-host/src/session.rs`. Immediately after `run_turn_streaming` (around line 532), add:

```rust
/// Submit a user turn composed of arbitrary [`ContentBlock`]s — text,
/// images, or any future modality. Streams [`TurnEvent`]s onto `events`
/// the same way [`Self::run_turn_streaming`] does.
///
/// The `@`-prefix parser runs only on the **leading** block if it is
/// `Text`; non-text leading blocks (e.g. an image-first turn) skip
/// `@`-parsing entirely.
///
/// Phase 4 ships this entrypoint as host-side machinery. The TUI does
/// not yet expose an image-attachment UI; this method is intended for
/// (a) the modality routing integration test, and (b) a future image
/// upload feature in the TUI.
pub async fn run_turn_streaming_with_blocks(
    &self,
    content: Vec<savvagent_protocol::ContentBlock>,
    events: mpsc::Sender<TurnEvent>,
) -> Result<TurnOutcome, HostError> {
    self.run_turn_inner(content, Some(events)).await
}
```

- [ ] **Step 2: Verify the host crate still builds**

Run: `cargo check -p savvagent-host`
Expected: clean.

- [ ] **Step 3: Add a quick smoke unit test**

Add inside the existing `#[cfg(test)] mod tests` at the bottom of `session.rs`, alongside the other host-level tests. (Find an existing test like `set_active_provider_preserves_history` to anchor the location.) The test uses whatever mock-provider helper already exists in this module; if there's a helper like `make_test_host`, reuse it:

```rust
#[tokio::test]
async fn run_turn_streaming_with_blocks_pushes_user_message_verbatim() {
    use savvagent_protocol::{ContentBlock, ImageSource, MediaType, Role};
    // The existing `make_test_host`-style helper in this module (search
    // for `async fn make_test_host` or similar) returns a Host wired up
    // with one mock provider that returns an EndTurn. If no such helper
    // exists, mirror the pattern in `pool_lifecycle.rs`.
    let host = make_test_host().await;

    let (tx, mut _rx) = tokio::sync::mpsc::channel(8);
    let blocks = vec![
        ContentBlock::Text {
            text: "what is this?".into(),
        },
        ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: MediaType::Png,
                data: "AAAA".into(),
            },
        },
    ];
    host.run_turn_streaming_with_blocks(blocks.clone(), tx)
        .await
        .expect("turn runs");

    // The just-submitted user message should be the second-to-last
    // message (last is the mock's assistant response). Its content
    // must be the blocks we passed in (no transformation, no extra
    // text-only wrapping). `Host::messages()` is defined at
    // session.rs:974 — it returns a clone of the current `Vec<Message>`.
    let messages = host.messages().await;
    let user_msg = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::User))
        .expect("at least one user message");
    assert_eq!(user_msg.content, blocks);
}
```

- [ ] **Step 4: Run the new test**

Run: `cargo test -p savvagent-host run_turn_streaming_with_blocks_pushes_user_message_verbatim`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/session.rs
git commit -m "feat(host): public run_turn_streaming_with_blocks entrypoint for image-bearing turns"
```

---

## Task 8: End-to-end modality routing tests

**Files:**
- Create: `crates/savvagent-host/tests/modality_routing.rs`

Four `#[tokio::test]`s exercise the Phase 4 behavior end-to-end. The mock provider pattern mirrors `crates/savvagent-host/tests/pool_lifecycle.rs` exactly so this test inherits the (already-proven) host startup wiring.

- [ ] **Step 1: Re-read the test scaffold pattern**

Run: `cat crates/savvagent-host/tests/pool_lifecycle.rs`

Things to note:
- `HostConfig::new(ProviderEndpoint::StreamableHttp { url: "http://unused".into() }, "m")` is the constructor. There is no `HostConfig::default()`.
- `cfg.startup_connect = StartupConnectPolicy::All;` is what actually connects the registered providers at startup.
- `ProviderRegistration::new(id, display_name, client, caps)` is used instead of a struct literal.
- `CompleteResponse` requires `id`, `model`, `content`, `stop_reason`, `stop_sequence`, `usage` — see the `EchoClient` in `pool_lifecycle.rs`.
- `ListModelsResponse` has a `default_model_id: Option<String>`; an empty stub uses `models: vec![], default_model_id: None`.
- The active provider after `Host::start` is the first registered one. There is no need to call `set_active_provider` if you put the right one first in `cfg.providers`.

- [ ] **Step 2: Write the test file**

Create `crates/savvagent-host/tests/modality_routing.rs`:

```rust
//! Phase 4 end-to-end:
//!
//! 1. Image attachment with active provider that has a vision-capable
//!    sibling model → router redirects within the same provider with
//!    `RoutingReason::Modality { kind: Image }`. No cross-provider
//!    hop.
//! 2. Image attachment with active provider that has no vision-capable
//!    model AND no other connected providers → falls through to
//!    Default and `TurnEvent::ModalityWarning` fires.
//! 3. Image attachment with active provider that has no vision-capable
//!    model BUT another connected provider does → still falls through
//!    to Default (same-provider-only policy refuses the silent
//!    cross-provider hop) and `TurnEvent::ModalityWarning` fires.
//! 4. `@`-override pinned at a vision-incapable model + image →
//!    `RoutingReason::Override` wins; `TurnEvent::ModalityWarning`
//!    still fires so the user sees why the next call may fail.

use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
use savvagent_host::{
    Host, HostConfig, ProviderEndpoint, ProviderRegistration, RoutingReason, StartupConnectPolicy,
    TurnEvent,
};
use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ContentBlock, ImageSource, ListModelsResponse, MediaType,
    ProviderError, ProviderId, StopReason, StreamEvent,
};
use tokio::sync::{Mutex, mpsc};

/// A minimal provider that records which model it was asked to handle
/// and returns a canned `end_turn` response. One `RecordingProvider`
/// per registered `ProviderRegistration` lets each test inspect what
/// the host dispatched.
struct RecordingProvider {
    seen_model: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl ProviderClient for RecordingProvider {
    async fn complete(
        &self,
        req: CompleteRequest,
        _stream: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        *self.seen_model.lock().await = Some(req.model.clone());
        Ok(CompleteResponse {
            id: "rec-0".into(),
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

/// Build a provider with one model that has the given `supports_vision`
/// value. The model id is used as the default model id.
fn caps_one(model: &str, vision: bool) -> ProviderCapabilities {
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

/// Build an anthropic-like provider with haiku (no vision) as default
/// and opus (vision) as a sibling.
fn caps_haiku_plus_opus() -> ProviderCapabilities {
    ProviderCapabilities::new(
        vec![
            ModelCapabilities {
                id: "haiku".into(),
                display_name: "Claude Haiku".into(),
                supports_vision: false,
                supports_audio: false,
                context_window: 0,
                cost_tier: CostTier::Cheap,
            },
            ModelCapabilities {
                id: "opus".into(),
                display_name: "Claude Opus".into(),
                supports_vision: true,
                supports_audio: false,
                context_window: 0,
                cost_tier: CostTier::Premium,
            },
        ],
        "haiku".into(),
    )
    .expect("valid caps")
}

fn image_blocks(prompt: &str) -> Vec<ContentBlock> {
    vec![
        ContentBlock::Text {
            text: prompt.into(),
        },
        ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: MediaType::Png,
                data: "AAAA".into(),
            },
        },
    ]
}

fn reg(
    id: &str,
    display: &str,
    caps: ProviderCapabilities,
    seen: Arc<Mutex<Option<String>>>,
) -> ProviderRegistration {
    ProviderRegistration::new(
        ProviderId::new(id).unwrap(),
        display,
        Arc::new(RecordingProvider { seen_model: seen })
            as Arc<dyn ProviderClient + Send + Sync>,
        caps,
    )
}

async fn collect_events(
    rx: &mut mpsc::Receiver<TurnEvent>,
    timeout_ms: u64,
) -> Vec<TurnEvent> {
    let mut out = Vec::new();
    while let Ok(Some(ev)) =
        tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx.recv()).await
    {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn image_input_redirects_to_same_provider_sibling_model() {
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        "Anthropic",
        caps_haiku_plus_opus(),
        Arc::clone(&a_seen),
    );

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "haiku",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;

    let host = Host::start(cfg).await.expect("host starts");
    assert_eq!(host.active_provider().await.as_str(), "anthropic");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _ = host
        .run_turn_streaming_with_blocks(image_blocks("describe this"), tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 50).await;
    let routed = events
        .iter()
        .find_map(|ev| match ev {
            TurnEvent::RouteSelected {
                provider_id,
                model_id,
                reason,
            } => Some((provider_id.clone(), model_id.clone(), reason.clone())),
            _ => None,
        })
        .expect("RouteSelected event");
    assert_eq!(routed.0.as_str(), "anthropic");
    assert_eq!(routed.1, "opus");
    assert!(matches!(routed.2, RoutingReason::Modality { .. }));

    // Mock saw "opus" — the redirect happened on the provider side.
    assert_eq!(*a_seen.lock().await, Some("opus".into()));

    // No ModalityWarning when redirect succeeds.
    let warning = events
        .iter()
        .find(|ev| matches!(ev, TurnEvent::ModalityWarning { .. }));
    assert!(warning.is_none(), "no warning when redirect succeeds");

    host.shutdown().await;
}

#[tokio::test]
async fn image_input_warns_when_active_provider_has_no_vision_and_no_others() {
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        "Anthropic",
        caps_one("haiku", false),
        Arc::clone(&a_seen),
    );

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "haiku",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;

    let host = Host::start(cfg).await.expect("host starts");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _ = host
        .run_turn_streaming_with_blocks(image_blocks("describe this"), tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 50).await;
    let routed = events
        .iter()
        .find_map(|ev| match ev {
            TurnEvent::RouteSelected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .expect("RouteSelected event");
    assert_eq!(routed, RoutingReason::Default);

    let warning = events
        .iter()
        .find_map(|ev| match ev {
            TurnEvent::ModalityWarning { message } => Some(message.clone()),
            _ => None,
        })
        .expect("ModalityWarning event");
    assert!(
        warning.contains("image") || warning.contains("vision"),
        "warning text should reference the modality; got: {warning}"
    );

    host.shutdown().await;
}

#[tokio::test]
async fn image_input_does_not_silently_cross_to_other_provider() {
    // Anthropic active, no vision. Gemini also connected with a vision
    // model. Same-provider-only policy refuses the silent hop —
    // routing falls through to Default with a warning. Phase 5's user
    // rules are the explicit cross-provider opt-in.
    let a_seen = Arc::new(Mutex::new(None));
    let g_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        "Anthropic",
        caps_one("haiku", false),
        Arc::clone(&a_seen),
    );
    let g_reg = reg(
        "gemini",
        "Gemini",
        caps_one("flash", true),
        Arc::clone(&g_seen),
    );

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "haiku",
    );
    cfg.providers = vec![a_reg, g_reg];
    cfg.startup_connect = StartupConnectPolicy::All;

    let host = Host::start(cfg).await.expect("host starts");
    assert_eq!(host.active_provider().await.as_str(), "anthropic");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _ = host
        .run_turn_streaming_with_blocks(image_blocks("describe this"), tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 50).await;
    let routed = events
        .iter()
        .find_map(|ev| match ev {
            TurnEvent::RouteSelected {
                provider_id,
                model_id,
                reason,
            } => Some((provider_id.clone(), model_id.clone(), reason.clone())),
            _ => None,
        })
        .expect("RouteSelected event");
    assert_eq!(routed.0.as_str(), "anthropic");
    assert_eq!(routed.1, "haiku");
    assert_eq!(routed.2, RoutingReason::Default);

    // Gemini's mock must not have been called.
    assert_eq!(*g_seen.lock().await, None);
    assert_eq!(*a_seen.lock().await, Some("haiku".into()));

    // Warning fires because vision was needed and the redirect failed.
    let warning = events
        .iter()
        .find(|ev| matches!(ev, TurnEvent::ModalityWarning { .. }));
    assert!(warning.is_some());

    host.shutdown().await;
}

#[tokio::test]
async fn override_wins_over_modality_and_still_warns() {
    // User typed `@anthropic:haiku <image>` — override pins a
    // vision-incapable model. RoutingReason must be Override; the
    // warning event still fires so the user sees why the next call
    // may fail.
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        "Anthropic",
        caps_haiku_plus_opus(),
        Arc::clone(&a_seen),
    );

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "opus",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;

    let host = Host::start(cfg).await.expect("host starts");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    // First block carries the @-prefix; second is the image. The
    // host's @-parser runs on the leading Text block per Task 4.
    let blocks = vec![
        ContentBlock::Text {
            text: "@anthropic:haiku describe this".into(),
        },
        ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: MediaType::Png,
                data: "AAAA".into(),
            },
        },
    ];
    let _ = host
        .run_turn_streaming_with_blocks(blocks, tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 50).await;
    let routed = events
        .iter()
        .find_map(|ev| match ev {
            TurnEvent::RouteSelected {
                provider_id,
                model_id,
                reason,
            } => Some((provider_id.clone(), model_id.clone(), reason.clone())),
            _ => None,
        })
        .expect("RouteSelected event");
    assert_eq!(routed.0.as_str(), "anthropic");
    assert_eq!(routed.1, "haiku");
    assert_eq!(routed.2, RoutingReason::Override);

    assert_eq!(*a_seen.lock().await, Some("haiku".into()));

    let warning = events
        .iter()
        .find(|ev| matches!(ev, TurnEvent::ModalityWarning { .. }));
    assert!(warning.is_some(), "warning must fire on override-no-vision");

    host.shutdown().await;
}
```

- [ ] **Step 3: Run all four tests**

Run: `cargo test -p savvagent-host --test modality_routing -- --nocapture`
Expected: all four PASS.

Common gotchas:
- If `Host::start` returns `NoActiveProvider`, verify `cfg.startup_connect = StartupConnectPolicy::All` was set.
- If a turn hangs, the synthetic provider returned `EndTurn` cleanly, so the loop should exit immediately. If it doesn't, double-check `cfg.max_iterations` (defaults from `HostConfig::new` are non-zero).
- If `@`-prefix parsing on the leading text block doesn't strip `@anthropic:haiku`, check Task 4's refactor wired the parser correctly. The test asserts `routed.1 == "haiku"` which only passes if both (a) the override was parsed and (b) the parser ran on the leading text block.

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent-host/tests/modality_routing.rs
git commit -m "test(host): end-to-end modality routing (4 cases: same-provider, no-vision, no-silent-hop, override+warning)"
```

---

## Task 9: README + CHANGELOG entry

**Files:**
- Modify: `README.md`
- Modify: `CHANGELOG.md`

Per `[[feedback_release_notes]]` and `[[feedback_release_docs]]`: every release ships with release notes + README update in the same commit as the version bump.

- [ ] **Step 1: Find the right README section**

Run: `grep -n '@provider\|@-prefix\|@@' README.md`
Expected: a section under slash commands or routing introduced in Phase 3.

Read that section + 5 lines on either side to understand the current voice.

- [ ] **Step 2: Add the modality routing paragraph**

Edit `README.md`. Add immediately after the `@provider:model` section:

```markdown
### Automatic modality routing

When you attach an image to your message, savvagent inspects the active
provider's chosen model. If it doesn't support vision (for example
`claude-haiku-4-5` or `o3`), the router automatically switches to a
sibling model on the **same provider** that does (e.g. haiku → opus on
Anthropic). The transcript badge above the response shows
`Modality(image)` when this happens.

If the active provider has no vision-capable model at all, the request
goes through to the active model unchanged and a muted note warns that
the model may reject it. The router does NOT silently jump to a
different provider — even if another connected provider has a
vision-capable model, that crosses a billing boundary you didn't pick.
Use `/use <provider>` to switch to a vision-capable provider, or
prefix the message with `@<provider>` to route just this turn.

Explicit `@provider:model` overrides always win, even when an image is
attached. If you pin a vision-incapable model with `@`, the request
still runs, the warning fires, and the provider's error (if any)
surfaces normally.
```

- [ ] **Step 3: Add the CHANGELOG entry**

Edit `CHANGELOG.md`. Insert at the top, above the `## 0.17.0` entry:

```markdown
## 0.18.0 - 2026-05-18

### Added

- **Automatic modality routing for image inputs.** When your message
  contains an image and the active provider's chosen model doesn't
  support vision, the router auto-redirects the turn to a sibling
  model on the **same provider** that does. Cross-provider redirects
  are not done automatically — that crosses a billing boundary the
  user picked. The transcript badge shows `Modality(image)` when a
  same-provider redirect happens.
- **`TurnEvent::ModalityWarning`.** Surfaced as a muted note in the TUI
  when an image is attached but the active provider has no
  vision-capable model, or when an `@`-override pinned a
  vision-incapable model. The request still runs; the warning
  explains why the next call may fail.
- **`Host::run_turn_streaming_with_blocks(content, events)`.** Public
  entrypoint that accepts a user turn as a `Vec<ContentBlock>` instead
  of a string, so a future image-upload UX can deliver image blocks
  without going through the text-only path.

### Internal

- Phase 4 of the multi-provider-pool roadmap (see
  `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`).
  New `crates/savvagent-host/src/router/modality.rs` module with field
  names (`has_image`, `has_pdf`, `has_audio`) aligned to Phase 5's
  `routing.toml` predicates so the user-rules layer can bind to the
  same struct without rename. `Router::pick` takes a new
  `RequiredModalities` argument; the `#[non_exhaustive]`
  `RoutingReason` enum gains a `Modality { kind }` variant.
```

- [ ] **Step 4: Verify the additions**

Run: `grep -n '0.18.0\|Modality(image)' README.md CHANGELOG.md | head -10`
Expected: each file has matching entries.

- [ ] **Step 5: Commit**

```bash
git add README.md CHANGELOG.md
git commit -m "docs: modality routing release notes + README section (Phase 4)"
```

---

## Task 10: Version bump to 0.18.0

**Files:**
- Modify: `Cargo.toml`

Per `[[feedback_semver]]`: pre-1.0, MINOR bump for new user-visible capability. Per `[[feedback_phase_release_rollup]]` + `[[project_multi_provider_release.md]]`: the per-phase `release(0.X.0)` commit on master is scaffolding for the multi-provider rollup; no tag is pushed until the full multi-provider initiative lands. The commit message can still be `release(0.18.0)` so the rollup tag picks up a clean changelog entry.

- [ ] **Step 1: Bump the workspace version**

Run: `grep -c 'version = "0.17.0"' Cargo.toml`
Expected: 12 or more hits (the workspace.package + every `[workspace.dependencies]` literal).

Edit `Cargo.toml`. Use the Edit tool with `replace_all = true` and:

- `old_string`: `version = "0.17.0"`
- `new_string`: `version = "0.18.0"`

Run: `grep -c 'version = "0.18.0"' Cargo.toml && grep -c 'version = "0.17.0"' Cargo.toml`
Expected: same count of 0.18.0 hits as before, zero 0.17.0 hits.

- [ ] **Step 2: Confirm the workspace still builds clean**

Run: `cargo check --workspace --all-targets`
Expected: clean. No version-pinning errors.

- [ ] **Step 3: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: green. If any failure mentions `TurnEvent` and `non_exhaustive` not honored, search for new exhaustive matches added since Phase 3 (rare; the enum is already `#[non_exhaustive]`).

- [ ] **Step 4: Verify with the same stable toolchain CI uses**

Per `[[feedback_match_ci_toolchain_locally.md]]`:

Run: `rustup run stable cargo fmt --check --all && rustup run stable cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit the version bump**

```bash
git add Cargo.toml Cargo.lock
git commit -m "release(0.18.0): modality-aware routing redirects image inputs to vision-capable models"
```

---

## Task 11: Push and verify CI

**Files:** none (push + verify only).

Per `[[feedback_verify_ci_after_push.md]]`: never call a push "done" without `gh run` confirming green for that SHA.

- [ ] **Step 1: Push the branch**

The current branch will already be named for Phase 4 work (a `git-expert`-style new-branch flow is fine if the work was done on `phase-3-cross-provider-routing`). Confirm:

Run: `git branch --show-current`

If still on `phase-3-cross-provider-routing`, create + check out a Phase 4 branch first:

Run: `git checkout -b phase-4-modality-routing`

Push:

Run: `git push -u origin phase-4-modality-routing`

- [ ] **Step 2: Open the PR**

Use `gh pr create` (the user prefers `git-expert` for git interactions per their global CLAUDE.md). Title: `feat: Phase 4 — modality-aware routing`. Body: a short summary plus a link to the spec section + the plan path.

- [ ] **Step 3: Wait for CI**

Run: `gh run watch --branch phase-4-modality-routing` (or `gh pr checks <pr_number> --watch`).
Expected: all checks green.

If clippy fires on the new `modality.rs` module:
- Common issue: an unused `import` if `RequiredModalityKind` isn't used in some doc-test. Fix the import surface.
- `match` exhaustiveness on `RoutingReason` (rare — the enum is `#[non_exhaustive]`).

- [ ] **Step 4: Update the multi-provider tracking issue**

Per `[[feedback_keep_issue_updated.md]]`: post a comment on the multi-provider tracking issue mentioning the Phase 4 PR + a one-line summary of behavior. Do NOT close the issue — Phases 5 and 6 are still pending.

---

## Self-review checklist (run after Task 10, before pushing)

**Spec coverage:**

- [x] "Modality match" routing-layer section → Task 1 (detector + picker), Task 3 (Router::pick layer), Task 5 (wired into `run_turn_inner`). Same-provider-only policy intentionally narrower than the spec's "highest-priority connected" wording; documented in Task 1 and the README so reviewers can find the decision.
- [x] Phase 4 entry under "Phasing" — "Add `ProviderCapabilities` consumption + per-model `supports_vision` flag; router auto-redirects image-bearing turns" → Tasks 1-8.
- [x] Transcript badge shows `Modality(image)` → Task 2 (Display impl renders `Modality(image)`).
- [x] `RoutingReason` remains `#[non_exhaustive]` → Task 2 keeps the attribute.
- [x] No new content-block types, no PDF/audio detection → modality.rs scope; reserved fields exist for Phase 5 vocabulary alignment but are never set in Phase 4.
- [x] Override always wins, even with image attached → Task 3 (`pick_override_wins_over_modality` unit test) + Task 8 (`override_wins_over_modality_and_still_warns` E2E).
- [x] No-fallback case still routes (Default) with a styled warning → Task 5 (`TurnEvent::ModalityWarning` emit) + Task 6 (TUI rendering) + Task 8 (warning E2E tests).
- [x] Public API for image-bearing turns → Task 7 (`run_turn_streaming_with_blocks`), so a future TUI feature can land without touching routing.
- [x] Phase 5 vocabulary alignment → `RequiredModalities { has_image, has_pdf, has_audio }` matches Phase 5's `routing.toml` predicate names exactly.

**Placeholder scan:**

- [x] No "TBD", "TODO", "fill in details", or "appropriate error handling" — every step shows the code or the exact command.
- [x] Every test step provides the actual test body.
- [x] Version bump command is concrete (`replace_all`).
- [x] No "similar to Task N" — full code repeated in each step.

**Type consistency:**

- [x] `RequiredModalities { has_image, has_pdf, has_audio }` (bitset with three fields) vs. `RequiredModalityKind::Image` (singular enum variant) — consistent across Task 1, Task 2, Task 3, Task 5.
- [x] `pick_vision_capable` (snake_case fn) — consistent.
- [x] `required_modalities(&[Message]) -> RequiredModalities` — consistent.
- [x] `Router::pick` signature: `(override_, providers, active_provider, active_model, required)` — Tasks 3, 5, 8 all align.
- [x] `RoutingReason::Modality { kind }` — Task 2 declares struct-form variant; Task 3 constructs it the same way; Task 5 matches against `Modality { .. }`; Task 6 routes via `Display`/match in the TUI.
- [x] `TurnEvent::ModalityWarning { message }` — Task 5 declares + emits; Task 6 matches in the TUI; Task 8 asserts in the E2E test.
- [x] `run_turn_inner` signature change: from `(String, Option<...>)` to `(Vec<ContentBlock>, Option<...>)` — Task 4 lands the refactor; Task 5 extends the body; Task 7 adds the new public entrypoint; existing entrypoints unchanged at the public boundary.
- [x] `HostConfig::new(ProviderEndpoint::StreamableHttp { url }, model)` + `StartupConnectPolicy::All` — used in Task 8 tests; mirrors `pool_lifecycle.rs` exactly; verified against `crates/savvagent-host/src/config.rs:199`.
- [x] `ProviderRegistration::new(id, display_name, client, capabilities)` — used in Task 8 tests; verified against `crates/savvagent-host/src/config.rs:60`.
- [x] `CompleteResponse { id, model, content, stop_reason, stop_sequence, usage }` — all six fields populated; verified against `crates/savvagent-protocol/src/response.rs:10`.
- [x] `ListModelsResponse { models, default_model_id }` — both fields populated; verified against `crates/savvagent-protocol/src/models.rs:14`.
- [x] `host.messages()` — used in Task 7 unit test; verified at `session.rs:974`. Not `host.history()`.

If you find any drift while implementing, fix it in the file you're editing and update later tasks before they're reached.

---

## Execution

Plan complete and saved to `docs/superpowers/plans/2026-05-18-multi-provider-pool-phase-4.md`.
