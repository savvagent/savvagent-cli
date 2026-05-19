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
