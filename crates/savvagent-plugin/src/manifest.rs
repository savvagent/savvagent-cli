//! Manifest + Contributions + per-kind Spec types.

use crate::effect::BoundAction;
use crate::event::HookKind;
use crate::types::{ChordPortable, PluginId, ProviderId, ThemeEntry};

/// Static metadata a plugin advertises at registration. Indexed by the
/// runtime; not re-queried per frame.
///
/// `Eq` is not derived because the transitively-contained
/// [`crate::Effect::RegisterInProcessTool`] carries a `ToolDef` whose
/// `input_schema: serde_json::Value` is not `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// The plugin id; must be unique across all registered plugins.
    pub id: PluginId,
    /// Human-readable name of the plugin, shown in the plugin manager.
    pub name: String,
    /// SemVer version string for the plugin (e.g. `"0.9.0"`).
    pub version: String,
    /// One-line description shown beneath the name in the plugin manager.
    pub description: String,
    /// Whether this plugin is core or user-toggleable.
    pub kind: PluginKind,
    /// All capabilities and UI contributions this plugin registers.
    pub contributions: Contributions,
}

/// Controls whether a plugin can be disabled by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginKind {
    /// Cannot be disabled by the user. Greyed-out in the plugin manager screen.
    Core,
    /// User-toggleable. Persists across runs via `~/.savvagent/plugins.toml`.
    Optional,
}

/// Bundled vectors of all contribution kinds a plugin registers.
///
/// Constructed by `Plugin::manifest` and indexed once at startup by the
/// runtime. Fields are independent; a plugin may fill any subset.
///
/// `Eq` is not derived (see [`Manifest`] for why).
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct Contributions {
    /// Slash commands this plugin handles (name + summary + optional args hint).
    pub slash_commands: Vec<SlashSpec>,
    /// Screens this plugin provides; pushed onto the runtime's screen stack.
    pub screens: Vec<ScreenSpec>,
    /// Theme catalog entries contributed by this plugin.
    pub themes: Vec<ThemeEntry>,
    /// LLM provider descriptors registered by this plugin.
    pub providers: Vec<ProviderSpec>,
    /// Host-lifecycle hooks this plugin subscribes to.
    pub hooks: Vec<HookKind>,
    /// Render-slot contributions (footer segments, tips line). Sorted by
    /// priority ascending at index-build time.
    pub slots: Vec<SlotSpec>,
    /// Keybinding registrations contributed by this plugin.
    pub keybindings: Vec<KeybindingSpec>,
    /// Tool names this plugin renders summaries for via
    /// `Plugin::summarize_tool_call` / `summarize_tool_result`.
    pub tool_summaries: Vec<ToolSummarySpec>,
}

/// Registration descriptor for a slash command contributed by a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashSpec {
    /// Command name without the leading `/` (e.g. `"theme"`).
    pub name: String,
    /// One-line summary shown in the command palette.
    pub summary: String,
    /// Optional usage hint shown in the command palette after the command name.
    /// Hint convention: `<path>` denotes a required argument; `[provider]` /
    /// `[list | <slug>]` denote optional arguments. The string itself is
    /// purely informational — runtime behavior is driven by
    /// [`SlashSpec::requires_arg`], not by inspecting the hint shape.
    pub args_hint: Option<String>,
    /// `true` if invoking the slash with no argument is a usage error.
    ///
    /// When `true`, the command palette prefills `"/<name> "` into the
    /// textarea on selection so the user can complete the command (e.g.
    /// `/view <path>`) rather than firing it immediately. When `false`,
    /// the palette dispatches the slash on selection — used by slashes
    /// whose no-arg behavior is meaningful (`/connect`, `/theme`,
    /// `/save`, `/language`, …).
    pub requires_arg: bool,
}

/// Registration descriptor for a screen contributed by a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenSpec {
    /// Unique identifier for this screen, used in [`crate::types::ScreenArgs`] and [`crate::effect::Effect::OpenScreen`].
    pub id: String,
    /// Describes how the screen is sized and positioned within the terminal.
    pub layout: ScreenLayout,
}

