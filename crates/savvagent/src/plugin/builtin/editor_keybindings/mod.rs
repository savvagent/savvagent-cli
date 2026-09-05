//! `internal:editor-keybindings` — `/editor-keybindings` opens a
//! scrollable help modal listing the keybindings active inside the
//! `view-file` / `edit-file` screens (the ratatui-code-editor widget).
//!
//! Mirrors `internal:prompt-keybindings` for symmetry. Content is
//! sourced from ratatui-code-editor's `Editor::input` match arms
//! (`editor.rs` in the upstream crate) plus the savvagent close/save
//! shortcuts handled by `main.rs`'s pre-screen-dispatch routing for
//! file screens.

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec, StyledLine,
};

use super::keybindings_view::{KeybindingRow, KeybindingSection, ScrollableKeybindingsScreen};

/// Screen id used by both [`Plugin::manifest`] and the runtime's
/// `open_screen` pre-flight.
pub const SCREEN_ID: &str = "editor-keybindings.viewer";

/// Core plugin that exposes `/editor-keybindings`.
pub struct EditorKeybindingsPlugin;

impl EditorKeybindingsPlugin {
    /// Construct a new `EditorKeybindingsPlugin`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for EditorKeybindingsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for EditorKeybindingsPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "editor-keybindings".into(),
            summary: rust_i18n::t!("slash.editor-keybindings-summary").to_string(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        contributions.screens = vec![ScreenSpec {
            id: SCREEN_ID.into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 70,
                height_pct: 80,
                title: Some(rust_i18n::t!("picker.editor-keybindings.modal-title").to_string()),
            },
        }];

        Manifest {
            id: PluginId::new("internal:editor-keybindings").expect("valid built-in id"),
            name: "Editor keybindings".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.editor-keybindings-description").to_string(),
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
        // Editor bindings are static (no plugin extension surface), so
        // the screen is fully populated up-front and the runtime
        // doesn't need to replace it in `open_screen`.
        Ok(Box::new(build_editor_keybindings_screen()))
    }
}

/// Build the populated editor-keybindings screen. Public so callers
/// (`apply_effects::open_screen`, tests) can rebuild on demand.
pub fn build_editor_keybindings_screen() -> ScrollableKeybindingsScreen {
    let sections = vec![
        section("picker.editor-keybindings.section-modal", modal_rows()),
        section("picker.editor-keybindings.section-cursor", cursor_rows()),
        section("picker.editor-keybindings.section-editing", editing_rows()),
        section(
            "picker.editor-keybindings.section-clipboard",
            clipboard_rows(),
        ),
        section("picker.editor-keybindings.section-history", history_rows()),
        section("picker.editor-keybindings.section-mouse", mouse_rows()),
        section("picker.editor-keybindings.section-notes", notes_rows()),
    ];
    ScrollableKeybindingsScreen::new(
        SCREEN_ID,
        sections,
        StyledLine::plain(rust_i18n::t!("picker.editor-keybindings.tips").to_string()),
    )
}

fn section(title_key: &str, rows: Vec<KeybindingRow>) -> KeybindingSection {
    KeybindingSection {
        title: rust_i18n::t!(title_key).to_string(),
        rows,
    }
}

/// Open/close/save shortcuts intercepted by savvagent before the
/// editor sees them (see `main.rs`'s file-screen routing).
fn modal_rows() -> Vec<KeybindingRow> {
    vec![
        row("Esc", "picker.editor-keybindings.row.close"),
        row("q", "picker.editor-keybindings.row.close-view"),
        row("Ctrl+S", "picker.editor-keybindings.row.save"),
    ]
}

/// Cursor + selection (ratatui-code-editor `Editor::input` defaults).
fn cursor_rows() -> Vec<KeybindingRow> {
    vec![
        row("←/→", "picker.editor-keybindings.row.cursor-char"),
        row("↑/↓", "picker.editor-keybindings.row.cursor-line"),
        row("Shift+←/→", "picker.editor-keybindings.row.select-char"),
        row("Shift+↑/↓", "picker.editor-keybindings.row.select-line"),
        row("Ctrl+A", "picker.editor-keybindings.row.select-all"),
    ]
}

/// Text editing.
fn editing_rows() -> Vec<KeybindingRow> {
    vec![
        row("any printable", "picker.editor-keybindings.row.insert-text"),
        row("Enter", "picker.editor-keybindings.row.newline"),
        row("Backspace", "picker.editor-keybindings.row.delete-prev"),
        row("Tab", "picker.editor-keybindings.row.indent"),
        row("Shift+Tab", "picker.editor-keybindings.row.unindent"),
        row("Ctrl+K", "picker.editor-keybindings.row.delete-line"),
        row("Ctrl+D", "picker.editor-keybindings.row.duplicate"),
    ]
}

/// Clipboard.
fn clipboard_rows() -> Vec<KeybindingRow> {
    vec![
        row("Ctrl+C", "picker.editor-keybindings.row.copy"),
        row("Ctrl+V", "picker.editor-keybindings.row.paste"),
        row("Ctrl+X", "picker.editor-keybindings.row.cut"),
    ]
}

/// Undo/redo.
fn history_rows() -> Vec<KeybindingRow> {
    vec![
        row("Ctrl+Z", "picker.editor-keybindings.row.undo"),
        row("Ctrl+Y", "picker.editor-keybindings.row.redo"),
    ]
}

/// Mouse.
fn mouse_rows() -> Vec<KeybindingRow> {
    vec![
        row("Scroll", "picker.editor-keybindings.row.mouse-scroll"),
        row("Click", "picker.editor-keybindings.row.mouse-click"),
        row("Drag", "picker.editor-keybindings.row.mouse-drag"),
        row("Double-click", "picker.editor-keybindings.row.mouse-word"),
        row("Triple-click", "picker.editor-keybindings.row.mouse-line"),
    ]
}

/// Caveats. The savvagent-global Ctrl+C quit fires before the editor
/// sees the key, so the editor's copy binding is shadowed; we surface
/// this so users aren't surprised when Ctrl+C exits the TUI mid-edit.
fn notes_rows() -> Vec<KeybindingRow> {
    vec![
        row("Ctrl+C", "picker.editor-keybindings.row.note-ctrl-c-quits"),
        row(
            "Shift+drag / Shift+click",
            "picker.editor-keybindings.row.note-shift-drag-copy",
        ),
    ]
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
        let p = EditorKeybindingsPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:editor-keybindings");
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == "editor-keybindings" && !s.requires_arg)
        );
        assert!(m.contributions.screens.iter().any(|s| s.id == SCREEN_ID));
    }

    #[tokio::test]
    async fn handle_slash_opens_viewer() {
        let mut p = EditorKeybindingsPlugin::new();
        let effs = p.handle_slash("editor-keybindings", vec![]).await.unwrap();
        match &effs[0] {
            Effect::OpenScreen { id, .. } => assert_eq!(id, SCREEN_ID),
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    #[test]
    fn populated_screen_includes_all_sections() {
        let s = build_editor_keybindings_screen();
        // 7 sections × (header + blank + N rows ≥ 1) + 6 inter-section blanks.
        assert!(
            s.line_count() > 30,
            "expected populated content; got {} lines",
            s.line_count()
        );
        assert_eq!(s.id(), SCREEN_ID);
    }
}
