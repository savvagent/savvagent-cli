//! `internal:language` — language catalog + `/language` slash + language picker screen.

pub mod catalog;
pub mod picker;
pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec, StyledLine,
};

use picker::LanguagePicker;
use screen::LanguagePickerScreen;

pub struct LanguagePlugin;

impl LanguagePlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LanguagePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for LanguagePlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "language".into(),
            summary: rust_i18n::t!("slash.language-summary").to_string(),
            args_hint: Some("[list | <code>]".into()),
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        contributions.screens = vec![ScreenSpec {
            id: "language.picker".into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 60,
                height_pct: 50,
                title: Some(rust_i18n::t!("picker.language.modal-title").to_string()),
            },
        }];

        Manifest {
            id: PluginId::new("internal:language").expect("valid built-in id"),
            name: "Languages".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.language-description").to_string(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        match args.first().map(String::as_str) {
            None | Some("list") => Ok(vec![Effect::OpenScreen {
                id: "language.picker".into(),
                args: ScreenArgs::LanguagePicker {
                    // Placeholder; apply_effects::open_screen patches this to
                    // app.active_language before constructing the screen.
                    current_code: "en".into(),
                },
            }]),
            Some(code) => match catalog::lookup(code) {
                Some(_) => Ok(vec![Effect::SetActiveLocale {
                    code: code.to_string(),
                    persist: true,
                }]),
                None => Ok(vec![Effect::PushNote {
                    line: StyledLine::plain(
                        rust_i18n::t!("notes.language-not-found", code = code).to_string(),
                    ),
                }]),
            },
        }
    }

    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        match (id, args) {
            ("language.picker", ScreenArgs::LanguagePicker { current_code }) => {
                let current = catalog::lookup(&current_code)
                    .map(|l| l.code)
                    .unwrap_or_else(|| {
                        tracing::warn!(
                            code = %current_code,
                            "language.picker received unknown code; defaulting to en"
                        );
                        "en"
                    });
                Ok(Box::new(LanguagePickerScreen::new(LanguagePicker::new(
                    current,
                ))))
            }
            (other, _) => Err(PluginError::ScreenNotFound(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handle_slash_no_args_opens_picker() {
        let mut p = LanguagePlugin::new();
        let effs = p.handle_slash("language", vec![]).await.unwrap();
        match &effs[0] {
            Effect::OpenScreen { id, args } => {
                assert_eq!(id, "language.picker");
                assert!(matches!(args, ScreenArgs::LanguagePicker { .. }));
            }
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_slash_list_also_opens_picker() {
        let mut p = LanguagePlugin::new();
        let effs = p
            .handle_slash("language", vec!["list".into()])
            .await
            .unwrap();
        assert!(matches!(effs[0], Effect::OpenScreen { .. }));
    }

    #[tokio::test]
    async fn handle_slash_known_code_sets_locale_persist_true() {
        let mut p = LanguagePlugin::new();
        let effs = p.handle_slash("language", vec!["es".into()]).await.unwrap();
        match &effs[0] {
            Effect::SetActiveLocale { code, persist } => {
                assert_eq!(code, "es");
                assert!(*persist);
            }
            other => panic!("expected SetActiveLocale, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_slash_unknown_code_pushes_note() {
        let mut p = LanguagePlugin::new();
        let effs = p.handle_slash("language", vec!["xx".into()]).await.unwrap();
        assert!(matches!(effs[0], Effect::PushNote { .. }));
    }

    #[test]
    fn manifest_declares_slash_and_screen() {
        let p = LanguagePlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:language");
        assert_eq!(m.kind, PluginKind::Core);
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == "language"),
            "manifest should advertise the /language slash command"
        );
        assert!(
            m.contributions
                .screens
                .iter()
                .any(|s| s.id == "language.picker"),
            "manifest should advertise the language.picker screen"
        );
    }

    #[test]
    fn create_screen_returns_language_picker_for_known_id_and_args() {
        let p = LanguagePlugin::new();
        match p.create_screen(
            "language.picker",
            ScreenArgs::LanguagePicker {
                current_code: "pt".into(),
            },
        ) {
            Ok(s) => assert_eq!(s.id(), "language.picker"),
            Err(e) => panic!("create_screen failed unexpectedly: {e:?}"),
        }
    }

    #[test]
    fn create_screen_unknown_id_returns_screen_not_found() {
        let p = LanguagePlugin::new();
        match p.create_screen("not-a-screen", ScreenArgs::None) {
            Err(e) => assert!(matches!(e, PluginError::ScreenNotFound(_))),
            Ok(_) => panic!("expected ScreenNotFound error for unknown screen id"),
        }
    }
}
