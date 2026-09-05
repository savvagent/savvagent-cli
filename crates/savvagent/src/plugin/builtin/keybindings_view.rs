//! Shared scrollable, sectioned keybindings viewer.
//!
//! Used by `internal:prompt-keybindings` (and any future help-style
//! plugin with the same shape). Each plugin supplies its own list of
//! [`KeybindingSection`]s; the screen handles rendering, scrolling,
//! and the close keystroke.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};

/// A single keybinding row: chord on the left, description on the right.
/// The chord is the already-formatted display string (e.g. `"Ctrl+Z"`),
/// not a parsed event — it goes straight onto the screen.
#[derive(Debug, Clone)]
pub struct KeybindingRow {
    /// Display chord, e.g. `"Ctrl+Z"`, `"Shift+Enter"`, `"↑/↓"`.
    pub chord: String,
    /// What the chord does, in the active locale.
    pub description: String,
}

/// One labelled group of keybindings. Sections are stacked vertically
/// with a blank line between them; the title is bold/accent.
#[derive(Debug, Clone)]
pub struct KeybindingSection {
    /// Localized section title (e.g. `"Prompt"`, `"Cursor"`).
    pub title: String,
    /// The rows belonging to this section. Empty sections are dropped
    /// at render time so callers can include them unconditionally.
    pub rows: Vec<KeybindingRow>,
}

/// Help-style scrollable screen rendering a list of sections. The
/// screen id is supplied at construction so a single struct can back
/// multiple plugin screens.
#[derive(Debug)]
pub struct ScrollableKeybindingsScreen {
    /// Screen id reported via [`Screen::id`]. Set per-plugin so the
    /// runtime can route close events back to the right slot.
    id: String,
    /// Pre-rendered, already-styled lines — sections are interleaved
    /// with headers and blanks at construction. Scrolling operates on
    /// this list.
    lines: Vec<StyledLine>,
    /// Localized tip line shown at the modal's bottom border.
    tips: StyledLine,
    /// First line index to render. Adjusted by scroll keys; clamped
    /// at render time so the user can't scroll past the bottom.
    scroll: usize,
}

impl ScrollableKeybindingsScreen {
    /// Construct a screen for `id` displaying `sections`. Empty
    /// sections (no rows) are dropped silently so callers can pass a
    /// uniform list of sections without checking for emptiness.
    pub fn new(id: impl Into<String>, sections: Vec<KeybindingSection>, tips: StyledLine) -> Self {
        let mut lines: Vec<StyledLine> = Vec::new();
        for section in sections.into_iter().filter(|s| !s.rows.is_empty()) {
            push_section(&mut lines, &section.title, section.rows);
        }
        Self {
            id: id.into(),
            lines,
            tips,
            scroll: 0,
        }
    }

    /// Test-only accessor for the rendered line count.
    #[cfg(test)]
    pub(crate) fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Test-only accessor for the scroll offset.
    #[cfg(test)]
    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }
}

