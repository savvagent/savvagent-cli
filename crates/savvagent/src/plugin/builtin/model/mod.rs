//! `internal:model` — `/model` slash + the model picker screen.
//!
//! With no args, `/model` opens [`screen::ModelPickerScreen`] populated
//! from `App::cached_models` (patched into `ScreenArgs::ModelPicker` by
//! `apply_effects::open_screen`). With an `<id>` argument it emits
//! [`Effect::SetActiveModel`] so the runtime can rebuild the host with
//! the requested model.
//!
//! The legacy `/model <id>` short-circuit in `main.rs::dispatch_slash_command`
//! still handles typed-arg invocations end-to-end (validation against
//! `list_models`, host rebuild, optimistic warnings). The plugin path is
//! used only when there's an active host AND `rest` is empty, in which
//! case `dispatch_slash_command` falls through to the plugin router,
//! which then opens this screen.

pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec,
};

use screen::ModelPickerScreen;

/// Plugin that registers the `/model` slash command and the
/// `model.picker` screen.
pub struct ModelPlugin;

impl ModelPlugin {
    /// Construct a new [`ModelPlugin`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for ModelPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ModelPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "model".into(),
            summary: rust_i18n::t!("slash.model-summary").to_string(),
            args_hint: Some("[<id>]".into()),
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        contributions.screens = vec![ScreenSpec {
            id: "model.picker".into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 60,
                height_pct: 60,
                title: Some(rust_i18n::t!("picker.model.modal-title").to_string()),
            },
        }];
        Manifest {
            id: PluginId::new("internal:model").expect("valid built-in id"),
            name: "Switch model".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.model-description").to_string(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        match args.first() {
            None => Ok(vec![Effect::OpenScreen {
                id: "model.picker".into(),
                // Placeholder; `apply_effects::open_screen` patches in
                // `App::model` + `App::cached_models` before the screen
                // is constructed.
                args: ScreenArgs::ModelPicker {
                    current_id: String::new(),
                    models: vec![],
                },
            }]),
            Some(id) => Ok(vec![Effect::SetActiveModel {
                id: id.clone(),
                persist: true,
            }]),
        }
    }

    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        match (id, args) {
            ("model.picker", ScreenArgs::ModelPicker { current_id, models }) => {
                Ok(Box::new(ModelPickerScreen::new(current_id, models)))
            }
            (other, _) => Err(PluginError::ScreenNotFound(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn slash_no_args_opens_picker() {
        let mut p = ModelPlugin::new();
        let effs = p.handle_slash("model", vec![]).await.unwrap();
        match &effs[0] {
            Effect::OpenScreen { id, args } => {
                assert_eq!(id, "model.picker");
                assert!(matches!(args, ScreenArgs::ModelPicker { .. }));
            }
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn slash_with_id_emits_set_active_model_persist_true() {
        let mut p = ModelPlugin::new();
        let effs = p
            .handle_slash("model", vec!["gemini-2.5-pro".into()])
            .await
            .unwrap();
        match &effs[0] {
            Effect::SetActiveModel { id, persist } => {
                assert_eq!(id, "gemini-2.5-pro");
                assert!(*persist);
            }
            other => panic!("expected SetActiveModel, got {other:?}"),
        }
    }

    #[test]
    fn manifest_declares_slash_and_screen() {
        let p = ModelPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:model");
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == "model" && !s.requires_arg),
            "manifest must advertise /model with requires_arg=false"
        );
        assert!(
            m.contributions
                .screens
                .iter()
                .any(|s| s.id == "model.picker"),
            "manifest must advertise the model.picker screen"
        );
    }

    #[test]
    fn create_screen_returns_picker_for_known_id_and_args() {
        let p = ModelPlugin::new();
        match p.create_screen(
            "model.picker",
            ScreenArgs::ModelPicker {
                current_id: "gemini-2.5-flash".into(),
                models: vec![savvagent_plugin::ModelEntry {
                    id: "gemini-2.5-flash".into(),
                    display_name: "Gemini 2.5 Flash".into(),
                }],
            },
        ) {
            Ok(s) => assert_eq!(s.id(), "model.picker"),
            Err(e) => panic!("create_screen failed unexpectedly: {e:?}"),
        }
    }

    #[test]
    fn create_screen_unknown_id_returns_screen_not_found() {
        let p = ModelPlugin::new();
        match p.create_screen("not-a-screen", ScreenArgs::None) {
            Err(e) => assert!(matches!(e, PluginError::ScreenNotFound(_))),
            Ok(_) => panic!("expected ScreenNotFound error for unknown screen id"),
        }
    }
}
