//! `internal:themes` — theme catalog + `/theme` slash + theme picker screen.
//!
//! The catalog itself ([`catalog::Theme`] and the
//! `~/.savvagent/theme.toml` round-trip helpers) and the lifted
//! [`picker::ThemePicker`] state machine live in this directory; this
//! module wraps them in a [`Plugin`] + [`Screen`] pair so the
//! `SelectingTheme` `InputMode` arm in the v0.8 keypath can go away.

pub mod catalog;
pub mod picker;
pub mod screen;
pub mod xterm_colors;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec, StyledLine, ThemeEntry,
};

use catalog::Theme;
use picker::ThemePicker;
use screen::ThemePickerScreen;

/// Core plugin exposing `/theme [list | <slug>]` and the picker modal.
pub struct ThemesPlugin;

impl ThemesPlugin {
    /// Construct a new `ThemesPlugin`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ThemesPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ThemesPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "theme".into(),
            summary: rust_i18n::t!("slash.theme-summary").to_string(),
            args_hint: Some("[list | <slug>]".into()),
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        contributions.screens = vec![ScreenSpec {
            id: "themes.picker".into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 60,
                height_pct: 70,
                title: Some(rust_i18n::t!("picker.themes.modal-title").to_string()),
            },
        }];
        contributions.themes = Theme::all()
            .into_iter()
            .map(|t| t.to_theme_entry())
            .collect();

        Manifest {
            id: PluginId::new("internal:themes").expect("valid built-in id"),
            name: "Themes".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.themes-description").to_string(),
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
            // No args → open the picker. The `current_slug` here is a
            // placeholder; `apply_effects::open_screen` patches it to the
            // app's live `active_theme` before constructing the screen so
            // plugins don't need read access to App state.
            None => Ok(vec![Effect::OpenScreen {
                id: "themes.picker".into(),
                args: ScreenArgs::ThemePicker {
                    current_slug: Theme::default().name().to_string(),
                },
            }]),
            Some("list") => Ok(vec![Effect::PushNote {
                line: format_listing(),
            }]),
            Some(slug) => match Theme::from_name(slug) {
                Some(_) => Ok(vec![Effect::SetActiveTheme {
                    slug: slug.to_string(),
                    persist: true,
                }]),
                None => Ok(vec![Effect::PushNote {
                    line: StyledLine::plain(
                        rust_i18n::t!("notes.theme-not-found", slug = slug).to_string(),
                    ),
                }]),
            },
        }
    }

    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        match (id, args) {
            ("themes.picker", ScreenArgs::ThemePicker { current_slug }) => {
                let current = Theme::from_name(&current_slug).unwrap_or_else(|| {
                    tracing::warn!(
                        slug = %current_slug,
                        "themes.picker received unknown slug; defaulting to Dark"
                    );
                    Theme::default()
                });
                Ok(Box::new(ThemePickerScreen::new(ThemePicker::new(current))))
            }
            (other, _) => Err(PluginError::ScreenNotFound(other.to_string())),
        }
    }

    fn themes(&self) -> Vec<ThemeEntry> {
        Theme::all()
            .into_iter()
            .map(|t| t.to_theme_entry())
            .collect()
    }
}

fn format_listing() -> StyledLine {
    let names: Vec<String> = Theme::all().iter().map(|t| t.name().to_string()).collect();
    StyledLine::plain(
        rust_i18n::t!(
            "picker.themes.listing",
            count = names.len(),
            names = names.join(", ")
        )
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handle_slash_no_args_opens_picker() {
        let mut p = ThemesPlugin::new();
        let effs = p.handle_slash("theme", vec![]).await.unwrap();
        match &effs[0] {
            Effect::OpenScreen { id, .. } => assert_eq!(id, "themes.picker"),
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_slash_list_emits_push_note() {
        let mut p = ThemesPlugin::new();
        let effs = p.handle_slash("theme", vec!["list".into()]).await.unwrap();
        assert!(matches!(effs[0], Effect::PushNote { .. }));
    }

    #[tokio::test]
    async fn handle_slash_known_slug_sets_theme_persist_true() {
        let mut p = ThemesPlugin::new();
        let effs = p.handle_slash("theme", vec!["light".into()]).await.unwrap();
        match &effs[0] {
            Effect::SetActiveTheme { slug, persist } => {
                assert_eq!(slug, "light");
                assert!(*persist);
            }
            other => panic!("expected SetActiveTheme, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_slash_unknown_slug_pushes_note_does_not_set_theme() {
        let mut p = ThemesPlugin::new();
        let effs = p
            .handle_slash("theme", vec!["totally-bogus".into()])
            .await
            .unwrap();
        assert!(matches!(effs[0], Effect::PushNote { .. }));
    }

    #[test]
    fn manifest_declares_screen_and_slash() {
        let p = ThemesPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:themes");
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == "theme")
        );
        assert!(
            m.contributions
                .screens
                .iter()
                .any(|s| s.id == "themes.picker")
        );
        // Catalog round-trip: every Theme should appear in contributions.themes.
        assert_eq!(m.contributions.themes.len(), Theme::all().len());
    }
}
