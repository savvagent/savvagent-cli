//! `internal:plugins-manager` — enable/disable Optional plugins.
//!
//! The slash command `/plugins` opens the [`PluginsManagerScreen`] modal;
//! the runtime populates its row list from the registry + manifests after
//! the empty screen is pushed (see `apply_effects::open_screen`). Toggles
//! flow back through [`Effect::TogglePlugin`], which the runtime persists
//! to `~/.savvagent/plugins.toml`.

pub mod persistence;
pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec,
};

use screen::PluginsManagerScreen;

/// Core plugin exposing `/plugins` and the manager modal.
pub struct PluginsManagerPlugin;

impl PluginsManagerPlugin {
    /// Construct a new `PluginsManagerPlugin`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for PluginsManagerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for PluginsManagerPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "plugins".into(),
            summary: rust_i18n::t!("slash.plugins-summary").to_string(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        contributions.screens = vec![ScreenSpec {
            id: "plugins.manager".into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 80,
                height_pct: 80,
                title: Some(rust_i18n::t!("picker.plugins-manager.modal-title").to_string()),
            },
        }];

        Manifest {
            id: PluginId::new("internal:plugins-manager").expect("valid built-in id"),
            name: "Plugins manager".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.plugins-manager-description").to_string(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        // The runtime ignores `ScreenArgs::PluginsManager`'s body and fills
        // the row list via apply_effects::open_screen, so we don't need to
        // pre-fetch anything here.
        Ok(vec![Effect::OpenScreen {
            id: "plugins.manager".into(),
            args: ScreenArgs::PluginsManager,
        }])
    }

    fn create_screen(&self, id: &str, _args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        if id != "plugins.manager" {
            return Err(PluginError::ScreenNotFound(id.to_string()));
        }
        // The screen needs a populated row list, but the plugin instance has
        // no read access to the registry. `apply_effects::open_screen` calls
        // back into the registry after we return and replaces this empty
        // screen with one populated via `PluginsManagerScreen::with_rows`.
        Ok(Box::new(PluginsManagerScreen::empty()))
    }
}

/// Build a short human-readable summary of a plugin's contributions, used
/// in the plugins-manager row label. Stable wording across releases so
/// the manager screen feels consistent.
pub(crate) fn summarize_contributions(contributions: &savvagent_plugin::Contributions) -> String {
    let mut parts: Vec<String> = Vec::new();
    let slash_n = contributions.slash_commands.len();
    if slash_n > 0 {
        parts.push(format!(
            "{slash_n} slash{}",
            if slash_n == 1 { "" } else { "es" }
        ));
    }
    let screen_n = contributions.screens.len();
    if screen_n > 0 {
        parts.push(format!(
            "{screen_n} screen{}",
            if screen_n == 1 { "" } else { "s" }
        ));
    }
    let theme_n = contributions.themes.len();
    if theme_n > 0 {
        parts.push(format!(
            "{theme_n} theme{}",
            if theme_n == 1 { "" } else { "s" }
        ));
    }
    let provider_n = contributions.providers.len();
    if provider_n > 0 {
        parts.push(format!(
            "{provider_n} provider{}",
            if provider_n == 1 { "" } else { "s" }
        ));
    }
    let hook_n = contributions.hooks.len();
    if hook_n > 0 {
        parts.push(format!(
            "{hook_n} hook{}",
            if hook_n == 1 { "" } else { "s" }
        ));
    }
    let slot_n = contributions.slots.len();
    if slot_n > 0 {
        parts.push(format!(
            "{slot_n} slot{}",
            if slot_n == 1 { "" } else { "s" }
        ));
    }
    let kb_n = contributions.keybindings.len();
    if kb_n > 0 {
        parts.push(format!(
            "{kb_n} keybinding{}",
            if kb_n == 1 { "" } else { "s" }
        ));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handle_slash_opens_manager_screen() {
        let mut p = PluginsManagerPlugin::new();
        let effs = p.handle_slash("plugins", vec![]).await.unwrap();
        match &effs[0] {
            Effect::OpenScreen { id, args } => {
                assert_eq!(id, "plugins.manager");
                assert!(matches!(args, ScreenArgs::PluginsManager));
            }
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    #[test]
    fn manifest_declares_screen_and_slash() {
        let p = PluginsManagerPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:plugins-manager");
        assert!(matches!(m.kind, PluginKind::Core));
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == "plugins")
        );
        assert!(
            m.contributions
                .screens
                .iter()
                .any(|s| s.id == "plugins.manager")
        );
    }

    #[test]
    fn create_screen_returns_empty_screen_for_id() {
        let p = PluginsManagerPlugin::new();
        let s = p
            .create_screen("plugins.manager", ScreenArgs::PluginsManager)
            .expect("screen created");
        assert_eq!(s.id(), "plugins.manager");
    }

    #[test]
    fn create_screen_rejects_unknown_id() {
        let p = PluginsManagerPlugin::new();
        // `dyn Screen` lacks a Debug impl, so we can't `.unwrap_err()`.
        match p.create_screen("not-mine", ScreenArgs::None) {
            Ok(_) => panic!("expected ScreenNotFound, got Ok(_)"),
            Err(PluginError::ScreenNotFound(s)) => assert_eq!(s, "not-mine"),
            Err(other) => panic!("expected ScreenNotFound, got {other:?}"),
        }
    }

    #[test]
    fn summarize_contributions_lists_each_populated_field() {
        let mut c = Contributions::default();
        c.slash_commands = vec![savvagent_plugin::SlashSpec {
            name: "x".into(),
            summary: "".into(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        c.screens = vec![savvagent_plugin::ScreenSpec {
            id: "x".into(),
            layout: ScreenLayout::Fullscreen { hide_chrome: false },
        }];
        let s = summarize_contributions(&c);
        assert!(s.contains("1 slash"));
        assert!(s.contains("1 screen"));
    }

    #[test]
    fn summarize_contributions_handles_empty() {
        let s = summarize_contributions(&Contributions::default());
        assert_eq!(s, "");
    }

    /// Confirm the plural-form branches: two slashes + two screens + one
    /// theme. Pins the exact wording so a future refactor that drops
    /// the pluralization (e.g. "2 slash") is caught here, not via UI
    /// review. Singular forms are already covered by
    /// `summarize_contributions_lists_each_populated_field`.
    #[test]
    fn summarize_contributions_pluralizes_correctly() {
        let mut c = Contributions::default();
        c.slash_commands = vec![
            savvagent_plugin::SlashSpec {
                name: "a".into(),
                summary: "".into(),
                args_hint: None,
                requires_arg: false,
                suppress_prompt_segments: vec![],
            },
            savvagent_plugin::SlashSpec {
                name: "b".into(),
                summary: "".into(),
                args_hint: None,
                requires_arg: false,
                suppress_prompt_segments: vec![],
            },
        ];
        c.screens = vec![
            savvagent_plugin::ScreenSpec {
                id: "a".into(),
                layout: ScreenLayout::Fullscreen { hide_chrome: false },
            },
            savvagent_plugin::ScreenSpec {
                id: "b".into(),
                layout: ScreenLayout::Fullscreen { hide_chrome: false },
            },
        ];
        let s = summarize_contributions(&c);
        assert!(s.contains("2 slashes"), "got: {s}");
        assert!(s.contains("2 screens"), "got: {s}");
    }
}
