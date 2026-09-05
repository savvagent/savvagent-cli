//! Effect enum — the closed vocabulary plugins use to request host actions.
//! See `docs/superpowers/specs/2026-05-12-v0.9.0-plugin-system-design.md`.

use crate::styled::StyledLine;
use crate::types::{ProviderId, ScreenArgs};
use savvagent_protocol::ToolDef;

/// Closed vocabulary of host operations a plugin can request. Returned from
/// `Plugin::handle_slash`, `Plugin::on_event`, and `Screen::on_key`.
///
/// `Debug` is hand-rolled (see below) because
/// [`Effect::RegisterInProcessTool`] carries
/// [`InProcessToolHandlerArc`](crate::InProcessToolHandlerArc), a newtype over
/// `Arc<dyn InProcessToolHandler>` whose target is not itself `Debug`.
///
/// `Eq` is intentionally not derived: the same variant carries a
/// `savvagent_protocol::ToolDef` whose `input_schema: serde_json::Value`
/// transitively includes `f64` and therefore can't be `Eq`. `PartialEq`
/// still works — the newtype supplies pointer-equality for the handler.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub enum Effect {
    /// Append a styled-line note to the conversation log.
    PushNote {
        /// The styled text line to append.
        line: StyledLine,
    },
    /// Push a new screen onto the runtime's screen stack.
    OpenScreen {
        /// Unique identifier for the screen to open.
        id: String,
        /// Arguments forwarded to the screen's constructor.
        args: ScreenArgs,
    },
    /// Pop the current screen from the runtime's screen stack.
    CloseScreen,
    /// Switch the active UI theme.
    SetActiveTheme {
        /// Slug of the theme to activate (e.g. `"dark"` or `"solarized"`).
        slug: String,
        /// Whether to persist the selection across sessions.
        persist: bool,
    },
    /// Switch the active UI locale.
    SetActiveLocale {
        /// Locale code from the shipped catalog (e.g. "en", "es", "pt", "hi").
        code: String,
        /// Whether to persist the selection to ~/.savvagent/language.toml.
        persist: bool,
    },
    /// Switch the active provider's model. The runtime resolves the active
    /// provider, rebuilds its in-process host with `id`, optionally persists
    /// the choice to `~/.savvagent/models.toml`, and refreshes
    /// `App::cached_models`. Emitted by the model-picker screen on Enter.
    SetActiveModel {
        /// Bare model id (e.g. `"gemini-2.5-flash"`, no `"models/"` prefix).
        id: String,
        /// Whether to persist the selection to `~/.savvagent/models.toml`.
        persist: bool,
    },
    /// Switch the active LLM provider.
    SetActiveProvider {
        /// Stable identifier of the provider to activate.
        id: ProviderId,
        /// Whether to persist the selection across sessions.
        persist: bool,
    },
    /// Announce that this plugin has a connected `ProviderClient` ready for
    /// use. The runtime fetches the client via a savvagent-internal seam (not
    /// part of the WIT-portable surface).
    RegisterProvider {
        /// Stable identifier for the provider being registered.
        id: ProviderId,
        /// Human-readable name shown in UI pickers.
        display_name: String,
    },
    /// Serialize the current transcript to disk at the given path.
    SaveTranscript {
        /// Absolute or repo-relative file path for the output.
        path: String,
    },
    /// Submit a message to the active provider as if the user typed it.
    PromptSend {
        /// The text to send.
        text: String,
    },
    /// Invoke a registered slash command by name.
    RunSlash {
        /// Name of the slash command, without the leading `/`.
        name: String,
        /// Positional arguments forwarded to the command handler.
        args: Vec<String>,
    },
    /// Erase all entries from the conversation log display.
    ClearLog,
    /// Replace the prompt textarea contents with `text` and position the
    /// cursor at the end. Used by the command palette to seed an in-progress
    /// slash command (e.g. `"/view "`) so the user can complete it via the
    /// `@` file picker rather than have it fire immediately with no args.
    PrefillInput {
        /// The literal text to install in the textarea (no trailing newline).
        text: String,
    },
    /// Shut down the application cleanly.
    Quit,
    /// Open the API-key entry modal for `provider_id`. Provider plugins
    /// emit this from `/connect <provider>` when the keyring has no
    /// credential for the provider, so the user lands on a masked input
    /// instead of a dead-end "key not found" note. The runtime resolves
    /// the id against its provider catalog to populate the prompt
    /// (display name, environment-variable hint); on submit it persists
    /// the key to the keyring and re-runs the connect flow.
    PromptApiKey {
        /// Stable identifier of the provider whose key to collect.
        provider_id: ProviderId,
    },
    /// Enable or disable a registered plugin by id. The runtime updates its
    /// enabled-set, rebuilds derived indexes, and (if the plugin is
    /// [`crate::manifest::PluginKind::Optional`]) persists the new state
    /// to `~/.savvagent/plugins.toml`. Toggling a
    /// [`crate::manifest::PluginKind::Core`] plugin is a no-op at the
    /// runtime level (the manager screen also refuses to emit it).
    TogglePlugin {
        /// Plugin to toggle.
        id: crate::types::PluginId,
        /// Desired enabled state (`true` to enable, `false` to disable).
        enabled: bool,
    },
    /// Re-read `~/.savvagent/routing.toml` and swap the host's stored
    /// rules. Sets `App::pending_routing_reload` so `main.rs::run_app`
    /// can drain it with host access (see `Effect::SetActiveModel` for
    /// the canonical pattern this mirrors).
    ReloadRoutingRules,
    /// Print the active routing rules and the most recent decision as
    /// styled notes. Sets `App::pending_routing_show` for the same
    /// reason as `ReloadRoutingRules`.
    ShowRoutingRules,
    /// Open a URL. The host shells to `xdg-open` (Linux), `open` (macOS),
    /// or `start` (Windows) when `target == SystemBrowser`. When
    /// `target == ContinueConversation`, the host treats the URL as a
    /// follow-up user prompt instead.
    OpenUrl {
        /// Absolute URL. Plugins MUST validate this before emitting;
        /// the host treats untrusted URLs as a security risk.
        url: String,
        /// Where the URL should be opened.
        target: UrlTarget,
    },
    /// Compound: apply children in order. Not atomic — partial application is
    /// observable if a later child fails or has user-visible side effects.
    /// Useful for `vec![SetActiveTheme{..}, CloseScreen]`-style sequences from
    /// a single handler.
    Stack(Vec<Effect>),
    /// Override the model used by the next *single* turn submitted via
    /// [`Effect::PromptSend`]. Cleared after the turn completes. Used by
    /// user-defined slash commands whose frontmatter contains `model:`.
    SetNextTurnModelOverride {
        /// Bare model id (e.g. `"claude-sonnet-4-6"`). Applied directly
        /// to the host via `Host::set_model` before the turn starts; no
        /// pre-flight catalog validation is performed. The provider may
        /// reject an unknown id during the turn, surfaced as the turn's
        /// normal failure mode. After the turn the prior model is
        /// restored.
        id: String,
    },
    /// Result of a trust prompt. Emitted by the trust modal screen and
    /// consumed by the runtime to update the in-memory trust map (and
    /// to persist `"always"` decisions to
    /// `~/.savvagent/trusted-projects.json`). When applied, the runtime
    /// resumes the slash command that triggered the prompt (stored on
    /// `App::pending_slash_after_trust`).
    SetTrustLevel {
        /// Canonical project root path the decision applies to.
        project_root: std::path::PathBuf,
        /// User's choice: `"always"`, `"session-text-only"`, or
        /// `"cancelled"`. Kept as a string to avoid pulling the runtime's
        /// `TrustLevel` enum into the WIT-portable surface.
        decision: String,
    },
    /// Re-call `Plugin::manifest()` for the named plugin and rebuild
    /// the derived manifest indexes (slash commands, render slots,
    /// hooks, keybindings, screens, tool_summaries) from the updated
    /// bundle. Used by `/reload-commands`.
    ReindexPlugin {
        /// Plugin whose manifest should be re-read.
        id: crate::types::PluginId,
    },
    /// Stash `(name, args)` on `App::pending_slash_after_trust` so a
    /// trust modal can resume the dispatch after it resolves. Emitted
    /// by `internal:user-slash-commands` before opening the trust
    /// modal; consumed by `apply_effects` together with
    /// `Effect::OpenScreen`.
    StashPendingSlash {
        /// Slash command name (no leading `/`).
        name: String,
        /// Positional arguments collected with the slash invocation.
        args: Vec<String>,
    },
    /// Announce that this plugin provides a `PreToolUseGate`. The
    /// runtime fetches the gate object via a savvagent-internal seam
    /// (not part of the WIT-portable surface) and installs it on the
    /// host. Mirrors the [`Effect::RegisterProvider`] pattern.
    RegisterPreToolGate {
        /// Plugin id whose `BuiltinHookPlugin::take_pre_tool_gate()`
        /// will be invoked to materialize the gate.
        plugin_id: crate::types::PluginId,
    },
    /// Prepend `text` to the most-recently-submitted user prompt
    /// before it reaches the model. Used by `UserPromptSubmit` hooks
    /// returning `additionalContext`. Multiple emissions concatenate
    /// in order with a `\n\n` separator between each; the original
    /// prompt remains last.
    PrependToPendingPrompt {
        /// Text to prepend. Empty string is a no-op.
        text: String,
    },
    /// Abort the turn that's about to start. Used by `UserPromptSubmit`
    /// or `Stop` hooks that blocked. The runtime renders `reason` as a
    /// `[blocked] …` PushNote in the conversation log; the prompt or
    /// stop is not sent to the model.
    CancelPendingTurn {
        /// User-visible reason. Empty string falls back to
        /// `"blocked by user hook"`.
        reason: String,
    },
    /// Register an in-process tool whose handler runs on the calling
    /// tokio runtime. Used by built-in plugins that need direct access
    /// to host state (the `task` tool from user-agents). The host
    /// stores the handler in `ToolRegistry`'s in-process map; the
    /// `spec.name` must be unique across both in-process and stdio
    /// tools.
    RegisterInProcessTool {
        /// Tool definition forwarded to providers (name, description, schema).
        spec: ToolDef,
        /// Handler invoked when the model calls this tool. The
        /// [`InProcessToolHandlerArc`](crate::InProcessToolHandlerArc) newtype
        /// wraps `Arc<dyn InProcessToolHandler>` and supplies pointer-equality
        /// `PartialEq` plus an opaque `Debug` so this variant works with
        /// `Effect`'s derives.
        handler: crate::InProcessToolHandlerArc,
    },
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Effect::PushNote { line } => f.debug_struct("PushNote").field("line", line).finish(),
            Effect::OpenScreen { id, args } => f
                .debug_struct("OpenScreen")
                .field("id", id)
                .field("args", args)
                .finish(),
            Effect::CloseScreen => f.write_str("CloseScreen"),
            Effect::OpenUrl { url, target } => f
                .debug_struct("OpenUrl")
                .field("url", url)
                .field("target", target)
                .finish(),
            Effect::SetActiveTheme { slug, persist } => f
                .debug_struct("SetActiveTheme")
                .field("slug", slug)
                .field("persist", persist)
                .finish(),
            Effect::SetActiveLocale { code, persist } => f
                .debug_struct("SetActiveLocale")
                .field("code", code)
                .field("persist", persist)
                .finish(),
            Effect::SetActiveModel { id, persist } => f
                .debug_struct("SetActiveModel")
                .field("id", id)
                .field("persist", persist)
                .finish(),
            Effect::SetActiveProvider { id, persist } => f
                .debug_struct("SetActiveProvider")
                .field("id", id)
                .field("persist", persist)
                .finish(),
            Effect::RegisterProvider { id, display_name } => f
                .debug_struct("RegisterProvider")
                .field("id", id)
                .field("display_name", display_name)
                .finish(),
            Effect::SaveTranscript { path } => f
                .debug_struct("SaveTranscript")
                .field("path", path)
                .finish(),
            Effect::PromptSend { text } => {
                f.debug_struct("PromptSend").field("text", text).finish()
            }
            Effect::RunSlash { name, args } => f
                .debug_struct("RunSlash")
                .field("name", name)
                .field("args", args)
                .finish(),
            Effect::ClearLog => f.write_str("ClearLog"),
            Effect::PrefillInput { text } => {
                f.debug_struct("PrefillInput").field("text", text).finish()
            }
            Effect::Quit => f.write_str("Quit"),
            Effect::PromptApiKey { provider_id } => f
                .debug_struct("PromptApiKey")
                .field("provider_id", provider_id)
                .finish(),
            Effect::TogglePlugin { id, enabled } => f
                .debug_struct("TogglePlugin")
                .field("id", id)
                .field("enabled", enabled)
                .finish(),
            Effect::ReloadRoutingRules => f.write_str("ReloadRoutingRules"),
            Effect::ShowRoutingRules => f.write_str("ShowRoutingRules"),
            Effect::Stack(children) => f.debug_tuple("Stack").field(children).finish(),
            Effect::SetNextTurnModelOverride { id } => f
                .debug_struct("SetNextTurnModelOverride")
                .field("id", id)
                .finish(),
            Effect::SetTrustLevel {
                project_root,
                decision,
            } => f
                .debug_struct("SetTrustLevel")
                .field("project_root", project_root)
                .field("decision", decision)
                .finish(),
            Effect::ReindexPlugin { id } => {
                f.debug_struct("ReindexPlugin").field("id", id).finish()
            }
            Effect::StashPendingSlash { name, args } => f
                .debug_struct("StashPendingSlash")
                .field("name", name)
                .field("args", args)
                .finish(),
            Effect::RegisterPreToolGate { plugin_id } => f
                .debug_struct("RegisterPreToolGate")
                .field("plugin_id", plugin_id)
                .finish(),
            Effect::PrependToPendingPrompt { text } => f
                .debug_struct("PrependToPendingPrompt")
                .field("text", text)
                .finish(),
            Effect::CancelPendingTurn { reason } => f
                .debug_struct("CancelPendingTurn")
                .field("reason", reason)
                .finish(),
            Effect::RegisterInProcessTool { spec, .. } => f
                .debug_struct("RegisterInProcessTool")
                .field("spec", spec)
                .field("handler", &"<dyn InProcessToolHandler>")
                .finish(),
        }
    }
}

