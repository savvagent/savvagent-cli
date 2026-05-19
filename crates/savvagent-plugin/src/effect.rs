//! Effect enum — the closed vocabulary plugins use to request host actions.
//! See `docs/superpowers/specs/2026-05-12-v0.9.0-plugin-system-design.md`.

use crate::styled::StyledLine;
use crate::types::{ProviderId, ScreenArgs};

/// Closed vocabulary of host operations a plugin can request. Returned from
/// `Plugin::handle_slash`, `Plugin::on_event`, and `Screen::on_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Persist the currently-open file editor's buffer to disk. Emitted
    /// by the `edit-file` screen plugin on Ctrl-S. The runtime resolves
    /// the target path from `App::active_file_path` and the buffer from
    /// `App::editor`; if neither is populated the effect is a no-op.
    SaveActiveFile,
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
    /// Compound: apply children in order. Not atomic — partial application is
    /// observable if a later child fails or has user-visible side effects.
    /// Useful for `vec![SetActiveTheme{..}, CloseScreen]`-style sequences from
    /// a single handler.
    Stack(Vec<Effect>),
}

/// The right-hand side of a [`crate::manifest::KeybindingSpec`]: either invoke a
/// slash command or emit a typed effect directly.
#[derive(Debug, Clone, PartialEq, Eq)]
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
