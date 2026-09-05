//! `internal:prompt-keybindings` — `/prompt-keybindings` opens a
//! scrollable help modal listing the keybindings active in the main
//! prompt input.
//!
//! Mirrors `internal:editor-keybindings` for symmetry: each plugin
//! owns a slash + a screen and contributes its own static sections.
//! The dynamic plugin-contributed bindings section is sourced at open
//! time from [`crate::plugin::manifests::Indexes`] by
//! `apply_effects::open_screen`.

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec, StyledLine,
};

use super::keybindings_view::{KeybindingRow, KeybindingSection, ScrollableKeybindingsScreen};

/// Screen id used by both [`Plugin::manifest`] and the runtime's
/// `open_screen` pre-flight.
pub const SCREEN_ID: &str = "prompt-keybindings.viewer";

/// Core plugin that exposes `/prompt-keybindings`.
pub struct PromptKeybindingsPlugin;

impl PromptKeybindingsPlugin {
    /// Construct a new `PromptKeybindingsPlugin`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for PromptKeybindingsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for PromptKeybindingsPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "prompt-keybindings".into(),
            summary: rust_i18n::t!("slash.prompt-keybindings-summary").to_string(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        contributions.screens = vec![ScreenSpec {
            id: SCREEN_ID.into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 70,
                height_pct: 80,
                title: Some(rust_i18n::t!("picker.prompt-keybindings.modal-title").to_string()),
            },
        }];

        Manifest {
            id: PluginId::new("internal:prompt-keybindings").expect("valid built-in id"),
            name: "Prompt keybindings".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.prompt-keybindings-description").to_string(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        Ok(vec![Effect::OpenScreen {
            id: SCREEN_ID.into(),
            args: ScreenArgs::None,
        }])
    }

    fn create_screen(&self, id: &str, _args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        if id != SCREEN_ID {
            return Err(PluginError::ScreenNotFound(id.to_string()));
        }
        // `apply_effects::open_screen` replaces this with one
        // populated by [`build_prompt_keybindings_screen`] so the
        // dynamic plugin-binding section is current.
        Ok(Box::new(ScrollableKeybindingsScreen::new(
            SCREEN_ID,
            vec![],
            StyledLine::plain(rust_i18n::t!("picker.prompt-keybindings.tips").to_string()),
        )))
    }
}

/// Build the populated prompt-keybindings screen. Called by
/// `apply_effects::open_screen` so the dynamic plugin section reflects
/// the live keybinding index at open time.
pub fn build_prompt_keybindings_screen(
    plugin_rows: Vec<KeybindingRow>,
) -> ScrollableKeybindingsScreen {
    let mut sections = vec![
        section("picker.prompt-keybindings.section-prompt", prompt_rows()),
        section("picker.prompt-keybindings.section-cursor", cursor_rows()),
        section("picker.prompt-keybindings.section-editing", editing_rows()),
        section("picker.prompt-keybindings.section-history", history_rows()),
        section("picker.prompt-keybindings.section-mouse", mouse_rows()),
    ];
    if !plugin_rows.is_empty() {
        sections.push(KeybindingSection {
            title: rust_i18n::t!("picker.prompt-keybindings.section-plugins").to_string(),
            rows: plugin_rows,
        });
    }
    ScrollableKeybindingsScreen::new(
        SCREEN_ID,
        sections,
        StyledLine::plain(rust_i18n::t!("picker.prompt-keybindings.tips").to_string()),
    )
}

fn section(title_key: &str, rows: Vec<KeybindingRow>) -> KeybindingSection {
    KeybindingSection {
        title: rust_i18n::t!(title_key).to_string(),
        rows,
    }
}

/// Editor-level keys that `main.rs` handles directly (before tui-textarea).
fn prompt_rows() -> Vec<KeybindingRow> {
    vec![
        row("Enter", "picker.prompt-keybindings.row.submit"),
        row("Shift+Enter", "picker.prompt-keybindings.row.newline"),
        row("Esc", "picker.prompt-keybindings.row.clear-input"),
        row("Ctrl+C", "picker.prompt-keybindings.row.quit"),
        row("/", "picker.prompt-keybindings.row.open-palette"),
        row("@", "picker.prompt-keybindings.row.file-picker"),
    ]
}