/// Destination for an [`Effect::OpenUrl`] effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlTarget {
    /// Open in the user's default system browser via
    /// `xdg-open`/`open`/`start`.
    SystemBrowser,
    /// Send the URL as a new user prompt in the active conversation
    /// (useful for relative paths the model means as
    /// "look at this file").
    ContinueConversation,
}

/// The right-hand side of a [`crate::manifest::KeybindingSpec`]: either invoke a
/// slash command or emit a typed effect directly.
///
/// `Eq` is not derived because the contained [`Effect`] cannot derive `Eq`
/// (see the comment on `Effect`).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum BoundAction {
    /// Invoke a registered slash command when the keybinding fires.
    RunSlash {
        /// Name of the slash command, without the leading `/`.
        name: String,
        /// Positional arguments forwarded to the command handler.
        args: Vec<String>,
    },
    /// Emit the contained [`Effect`] directly when the keybinding fires.
    EmitEffect(
        /// The effect to emit.
        Effect,
    ),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_is_recursive() {
        let outer = Effect::Stack(vec![
            Effect::SetActiveTheme {
                slug: "dark".into(),
                persist: true,
            },
            Effect::CloseScreen,
        ]);
        match outer {
            Effect::Stack(children) => assert_eq!(children.len(), 2),
            _ => panic!(),
        }
    }

    #[test]
    fn bound_action_holds_an_effect() {
        let _ = BoundAction::EmitEffect(Effect::Quit);
        let _ = BoundAction::RunSlash {
            name: "theme".into(),
            args: vec![],
        };
    }

    #[test]
    fn prefill_input_carries_text() {
        let eff = Effect::PrefillInput {
            text: "/view ".into(),
        };
        match eff {
            Effect::PrefillInput { text } => assert_eq!(text, "/view "),
            _ => panic!("expected PrefillInput"),
        }
    }

    #[test]
    fn toggle_plugin_carries_id_and_state() {
        use crate::types::PluginId;
        let eff = Effect::TogglePlugin {
            id: PluginId::new("internal:provider-anthropic").expect("valid"),
            enabled: false,
        };
        match eff {
            Effect::TogglePlugin { id, enabled } => {
                assert_eq!(id.as_str(), "internal:provider-anthropic");
                assert!(!enabled);
            }
            _ => panic!("expected TogglePlugin"),
        }
    }

    #[test]
    fn set_active_model_carries_id_and_persist() {
        let eff = Effect::SetActiveModel {
            id: "gemini-2.5-flash".into(),
            persist: true,
        };
        match eff {
            Effect::SetActiveModel { id, persist } => {
                assert_eq!(id, "gemini-2.5-flash");
                assert!(persist);
            }
            _ => panic!("expected SetActiveModel"),
        }
    }

    #[test]
    fn set_active_locale_carries_code_and_persist() {
        let eff = Effect::SetActiveLocale {
            code: "es".into(),
            persist: true,
        };
        match eff {
            Effect::SetActiveLocale { code, persist } => {
                assert_eq!(code, "es");
                assert!(persist);
            }
            _ => panic!("expected SetActiveLocale"),
        }
    }

    #[test]
    fn reload_routing_rules_constructs() {
        let _ = Effect::ReloadRoutingRules;
    }

    #[test]
    fn show_routing_rules_constructs() {
        let _ = Effect::ShowRoutingRules;
    }
}

