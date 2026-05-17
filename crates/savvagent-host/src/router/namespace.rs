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
            tool_use_id: tool_use_id
                .strip_prefix(prefix)
                .unwrap_or(tool_use_id)
                .to_string(),
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

    #[test]
    fn namespace_prefixes_tool_use_ids() {
        let blocks = vec![
            ContentBlock::Text { text: "ok".into() },
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