fn push_section(out: &mut Vec<StyledLine>, title: &str, rows: Vec<KeybindingRow>) {
    if !out.is_empty() {
        out.push(StyledLine::plain(""));
    }
    out.push(StyledLine {
        spans: vec![StyledSpan {
            text: title.to_string(),
            fg: Some(ThemeColor::Accent),
            bg: None,
            modifiers: TextMods {
                bold: true,
                ..Default::default()
            },
        }],
    });
    out.push(StyledLine::plain(""));
    // Pad the chord column so descriptions align within a section.
    let chord_col_width = rows
        .iter()
        .map(|r| r.chord.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    for r in rows {
        let padding = " ".repeat(chord_col_width.saturating_sub(r.chord.chars().count()));
        out.push(StyledLine {
            spans: vec![
                StyledSpan {
                    text: format!("  {}{}", r.chord, padding),
                    fg: Some(ThemeColor::Fg),
                    bg: None,
                    modifiers: TextMods {
                        bold: true,
                        ..Default::default()
                    },
                },
                StyledSpan {
                    text: r.description,
                    fg: Some(ThemeColor::Muted),
                    bg: None,
                    modifiers: TextMods::default(),
                },
            ],
        });
    }
}

#[async_trait]
impl Screen for ScrollableKeybindingsScreen {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn render(&self, region: Region) -> Vec<StyledLine> {
        // Clamp scroll so the last `region.height` rows are the
        // maximum we ever expose — prevents over-scrolling past the
        // bottom. (The runtime clips the returned vec to `region.height`
        // anyway; clamping here keeps the cursor-window state honest.)
        let height = region.height as usize;
        let max_scroll = self.lines.len().saturating_sub(height);
        let start = self.scroll.min(max_scroll);
        self.lines.iter().skip(start).cloned().collect()
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            KeyCodePortable::Esc => Ok(vec![Effect::CloseScreen]),
            KeyCodePortable::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                Ok(vec![])
            }
            KeyCodePortable::Down => {
                self.scroll = self.scroll.saturating_add(1);
                Ok(vec![])
            }
            KeyCodePortable::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
                Ok(vec![])
            }
            KeyCodePortable::PageDown => {
                self.scroll = self.scroll.saturating_add(10);
                Ok(vec![])
            }
            KeyCodePortable::Home => {
                self.scroll = 0;
                Ok(vec![])
            }
            KeyCodePortable::End => {
                self.scroll = self.lines.len();
                Ok(vec![])
            }
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![self.tips.clone()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::KeyMods;

    fn key(c: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code: c,
            modifiers: KeyMods::default(),
        }
    }

    fn section(title: &str, rows: Vec<(&str, &str)>) -> KeybindingSection {
        KeybindingSection {
            title: title.into(),
            rows: rows
                .into_iter()
                .map(|(c, d)| KeybindingRow {
                    chord: c.into(),
                    description: d.into(),
                })
                .collect(),
        }
    }

    #[test]
    fn empty_sections_are_filtered_out() {
        let s = ScrollableKeybindingsScreen::new(
            "id",
            vec![
                section("Has rows", vec![("X", "desc")]),
                KeybindingSection {
                    title: "Empty".into(),
                    rows: vec![],
                },
            ],
            StyledLine::plain("tips"),
        );
        let joined: String = s
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("Has rows"), "got: {joined}");
        assert!(
            !joined.contains("Empty"),
            "empty section should not appear; got: {joined}"
        );
    }

    #[tokio::test]
    async fn esc_closes() {
        let mut s = ScrollableKeybindingsScreen::new(
            "id",
            vec![section("S", vec![("X", "desc")])],
            StyledLine::plain("tips"),
        );
        let effs = s.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn down_increments_then_up_decrements() {
        let mut s = ScrollableKeybindingsScreen::new(
            "id",
            vec![section("S", vec![("A", "1"), ("B", "2"), ("C", "3")])],
            StyledLine::plain("tips"),
        );
        s.on_key(key(KeyCodePortable::Down)).await.unwrap();
        s.on_key(key(KeyCodePortable::Down)).await.unwrap();
        assert_eq!(s.scroll(), 2);
        s.on_key(key(KeyCodePortable::Up)).await.unwrap();
        assert_eq!(s.scroll(), 1);
    }

    #[test]
    fn line_count_includes_header_blanks_and_rows() {
        let s = ScrollableKeybindingsScreen::new(
            "id",
            vec![section("S", vec![("A", "1"), ("B", "2")])],
            StyledLine::plain("tips"),
        );
        // Section adds: 1 title + 1 blank + N rows. With 2 rows: 4 lines.
        // (No leading blank since this is the first section.)
        assert_eq!(s.line_count(), 4);
    }

    #[test]
    fn id_is_returned_as_constructed() {
        let s = ScrollableKeybindingsScreen::new(
            "prompt-keybindings.viewer",
            vec![],
            StyledLine::plain("tips"),
        );
        assert_eq!(s.id(), "prompt-keybindings.viewer");
    }
}