#[cfg(test)]
mod added_hook_effects_smoke {
    use super::*;
    use crate::types::PluginId;

    #[test]
    fn variants_constructable() {
        let _ = Effect::RegisterPreToolGate {
            plugin_id: PluginId::new("internal:user-hooks").unwrap(),
        };
        let _ = Effect::PrependToPendingPrompt {
            text: "context".into(),
        };
        let _ = Effect::CancelPendingTurn {
            reason: "no".into(),
        };
    }
}

#[cfg(test)]
mod added_effects_smoke {
    use super::*;
    use crate::types::PluginId;
    use std::path::PathBuf;

    #[test]
    fn variants_constructable() {
        let _ = Effect::SetNextTurnModelOverride {
            id: "claude-sonnet-4-6".into(),
        };
        let _ = Effect::SetTrustLevel {
            project_root: PathBuf::from("/proj"),
            decision: "always".into(),
        };
        let _ = Effect::ReindexPlugin {
            id: PluginId::new("internal:user-slash-commands").unwrap(),
        };
        let _ = Effect::StashPendingSlash {
            name: "review".into(),
            args: vec!["HEAD".into()],
        };
    }
}

#[cfg(test)]
mod tests_in_process_tool {
    use super::*;
    use crate::InProcessToolHandler;
    use async_trait::async_trait;
    use savvagent_protocol::ToolDef;
    use serde_json::Value;
    use std::sync::Arc;

