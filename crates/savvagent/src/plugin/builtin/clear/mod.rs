//! `internal:clear` — clears the conversation log.

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, SlashSpec,
};

/// Plugin that registers the `/clear` slash command.
///
/// `/clear` emits [`Effect::ClearLog`], which `apply_effects` in PR 3
/// maps to [`App::clear_log`].
pub struct ClearPlugin;

impl ClearPlugin {
    /// Construct a new [`ClearPlugin`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClearPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ClearPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "clear".into(),
            summary: rust_i18n::t!("slash.clear-summary").to_string(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        Manifest {
            id: PluginId::new("internal:clear").expect("valid built-in id"),
            name: "Clear log".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.clear-description").to_string(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(&mut self, _: &str, _: Vec<String>) -> Result<Vec<Effect>, PluginError> {
        Ok(vec![Effect::ClearLog])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clear_returns_clear_log_effect() {
        let mut p = ClearPlugin::new();
        let effs = p.handle_slash("clear", vec![]).await.unwrap();
        assert_eq!(effs.len(), 1);
        assert!(matches!(effs[0], Effect::ClearLog));
    }
}
