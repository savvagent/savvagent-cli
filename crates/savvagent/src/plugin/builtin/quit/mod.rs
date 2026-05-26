//! `internal:quit` — shuts down the application cleanly.

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, SlashSpec,
};

/// Plugin that registers the `/quit` slash command.
///
/// `/quit` emits [`Effect::Quit`], which `apply_effects` maps to
/// [`crate::app::App::request_quit`] (setting `should_quit = true` so the
/// event loop exits on its next tick).
///
/// Registered as [`PluginKind::Core`] so the plugins-manager screen
/// refuses to disable it — disabling `/quit` would leave the user with no
/// in-band way to leave the TUI from the command palette.
pub struct QuitPlugin;

impl QuitPlugin {
    /// Construct a new [`QuitPlugin`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for QuitPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for QuitPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "quit".into(),
            summary: rust_i18n::t!("slash.quit-summary").to_string(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        Manifest {
            id: PluginId::new("internal:quit").expect("valid built-in id"),
            name: "Quit".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.quit-description").to_string(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(&mut self, _: &str, _: Vec<String>) -> Result<Vec<Effect>, PluginError> {
        Ok(vec![Effect::Quit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/quit` returns exactly one [`Effect::Quit`] — regression test for
    /// the post-v0.9 hotfix where `/quit` was missing from the plugin
    /// surface entirely and `Effect::RunSlash { name: "quit", .. }` from
    /// the palette hit `SlashError::Unknown`.
    #[tokio::test]
    async fn quit_returns_quit_effect() {
        let mut p = QuitPlugin::new();
        let effs = p.handle_slash("quit", vec![]).await.unwrap();
        assert_eq!(effs.len(), 1);
        assert!(matches!(effs[0], Effect::Quit));
    }

    #[tokio::test]
    async fn manifest_marks_quit_as_core() {
        let p = QuitPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:quit");
        assert!(matches!(m.kind, PluginKind::Core));
        assert_eq!(m.contributions.slash_commands.len(), 1);
        assert_eq!(m.contributions.slash_commands[0].name, "quit");
    }
}
