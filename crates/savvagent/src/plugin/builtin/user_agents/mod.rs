//! `internal:user-agents` — discovers user-defined agent definition
//! files and exposes them via an in-process `task` tool.
//! See `docs/superpowers/specs/2026-05-23-user-agents-design.md`.

pub mod body;
pub mod discovery;
pub mod frontmatter;
pub mod index;
pub mod spec;
pub mod task_tool;

use std::path::PathBuf;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
    PluginKind, SlashSpec, StyledLine,
};

pub use index::AgentIndex;
#[allow(unused_imports)] // Consumed by later tasks (16-20) once discovery is wired.
pub use spec::{AgentSpec, ToolsScope};

pub struct UserAgentsPlugin {
    project_root: PathBuf,
    home: PathBuf,
    index: AgentIndex,
}

impl UserAgentsPlugin {
    pub fn new() -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            project_root,
            home,
            index: AgentIndex::empty(),
        }
    }

    async fn register_task_tool_effects(&self) -> Vec<Effect> {
        if self.index.is_empty().await {
            return vec![];
        }
        let spec = task_tool::build_tool_def(&self.index).await;
        let handler = task_tool::handler_arc(self.index.clone());
        vec![Effect::RegisterInProcessTool { spec, handler }]
    }
}

impl Default for UserAgentsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for UserAgentsPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "reload-agents".into(),
            summary: "Rescan user-defined agents".into(),
            args_hint: None,
            requires_arg: false,
        }];
        contributions.hooks = vec![HookKind::HostStarting];
        Manifest {
            id: PluginId::new("internal:user-agents").expect("valid built-in id"),
            name: "User agents".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "User-defined subagents via the task tool".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        if name != "reload-agents" {
            return Ok(vec![]);
        }
        let entries = discovery::discover(&self.project_root, &self.home);
        self.index.replace(entries).await;
        let mut effs = self.register_task_tool_effects().await;
        effs.push(Effect::PushNote {
            line: StyledLine::plain(format!(
                "user-agents: reloaded ({} agent(s))",
                self.index.len().await
            )),
        });
        Ok(effs)
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        if matches!(event, HostEvent::HostStarting) {
            let entries = discovery::discover(&self.project_root, &self.home);
            self.index.replace(entries).await;
            return Ok(self.register_task_tool_effects().await);
        }
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_id_is_internal_user_agents() {
        let plugin = UserAgentsPlugin::new();
        assert_eq!(plugin.manifest().id.as_str(), "internal:user-agents");
    }

    #[test]
    fn manifest_contributes_reload_agents_slash() {
        let plugin = UserAgentsPlugin::new();
        let m = plugin.manifest();
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == "reload-agents")
        );
    }

    #[test]
    fn manifest_subscribes_to_host_starting() {
        let plugin = UserAgentsPlugin::new();
        let m = plugin.manifest();
        assert!(m.contributions.hooks.contains(&HookKind::HostStarting));
    }

    #[tokio::test]
    async fn task_tool_not_registered_when_index_empty() {
        let plugin = UserAgentsPlugin::new();
        let effects = plugin.register_task_tool_effects().await;
        assert!(
            effects.is_empty(),
            "no task tool until at least one agent discovered"
        );
    }
}
