//! First-run trust modal: `y` / `n` / `q` decision for project-local
//! command directories that include shell substitution.

use std::path::PathBuf;

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, ScreenArgs, StyledLine,
    StyledSpan, TextMods, ThemeColor,
};

/// The trust modal pushed onto the screen stack via
/// `Effect::OpenScreen { id: "trust.modal", args: ScreenArgs::TrustModal { project_root } }`.
#[derive(Debug)]
pub struct TrustModal {
    project_root: PathBuf,
}

impl TrustModal {
    /// Construct from `ScreenArgs::TrustModal` carrying the project root.
    pub fn from_args(args: ScreenArgs) -> Result<Self, PluginError> {
        match args {
            ScreenArgs::TrustModal { project_root } => Ok(Self { project_root }),
            _ => Err(PluginError::ScreenNotFound("trust.modal".into())),
        }
    }
}

#[async_trait]
impl Screen for TrustModal {
    fn id(&self) -> String {
        "trust.modal".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        let path_str = self.project_root.display().to_string();
        vec![
            StyledLine {
                spans: vec![StyledSpan {
                    text: format!("Commands in {path_str} use shell substitution (!cmd)."),
                    fg: Some(ThemeColor::Warning),
                    bg: None,
                    modifiers: TextMods::default(),
                }],
            },
            StyledLine::plain("Trust this project's commands?".to_string()),
            StyledLine::plain(
                "  y = always   n = this session (text-only)   q = cancel".to_string(),
            ),
        ]
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            KeyCodePortable::Char('y') | KeyCodePortable::Char('Y') => Ok(vec![
                Effect::SetTrustLevel {
                    project_root: self.project_root.clone(),
                    decision: "always".into(),
                },
                Effect::CloseScreen,
            ]),
            KeyCodePortable::Char('n') | KeyCodePortable::Char('N') => Ok(vec![
                Effect::SetTrustLevel {
                    project_root: self.project_root.clone(),
                    decision: "session-text-only".into(),
                },
                Effect::CloseScreen,
            ]),
            KeyCodePortable::Char('q') | KeyCodePortable::Char('Q') | KeyCodePortable::Esc => {
                Ok(vec![
                    Effect::SetTrustLevel {
                        project_root: self.project_root.clone(),
                        decision: "cancelled".into(),
                    },
                    Effect::CloseScreen,
                ])
            }
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            "y = always trust   n = session only   q/Esc = cancel".to_string(),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::KeyMods;

    fn modal() -> TrustModal {
        TrustModal {
            project_root: PathBuf::from("/proj/x"),
        }
    }

    fn key(c: char) -> KeyEventPortable {
        KeyEventPortable {
            code: KeyCodePortable::Char(c),
            modifiers: KeyMods::default(),
        }
    }

    fn esc_key() -> KeyEventPortable {
        KeyEventPortable {
            code: KeyCodePortable::Esc,
            modifiers: KeyMods::default(),
        }
    }

    fn other_key() -> KeyEventPortable {
        KeyEventPortable {
            code: KeyCodePortable::Enter,
            modifiers: KeyMods::default(),
        }
    }

    #[tokio::test]
    async fn y_returns_always() {
        let mut m = modal();
        let effs = m.on_key(key('y')).await.unwrap();
        assert_eq!(effs.len(), 2);
        match &effs[0] {
            Effect::SetTrustLevel {
                project_root,
                decision,
            } => {
                assert_eq!(project_root, &PathBuf::from("/proj/x"));
                assert_eq!(decision, "always");
            }
            _ => panic!("expected SetTrustLevel, got {:?}", effs[0]),
        }
        assert!(matches!(effs[1], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn n_returns_session_text_only() {
        let mut m = modal();
        let effs = m.on_key(key('n')).await.unwrap();
        assert_eq!(effs.len(), 2);
        match &effs[0] {
            Effect::SetTrustLevel { decision, .. } => {
                assert_eq!(decision, "session-text-only");
            }
            _ => panic!("expected SetTrustLevel, got {:?}", effs[0]),
        }
        assert!(matches!(effs[1], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn q_returns_cancelled() {
        let mut m = modal();
        let effs = m.on_key(key('q')).await.unwrap();
        assert_eq!(effs.len(), 2);
        match &effs[0] {
            Effect::SetTrustLevel { decision, .. } => {
                assert_eq!(decision, "cancelled");
            }
            _ => panic!("expected SetTrustLevel, got {:?}", effs[0]),
        }
        assert!(matches!(effs[1], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn esc_returns_cancelled() {
        let mut m = modal();
        let effs = m.on_key(esc_key()).await.unwrap();
        assert_eq!(effs.len(), 2);
        match &effs[0] {
            Effect::SetTrustLevel { decision, .. } => {
                assert_eq!(decision, "cancelled");
            }
            _ => panic!("expected SetTrustLevel, got {:?}", effs[0]),
        }
        assert!(matches!(effs[1], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn unrelated_key_is_noop() {
        let mut m = modal();
        let effs = m.on_key(other_key()).await.unwrap();
        assert!(effs.is_empty(), "expected no effects, got {effs:?}");
    }

    #[tokio::test]
    async fn y_uppercase_returns_always() {
        let mut m = modal();
        let effs = m.on_key(key('Y')).await.unwrap();
        assert_eq!(effs.len(), 2);
        match &effs[0] {
            Effect::SetTrustLevel { decision, .. } => {
                assert_eq!(decision, "always");
            }
            _ => panic!("expected SetTrustLevel, got {:?}", effs[0]),
        }
        assert!(matches!(effs[1], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn n_uppercase_returns_session_text_only() {
        let mut m = modal();
        let effs = m.on_key(key('N')).await.unwrap();
        assert_eq!(effs.len(), 2);
        match &effs[0] {
            Effect::SetTrustLevel { decision, .. } => {
                assert_eq!(decision, "session-text-only");
            }
            _ => panic!("expected SetTrustLevel, got {:?}", effs[0]),
        }
        assert!(matches!(effs[1], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn q_uppercase_returns_cancelled() {
        let mut m = modal();
        let effs = m.on_key(key('Q')).await.unwrap();
        assert_eq!(effs.len(), 2);
        match &effs[0] {
            Effect::SetTrustLevel { decision, .. } => {
                assert_eq!(decision, "cancelled");
            }
            _ => panic!("expected SetTrustLevel, got {:?}", effs[0]),
        }
        assert!(matches!(effs[1], Effect::CloseScreen));
    }

    #[test]
    fn from_args_accepts_trust_modal_variant() {
        let args = ScreenArgs::TrustModal {
            project_root: PathBuf::from("/proj/x"),
        };
        let m = TrustModal::from_args(args).unwrap();
        assert_eq!(m.project_root, PathBuf::from("/proj/x"));
    }

    #[test]
    fn from_args_rejects_wrong_variant() {
        let err = TrustModal::from_args(ScreenArgs::None).unwrap_err();
        assert!(matches!(err, PluginError::ScreenNotFound(_)));
    }
}
