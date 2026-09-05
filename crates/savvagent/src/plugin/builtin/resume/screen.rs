//! Transcript picker — lists saved transcripts; full transcript-replay into
//! the log is deferred to a later milestone (see module doc below).

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine, StyledSpan,
    TextMods, ThemeColor, TranscriptHandle,
};

/// Fullscreen-modal picker that lists saved transcripts. Enter currently
/// closes the picker and surfaces a note that full transcript replay isn't
/// implemented yet — it used to bridge through the now-removed `/view`
/// slash command (removed alongside `/edit`/`/editor-keybindings`), which
/// never had a role beyond a placeholder for this milestone anyway. Full
/// transcript-replay into the log is deferred to a later milestone.
#[derive(Debug)]
pub struct ResumePickerScreen {
    items: Vec<TranscriptHandle>,
    cursor: usize,
}

impl ResumePickerScreen {
    /// Construct a picker pre-loaded with the given transcript handles.
    pub fn new(items: Vec<TranscriptHandle>) -> Self {
        Self { items, cursor: 0 }
    }
}

#[async_trait]
impl Screen for ResumePickerScreen {
    fn id(&self) -> String {
        "resume.picker".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        if self.items.is_empty() {
            return vec![StyledLine {
                spans: vec![StyledSpan {
                    text: rust_i18n::t!("picker.resume.no-transcripts").to_string(),
                    fg: Some(ThemeColor::Warning),
                    bg: None,
                    modifiers: TextMods::default(),
                }],
            }];
        }
        self.items
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let marker = if i == self.cursor { "▶ " } else { "  " };
                StyledLine::plain(format!("{marker}{}", h.label))
            })
            .collect()
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            KeyCodePortable::Esc => Ok(vec![Effect::CloseScreen]),
            KeyCodePortable::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                Ok(vec![])
            }
            KeyCodePortable::Down => {
                let max = self.items.len().saturating_sub(1);
                if self.cursor < max {
                    self.cursor += 1;
                }
                Ok(vec![])
            }
            KeyCodePortable::Enter => {
                if self.items.get(self.cursor).is_none() {
                    return Ok(vec![Effect::CloseScreen]);
                };
                Ok(vec![Effect::Stack(vec![
                    Effect::CloseScreen,
                    Effect::PushNote {
                        line: StyledLine::plain(
                            rust_i18n::t!("notes.transcript-view-unavailable").to_string(),
                        ),
                    },
                ])])
            }
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            rust_i18n::t!("picker.resume.tips").to_string(),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::{KeyMods, Timestamp};

    fn key(c: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code: c,
            modifiers: KeyMods::default(),
        }
    }

    #[tokio::test]
    async fn empty_renders_helpful_message() {
        let s = ResumePickerScreen::new(vec![]);
        let lines = s.render(Region {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        });
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains(rust_i18n::t!("picker.resume.no-transcripts").as_ref()),
            "expected no-transcripts text, got: {joined}"
        );
    }

    #[tokio::test]
    async fn enter_closes_and_notes_replay_unavailable() {
        let mut s = ResumePickerScreen::new(vec![TranscriptHandle {
            id: "transcript-x.json".into(),
            label: "transcript-x.json".into(),
            saved_at: Timestamp { secs: 0, nanos: 0 },
        }]);
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match &effs[0] {
            Effect::Stack(children) => {
                assert!(matches!(children[0], Effect::CloseScreen));
                match &children[1] {
                    Effect::PushNote { line } => {
                        let joined: String = line.spans.iter().map(|s| s.text.clone()).collect();
                        assert!(
                            joined.contains(
                                rust_i18n::t!("notes.transcript-view-unavailable").as_ref()
                            ),
                            "expected transcript-view-unavailable note, got: {joined}"
                        );
                    }
                    _ => panic!(),
                }
            }
            _ => panic!(),
        }
    }
}
