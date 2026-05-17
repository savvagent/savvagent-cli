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
use crate::router::ProviderView;
use crate::router::router::RoutingOverride;

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
                provider: a_id.clone(),
                model: "claude-opus-4-7".into(),
            },
            ModelAlias {
                alias: "flash".into(),
                provider: g_id.clone(),
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
