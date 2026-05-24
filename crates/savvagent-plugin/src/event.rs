//! HostEvent payloads + HookKind discriminants.

use crate::types::ProviderId;

/// Discriminant-only mirror of [`HostEvent`] used as a hook-registry key.
///
/// Each variant corresponds to exactly one [`HostEvent`] variant. Plugins
/// register interest by providing a set of `HookKind`s; the runtime uses
/// these to route fired events without cloning the full payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HookKind {
    /// Emitted once at startup before any other hook.
    HostStarting,
    /// Emitted when a provider connection is established.
    Connect,
    /// Emitted when a provider connection is torn down.
    Disconnect,
    /// Emitted at the beginning of each agent turn.
    TurnStart,
    /// Emitted at the end of each agent turn, carrying success/failure status.
    TurnEnd,
    /// Emitted just before the host dispatches a tool call.
    ToolCallStart,
    /// Emitted after a tool call completes, carrying success/failure status.
    ToolCallEnd,
    /// Emitted when the user submits a prompt to the agent.
    PromptSubmitted,
    /// Emitted after a conversation transcript has been persisted to disk.
    TranscriptSaved,
    /// Emitted after a provider plugin announces a constructed client via
    /// [`crate::effect::Effect::RegisterProvider`] and the runtime has wired
    /// it into the provider table. Subscribers (notably `internal:connect`)
    /// use this to keep their UI in sync.
    ProviderRegistered,
    /// Emitted when the rough conversation context-size estimate changes.
    /// Carries the current `app.context_size` (chars/4 heuristic) so the
    /// home footer can render a `~N ctx` segment without polling.
    ContextSizeChanged,
    /// Emitted when the active provider changes, either on startup or when
    /// the user runs `/use <provider>`. Subscribers (typically provider
    /// plugins) use this to update their slot rendering.
    ActiveProviderChanged,
    /// Emitted once per subagent (Sub-Host) turn after it reaches
    /// `end_turn`, before the result is returned to the parent as a
    /// `task` ToolResult. Does NOT fire for cancelled subagent turns.
    SubagentStop,
}

/// Typed host-lifecycle events that the runtime fires into the plugin bus.
///
/// Each variant carries the minimal payload needed for plugins to react to
/// that lifecycle moment. Use [`HostEvent::kind`] to obtain a [`HookKind`]
/// suitable for hash-map dispatch without consuming the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    /// The host is starting; fired once before any provider connection.
    HostStarting,
    /// A provider was successfully connected.
    Connect {
        /// Identifier of the provider that connected.
        provider_id: ProviderId,
    },
    /// A provider connection was closed or lost.
    Disconnect {
        /// Identifier of the provider that disconnected.
        provider_id: ProviderId,
        /// Human-readable explanation of why the connection ended.
        reason: String,
    },
    /// An agent turn has begun.
    TurnStart {
        /// Monotonic per-session turn counter, starting at 1.
        turn_id: u32,
    },
    /// An agent turn has completed.
    TurnEnd {
        /// Monotonic per-session turn counter matching the corresponding [`HostEvent::TurnStart`].
        turn_id: u32,
        /// `true` if the turn completed without error; `false` otherwise.
        success: bool,
    },
    /// A tool call has been dispatched to the tool registry.
    ToolCallStart {
        /// Opaque identifier that correlates this event with its [`HostEvent::ToolCallEnd`].
        call_id: String,
        /// Name of the tool being invoked.
        tool: String,
    },
    /// A tool call has returned.
    ToolCallEnd {
        /// Opaque identifier matching the corresponding [`HostEvent::ToolCallStart`].
        call_id: String,
        /// `true` if the tool returned without error; `false` otherwise.
        success: bool,
    },
    /// The user submitted a prompt to the agent.
    PromptSubmitted {
        /// The full text of the submitted prompt.
        text: String,
    },
    /// A conversation transcript was written to disk.
    TranscriptSaved {
        /// Absolute path to the saved transcript file.
        path: String,
    },
    /// A provider plugin's constructed client has been wired into the
    /// runtime's provider table. Fired immediately after the
    /// `Effect::RegisterProvider` handler runs.
    ProviderRegistered {
        /// Stable identifier of the provider that just registered.
        id: ProviderId,
        /// Human-readable display name (forwarded from the plugin).
        display_name: String,
    },
    /// The rough conversation context-size estimate changed. The event
    /// loop emits this whenever `App::context_size` (the chars/4
    /// heuristic) moves so footer/status plugins can show a `~N ctx`
    /// segment without polling.
    ContextSizeChanged {
        /// Estimated total context size in tokens.
        tokens: u32,
    },
    /// The active provider changed. Fired by the TUI after a successful
    /// [`crate::effect::Effect::SetActiveProvider`] call (e.g. from
    /// `/use <provider>`) and once on startup with the initial active id.
    ActiveProviderChanged {
        /// Identifier of the provider that is now active.
        id: ProviderId,
    },
    /// Fired when a Sub-Host (subagent) reaches `end_turn`. The result
    /// has not yet been returned to the parent's `task` tool call.
    /// Not fired on cancelled subagent turns.
    SubagentStop {
        /// The agent name (slug) that just finished.
        agent_name: String,
        /// Whether the subagent's turn ended cleanly.
        success: bool,
    },
}

