//! `internal:command-palette` — filterable list of all visible slash
//! commands, opened via the `/` keybinding from the home view.

pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    BoundAction, ChordPortable, Contributions, Effect, KeyCodePortable, KeyEventPortable, KeyMods,
    KeyScope, KeybindingSpec, Manifest, Plugin, PluginError, PluginId, PluginKind, Region, Screen,
    ScreenArgs, ScreenLayout, ScreenSpec, SlotSpec, StyledLine,
};

use screen::PaletteScreen;

/// Plugin wrapper for the filterable slash-command picker.
///
/// Registers the `palette` screen, the `OnHome` `/` keybinding, and a
/// `home.tips` slot contribution so future palette hints can ship without
/// manifest changes.
pub struct CommandPalettePlugin;

impl CommandPalettePlugin {
    /// Create a new `CommandPalettePlugin`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CommandPalettePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for CommandPalettePlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        // Rendered as an inline `BottomSheet` directly above the prompt
        // (matching Copilot CLI / Claude Code / OpenCode's `/` picker),
        // rather than a floating `CenteredModal`. 12 rows: 1 for the
        // `> filter` line, 1 spacer/scroll-hint, up to 9 command rows,
        // and 1 reserved for the runtime's tips row at the bottom.
        contributions.screens = vec![ScreenSpec {
            id: "palette".into(),
            layout: ScreenLayout::BottomSheet { height: 12 },
        }];
        let open_palette = BoundAction::EmitEffect(Effect::OpenScreen {
            id: "palette".into(),
            args: ScreenArgs::None,
        });
        // `/` opens the palette from the home view. Ctrl-P was a v0.8
        // muscle-memory shortcut but conflicts with tui-textarea's
        // built-in "move cursor up" binding — relevant now that the
        // prompt grows for multi-line input — so it's been retired.
        contributions.keybindings = vec![KeybindingSpec {
            chord: ChordPortable::new(KeyEventPortable {
                code: KeyCodePortable::Char('/'),
                modifiers: KeyMods::default(),
            }),
            scope: KeyScope::OnHome,
            action: open_palette,
        }];
        contributions.slots = vec![SlotSpec {
            slot_id: "home.tips".into(),
            priority: 200,
        }];

        Manifest {
            id: PluginId::new("internal:command-palette").expect("valid built-in id"),
            name: "Command palette".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "/-prefixed command picker".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    fn create_screen(&self, id: &str, _args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        if id != "palette" {
            return Err(PluginError::ScreenNotFound(id.to_string()));
        }
        // `apply_effects::open_screen` replaces this empty placeholder with a
        // `PaletteScreen::with_commands(...)` populated from the runtime's
        // slash index. The empty form here exists only to satisfy the
        // `Plugin::create_screen` contract (which can't reach into the App).
        Ok(Box::new(PaletteScreen::empty()))
    }

    fn render_slot(&self, slot_id: &str, _region: Region) -> Vec<StyledLine> {
        // Lower-priority slot contribution; intentionally empty until
        // PR 4+ adds palette-specific tips. The slot reservation is here
        // so future palette hints (e.g., "Ctrl-K to clear filter") can
        // ship without manifest changes.
        if slot_id != "home.tips" {
            return vec![];
        }
        vec![]
    }
}