/// Geometry and chrome rules for a contributed screen.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScreenLayout {
    /// Occupies the entire terminal, optionally hiding the status bar and title.
    Fullscreen {
        /// When `true`, the runtime hides its chrome (status bar, title) while
        /// this screen is on top.
        hide_chrome: bool,
    },
    /// A floating box centred over the terminal with percentage-based sizing.
    CenteredModal {
        /// Modal width as a percentage of the terminal width (0-100).
        width_pct: u16,
        /// Modal height as a percentage of the terminal height (0-100).
        height_pct: u16,
        /// Optional title string rendered in the modal's border.
        title: Option<String>,
    },
    /// Anchored to the bottom of the terminal with a fixed row height.
    BottomSheet {
        /// Height of the sheet in terminal rows.
        height: u16,
    },
}

/// Registration descriptor for an LLM provider contributed by a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSpec {
    /// Stable identifier for the provider (e.g. `ProviderId::new("anthropic").unwrap()`).
    pub id: ProviderId,
    /// Human-readable name shown in provider picker UIs.
    pub display_name: String,
    /// `true` if the user must supply an API key or equivalent credential.
    pub requires_credential: bool,
    /// `true` if the provider runs in-process rather than over the network.
    pub in_process: bool,
}

/// Registration descriptor for a render-slot contribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotSpec {
    /// Identifier of the slot this contribution targets (e.g. `"home.tips"`).
    pub slot_id: String,
    /// Render priority; lower values are rendered first (closer to the top or left).
    pub priority: i32,
}

/// Registration descriptor for a keybinding contributed by a plugin.
///
/// `Eq` is not derived because [`BoundAction`] cannot be `Eq` (see [`Effect`](crate::Effect)).
#[derive(Debug, Clone, PartialEq)]
pub struct KeybindingSpec {
    /// The key chord that triggers this binding.
    pub chord: ChordPortable,
    /// The scope in which this binding is active.
    pub scope: KeyScope,
    /// The action to perform when the chord fires in scope.
    pub action: BoundAction,
}

/// Registration descriptor for a tool-summary contribution. Declares one
/// tool name (matched against `ContentBlock::ToolUse.name`) this plugin
/// renders via `Plugin::summarize_tool_call` / `summarize_tool_result`.
///
/// Duplicate tool names across plugins are resolved by the registry at
/// index-build time: first-registered wins, conflicts log a `warn!`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSummarySpec {
    /// The tool name this plugin claims (e.g. `"read_file"`).
    pub tool_name: String,
}

/// Describes which screen context a keybinding is active in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyScope {
    /// Active everywhere (home view and any screen).
    Global,
    /// Active only when no screen is on top of the stack.
    OnHome,
    /// Active only when the named screen is on top of the stack.
    OnScreen(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_contributions_is_empty() {
        let c = Contributions::default();
        assert!(c.slash_commands.is_empty());
        assert!(c.screens.is_empty());
        assert!(c.themes.is_empty());
        assert!(c.providers.is_empty());
        assert!(c.hooks.is_empty());
        assert!(c.slots.is_empty());
        assert!(c.keybindings.is_empty());
        assert!(c.tool_summaries.is_empty());
    }

    #[test]
    fn manifest_can_carry_tool_summaries() {
        let m = Manifest {
            id: PluginId("internal:test-tool-fs".into()),
            name: "Test tool-fs summaries".into(),
            version: "0.10.0".into(),
            description: "Summaries for tool-fs tool names".into(),
            kind: PluginKind::Core,
            contributions: Contributions {
                tool_summaries: vec![
                    ToolSummarySpec {
                        tool_name: "read_file".into(),
                    },
                    ToolSummarySpec {
                        tool_name: "write_file".into(),
                    },
                ],
                ..Contributions::default()
            },
        };
        assert_eq!(m.contributions.tool_summaries.len(), 2);
        assert_eq!(m.contributions.tool_summaries[0].tool_name, "read_file");
    }

    #[test]
    fn manifest_constructs() {
        let m = Manifest {
            id: PluginId("internal:home-tips".into()),
            name: "Home tips".into(),
            version: "0.9.0".into(),
            description: "Renders the tips line above the prompt".into(),
            kind: PluginKind::Core,
            contributions: Contributions {
                slots: vec![SlotSpec {
                    slot_id: "home.tips".into(),
                    priority: 100,
                }],
                ..Contributions::default()
            },
        };
        assert_eq!(m.kind, PluginKind::Core);
        assert_eq!(m.contributions.slots.len(), 1);
    }
}