impl HostEvent {
    /// Returns the [`HookKind`] discriminant for this event.
    ///
    /// Useful for index lookups in the plugin hook registry without
    /// requiring ownership of the full payload.
    pub fn kind(&self) -> HookKind {
        match self {
            HostEvent::HostStarting => HookKind::HostStarting,
            HostEvent::Connect { .. } => HookKind::Connect,
            HostEvent::Disconnect { .. } => HookKind::Disconnect,
            HostEvent::TurnStart { .. } => HookKind::TurnStart,
            HostEvent::TurnEnd { .. } => HookKind::TurnEnd,
            HostEvent::ToolCallStart { .. } => HookKind::ToolCallStart,
            HostEvent::ToolCallEnd { .. } => HookKind::ToolCallEnd,
            HostEvent::PromptSubmitted { .. } => HookKind::PromptSubmitted,
            HostEvent::TranscriptSaved { .. } => HookKind::TranscriptSaved,
            HostEvent::ProviderRegistered { .. } => HookKind::ProviderRegistered,
            HostEvent::ContextSizeChanged { .. } => HookKind::ContextSizeChanged,
            HostEvent::ActiveProviderChanged { .. } => HookKind::ActiveProviderChanged,
            HostEvent::SubagentStop { .. } => HookKind::SubagentStop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderId;

    #[test]
    fn kind_matches_variant() {
        let e = HostEvent::Connect {
            provider_id: ProviderId::new("anthropic").unwrap(),
        };
        assert_eq!(e.kind(), HookKind::Connect);

        let e = HostEvent::TurnStart { turn_id: 7 };
        assert_eq!(e.kind(), HookKind::TurnStart);
    }

    #[test]
    fn host_starting_carries_no_payload() {
        let e = HostEvent::HostStarting;
        assert_eq!(e.kind(), HookKind::HostStarting);
    }

    #[test]
    fn kind_maps_every_variant_correctly() {
        let pid = ProviderId::new("p").unwrap();
        let cases: Vec<(HostEvent, HookKind)> = vec![
            (HostEvent::HostStarting, HookKind::HostStarting),
            (
                HostEvent::Connect {
                    provider_id: pid.clone(),
                },
                HookKind::Connect,
            ),
            (
                HostEvent::Disconnect {
                    provider_id: pid.clone(),
                    reason: "x".into(),
                },
                HookKind::Disconnect,
            ),
            (HostEvent::TurnStart { turn_id: 1 }, HookKind::TurnStart),
            (
                HostEvent::TurnEnd {
                    turn_id: 1,
                    success: true,
                },
                HookKind::TurnEnd,
            ),
            (
                HostEvent::ToolCallStart {
                    call_id: "c".into(),
                    tool: "t".into(),
                },
                HookKind::ToolCallStart,
            ),
            (
                HostEvent::ToolCallEnd {
                    call_id: "c".into(),
                    success: true,
                },
                HookKind::ToolCallEnd,
            ),
            (
                HostEvent::PromptSubmitted { text: "hi".into() },
                HookKind::PromptSubmitted,
            ),
            (
                HostEvent::TranscriptSaved {
                    path: "/tmp/t.json".into(),
                },
                HookKind::TranscriptSaved,
            ),
            (
                HostEvent::ProviderRegistered {
                    id: pid.clone(),
                    display_name: "Provider".into(),
                },
                HookKind::ProviderRegistered,
            ),
            (
                HostEvent::ContextSizeChanged { tokens: 42 },
                HookKind::ContextSizeChanged,
            ),
            (
                HostEvent::ActiveProviderChanged { id: pid.clone() },
                HookKind::ActiveProviderChanged,
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(event.kind(), expected, "kind() mismatch for {:?}", event);
        }
    }

    #[test]
    fn subagent_stop_kind_round_trip() {
        let e = HostEvent::SubagentStop {
            agent_name: "code-reviewer".into(),
            success: true,
        };
        assert_eq!(e.kind(), HookKind::SubagentStop);
    }
}
