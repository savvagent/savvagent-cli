//! Slash command routing.

use std::sync::Arc;

use savvagent_plugin::{Effect, PluginError, PluginId};
use tokio::sync::RwLock;

use crate::plugin::manifests::Indexes;
use crate::plugin::registry::PluginRegistry;

/// Routes bare slash command names to the plugin that owns them and dispatches
/// the call. Re-entrancy depth enforcement is handled upstream in
/// `apply_effects` via `MAX_RUNSLASH_DEPTH`; `SlashRouter` is a pure
/// resolver + single-shot dispatcher.
pub struct SlashRouter {
    indexes: Arc<RwLock<Indexes>>,
    registry: Arc<RwLock<PluginRegistry>>,
}

/// Errors that can occur during slash command dispatch.
#[derive(Debug)]
pub enum SlashError {
    /// No enabled plugin has registered a slash command with this name.
    Unknown(String),
    /// The plugin's own `handle_slash` returned an error.
    Plugin(PluginError),
}

impl std::fmt::Display for SlashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlashError::Unknown(name) => write!(f, "unknown slash command: /{name}"),
            SlashError::Plugin(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SlashError {}

impl SlashRouter {
    /// Construct a router backed by the given index and registry snapshots.
    pub fn new(indexes: Arc<RwLock<Indexes>>, registry: Arc<RwLock<PluginRegistry>>) -> Self {
        Self { indexes, registry }
    }

    /// Look up which plugin owns `name` (without the leading `/`).
    /// Returns `None` if no enabled plugin has registered that command.
    pub async fn resolve(&self, name: &str) -> Option<PluginId> {
        let indexes_guard = self.indexes.read().await;
        indexes_guard.slash.get(name).cloned()
    }

    /// Dispatch a slash command by name. Locks the owning plugin, calls
    /// `handle_slash`, and returns the emitted effects.
    ///
    /// Returns [`SlashError::Unknown`] if no plugin owns `name`.
    /// Re-entrancy depth is enforced by the caller (`apply_effects`).
    pub async fn dispatch(&self, name: &str, args: Vec<String>) -> Result<Vec<Effect>, SlashError> {
        let pid = self
            .resolve(name)
            .await
            .ok_or_else(|| SlashError::Unknown(name.to_string()))?;

        let registry_guard = self.registry.read().await;
        let handle = registry_guard
            .get(&pid)
            .ok_or_else(|| SlashError::Unknown(name.to_string()))?;

        let result = {
            let mut plugin = handle.lock().await;
            plugin
                .handle_slash(name, args)
                .await
                .map_err(SlashError::Plugin)?
        };

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use savvagent_plugin::{Contributions, Manifest, Plugin, PluginKind, SlashSpec, StyledLine};

    struct Echo(String);

    #[async_trait]
    impl Plugin for Echo {
        fn manifest(&self) -> Manifest {
            let mut contributions = Contributions::default();
            contributions.slash_commands = vec![SlashSpec {
                name: "echo".into(),
                summary: "".into(),
                args_hint: None,
                requires_arg: false,
                suppress_prompt_segments: vec![],
            }];
            Manifest {
                id: PluginId::new(&self.0).expect("valid test id"),
                name: self.0.clone(),
                version: "0".into(),
                description: "".into(),
                kind: PluginKind::Optional,
                contributions,
            }
        }

        async fn handle_slash(
            &mut self,
            _: &str,
            args: Vec<String>,
        ) -> Result<Vec<Effect>, PluginError> {
            Ok(vec![Effect::PushNote {
                line: StyledLine::plain(args.join(" ")),
            }])
        }
    }

    #[tokio::test]
    async fn dispatch_routes_to_the_plugin() {
        let reg = PluginRegistry::from_plugins(vec![Box::new(Echo("test:p".into()))]);
        let idx = Indexes::build(&reg).await.unwrap();
        let r = SlashRouter::new(Arc::new(RwLock::new(idx)), Arc::new(RwLock::new(reg)));
        let out = r.dispatch("echo", vec!["hi".into()]).await.unwrap();
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn unknown_slash_yields_unknown_error() {
        let reg = PluginRegistry::from_plugins(vec![Box::new(Echo("test:p".into()))]);
        let idx = Indexes::build(&reg).await.unwrap();
        let r = SlashRouter::new(Arc::new(RwLock::new(idx)), Arc::new(RwLock::new(reg)));
        let err = r.dispatch("nope", vec![]).await.unwrap_err();
        assert!(matches!(err, SlashError::Unknown(ref n) if n == "nope"));
    }
}