/// Cursor navigation (tui-textarea built-ins).
fn cursor_rows() -> Vec<KeybindingRow> {
    vec![
        row("←/→", "picker.prompt-keybindings.row.cursor-char"),
        row("↑/↓", "picker.prompt-keybindings.row.cursor-line"),
        row(
            "Home / Ctrl+A",
            "picker.prompt-keybindings.row.cursor-line-start",
        ),
        row(
            "End / Ctrl+E",
            "picker.prompt-keybindings.row.cursor-line-end",
        ),
        row("Ctrl+←/→", "picker.prompt-keybindings.row.cursor-word"),
        row(
            "Alt+B / Alt+F",
            "picker.prompt-keybindings.row.cursor-word-alt",
        ),
        row("PgUp / PgDn", "picker.prompt-keybindings.row.cursor-page"),
        row(
            "Alt+< / Alt+>",
            "picker.prompt-keybindings.row.cursor-top-bottom",
        ),
    ]
}

/// Editing (tui-textarea built-ins).
fn editing_rows() -> Vec<KeybindingRow> {
    vec![
        row(
            "Backspace / Ctrl+H",
            "picker.prompt-keybindings.row.delete-prev",
        ),
        row(
            "Delete / Ctrl+D",
            "picker.prompt-keybindings.row.delete-next",
        ),
        row(
            "Ctrl+W / Alt+Backspace",
            "picker.prompt-keybindings.row.delete-word-back",
        ),
        row("Alt+D", "picker.prompt-keybindings.row.delete-word-fwd"),
        row("Ctrl+K", "picker.prompt-keybindings.row.delete-to-end"),
        row("Ctrl+J", "picker.prompt-keybindings.row.delete-to-start"),
        row("Tab", "picker.prompt-keybindings.row.tab"),
    ]
}

/// Undo/redo (savvagent intercepts + tui-textarea defaults) plus the
/// shell-style prompt-history recall bound on Up/Down at an empty input.
fn history_rows() -> Vec<KeybindingRow> {
    vec![
        row("Ctrl+Z / Ctrl+U", "picker.prompt-keybindings.row.undo"),
        row("Ctrl+Y / Ctrl+R", "picker.prompt-keybindings.row.redo"),
        row("↑ / ↓", "picker.prompt-keybindings.row.recall-prompt"),
    ]
}

/// Mouse capture is enabled on a best-effort basis at startup (see
/// `crates/savvagent/src/tui.rs`'s `EnableMouseCapture` call, which logs a
/// warning and continues if the terminal doesn't support it). When it's on,
/// it suppresses the terminal's native click-drag text selection everywhere
/// in the app; Shift bypasses it for native selection/copy.
fn mouse_rows() -> Vec<KeybindingRow> {
    vec![row(
        "Shift+drag / Shift+click",
        "picker.prompt-keybindings.row.mouse-capture",
    )]
}

fn row(chord: &str, description_key: &str) -> KeybindingRow {
    KeybindingRow {
        chord: chord.to_string(),
        description: rust_i18n::t!(description_key).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_slash_and_screen() {
        let p = PromptKeybindingsPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:prompt-keybindings");
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == "prompt-keybindings" && !s.requires_arg)
        );
        assert!(m.contributions.screens.iter().any(|s| s.id == SCREEN_ID));
    }

    #[tokio::test]
    async fn handle_slash_opens_viewer() {
        let mut p = PromptKeybindingsPlugin::new();
        let effs = p.handle_slash("prompt-keybindings", vec![]).await.unwrap();
        match &effs[0] {
            Effect::OpenScreen { id, .. } => assert_eq!(id, SCREEN_ID),
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    #[test]
    fn populated_screen_includes_static_sections() {
        let s = build_prompt_keybindings_screen(vec![]);
        // 5 static sections × (header + blank + N rows) + 4 inter-section blanks.
        assert!(
            s.line_count() > 20,
            "expected populated content; got {} lines",
            s.line_count()
        );
        assert_eq!(s.id(), SCREEN_ID);
    }

    #[test]
    fn dynamic_plugin_rows_become_a_section() {
        let row = KeybindingRow {
            chord: "Ctrl+Shift+X".into(),
            description: "Test".into(),
        };
        let s = build_prompt_keybindings_screen(vec![row]);
        // The rendered lines should contain the chord.
        // (We can't introspect sections via the screen directly; we
        // verify by checking the rendered region instead.)
        let lines = s.render(savvagent_plugin::Region {
            x: 0,
            y: 0,
            width: 80,
            height: 200,
        });
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("Ctrl+Shift+X"), "got: {joined}");
    }
}
