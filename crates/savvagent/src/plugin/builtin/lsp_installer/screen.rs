//! `LspPickerScreen` — `Screen` impl bridging picker outcomes to `Effect`s.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};

use crate::plugin::builtin::lsp_installer::picker::LspPicker;
use crate::plugin::widgets::MultiSelectOutcome;

/// `Screen` impl for `/lsp`. Renders the catalog as a checkbox list,
/// translates portable key events into [`MultiSelectOutcome`]s via the
/// underlying widget, then maps the outcome to closed-vocabulary
/// `Effect`s.
pub struct LspPickerScreen {
    inner: LspPicker,
}

impl LspPickerScreen {
    /// Open a fresh picker. The catalog is read from the static
    /// `CATALOG` slice each time, so consecutive opens see the same
    /// rows.
    pub fn new() -> Self {
        Self {
            inner: LspPicker::new(),
        }
    }
}

impl Default for LspPickerScreen {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Screen for LspPickerScreen {
    fn id(&self) -> String {
        "lsp_installer.picker".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        let mut out: Vec<StyledLine> = Vec::new();
        out.push(StyledLine::plain(format!(
            "Filter: {}",
            self.inner.inner.filter()
        )));
        out.push(StyledLine::plain(""));
        out.push(StyledLine::plain(
            "Select language servers to install. Space toggles, Enter confirms, Esc cancels.",
        ));
        out.push(StyledLine::plain(format!(
            "  Selected: {}",
            self.inner.inner.selected().len()
        )));
        out.push(StyledLine::plain(""));

        for (i, entry) in self.inner.inner.filtered().iter().enumerate() {
            let is_cursor = i == self.inner.inner.cursor();
            let cursor_marker = if is_cursor { ">" } else { " " };
            let mark = if self.inner.inner.selected().contains(entry.id) {
                "[x]"
            } else {
                "[ ]"
            };
            let category = entry.method.category_label();
            out.push(StyledLine {
                spans: vec![StyledSpan {
                    text: format!(
                        "{cursor_marker} {mark} {:<32} {:<12} {:<14} ({})",
                        entry.display_name, entry.language_label, entry.version, category
                    ),
                    fg: Some(if is_cursor {
                        ThemeColor::Accent
                    } else {
                        ThemeColor::Fg
                    }),
                    bg: None,
                    modifiers: TextMods {
                        bold: is_cursor,
                        ..Default::default()
                    },
                }],
            });
        }
        out
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        let ct_event = portable_to_crossterm(&key);
        let outcome = self.inner.inner.on_key(ct_event);
        match outcome {
            MultiSelectOutcome::Stay
            | MultiSelectOutcome::Preview(_)
            | MultiSelectOutcome::Toggle(_) => Ok(vec![]),
            MultiSelectOutcome::Cancel => Ok(vec![Effect::CloseScreen]),
            MultiSelectOutcome::Confirm(items) => {
                if items.is_empty() {
                    return Ok(vec![Effect::CloseScreen]);
                }
                let mut args = vec!["__install".to_string()];
                args.extend(items.iter().map(|e| e.id.to_string()));
                Ok(vec![Effect::Stack(vec![
                    Effect::CloseScreen,
                    Effect::RunSlash {
                        name: "lsp".into(),
                        args,
                    },
                ])])
            }
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            "↑/↓ move • Space toggle • Enter install selected • Esc cancel",
        )]
    }
}

fn portable_to_crossterm(key: &KeyEventPortable) -> crossterm::event::KeyEvent {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let code = match key.code {
        KeyCodePortable::Char(c) => KeyCode::Char(c),
        KeyCodePortable::Enter => KeyCode::Enter,
        KeyCodePortable::Esc => KeyCode::Esc,
        KeyCodePortable::Up => KeyCode::Up,
        KeyCodePortable::Down => KeyCode::Down,
        KeyCodePortable::Backspace => KeyCode::Backspace,
        _ => KeyCode::Null,
    };
    let mut mods = KeyModifiers::empty();
    if key.modifiers.ctrl {
        mods |= KeyModifiers::CONTROL;
    }
    if key.modifiers.alt {
        mods |= KeyModifiers::ALT;
    }
    if key.modifiers.shift {
        mods |= KeyModifiers::SHIFT;
    }
    KeyEvent::new(code, mods)
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::KeyMods;

    fn key(code: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code,
            modifiers: KeyMods::default(),
        }
    }

    #[tokio::test]
    async fn esc_closes_screen() {
        let mut s = LspPickerScreen::new();
        let effs = s.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs.as_slice(), [Effect::CloseScreen]));
    }

    #[tokio::test]
    async fn enter_with_zero_selection_just_closes() {
        let mut s = LspPickerScreen::new();
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        assert!(matches!(effs.as_slice(), [Effect::CloseScreen]));
    }

    #[tokio::test]
    async fn enter_with_one_selection_emits_runslash_install() {
        let mut s = LspPickerScreen::new();
        s.on_key(key(KeyCodePortable::Char(' '))).await.unwrap();
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match &effs[..] {
            [Effect::Stack(children)] => {
                assert!(matches!(children[0], Effect::CloseScreen));
                match &children[1] {
                    Effect::RunSlash { name, args } => {
                        assert_eq!(name, "lsp");
                        assert_eq!(args[0], "__install");
                        assert_eq!(args.len(), 2, "exactly one id appended");
                    }
                    other => panic!("expected RunSlash, got {other:?}"),
                }
            }
            other => panic!("expected single Stack, got {other:?}"),
        }
    }

    #[test]
    fn id_is_picker() {
        let s = LspPickerScreen::new();
        assert_eq!(s.id(), "lsp_installer.picker");
    }
}