    struct Stub;

    #[async_trait]
    impl InProcessToolHandler for Stub {
        async fn call(
            &self,
            _input: Value,
            _ctx: Arc<dyn std::any::Any + Send + Sync>,
        ) -> Result<Value, String> {
            Ok(Value::String("ok".into()))
        }
    }

    #[test]
    fn register_in_process_tool_holds_handler() {
        let spec = ToolDef {
            name: "task".into(),
            description: "spawn a subagent".into(),
            input_schema: serde_json::json!({}),
        };
        let effect = Effect::RegisterInProcessTool {
            spec,
            handler: crate::InProcessToolHandlerArc::new(Stub),
        };
        match effect {
            Effect::RegisterInProcessTool { spec, .. } => {
                assert_eq!(spec.name, "task");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn handler_arc_partial_eq_uses_pointer_identity() {
        let a = crate::InProcessToolHandlerArc::new(Stub);
        let b = a.clone();
        assert_eq!(a, b, "clones of the same Arc compare equal");

        let c = crate::InProcessToolHandlerArc::new(Stub);
        assert_ne!(a, c, "independently-constructed handlers compare unequal");
    }

    #[test]
    fn effect_debug_renders_handler_opaquely() {
        let spec = ToolDef {
            name: "task".into(),
            description: "spawn a subagent".into(),
            input_schema: serde_json::json!({}),
        };
        let effect = Effect::RegisterInProcessTool {
            spec,
            handler: crate::InProcessToolHandlerArc::new(Stub),
        };
        let rendered = format!("{effect:?}");
        assert!(
            rendered.contains("RegisterInProcessTool"),
            "expected RegisterInProcessTool variant name in debug output, got: {rendered}"
        );
        assert!(
            rendered.contains("<dyn InProcessToolHandler>"),
            "expected opaque handler placeholder, got: {rendered}"
        );
    }
}
