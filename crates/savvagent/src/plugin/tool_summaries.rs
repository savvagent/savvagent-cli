//! Tool-summary routing: resolves a tool name to its owning plugin and
//! dispatches `Plugin::summarize_tool_call` / `summarize_tool_result`.
//!
//! The caller (the TUI's `compute_home_frame_data`) falls back to
//! `savvagent_plugin::styled::json_spans` when the router returns `None`.

use savvagent_plugin::StyledSpan;

use crate::plugin::manifests::Indexes;
use crate::plugin::registry::PluginRegistry;

/// Routes a tool name to its owning plugin (via `Indexes::tool_summaries`)
/// and forwards summary requests. Asynchronous because plugins live behind
/// `Arc<tokio::sync::Mutex<dyn Plugin>>` in the registry.
use std::sync::Arc;

use tokio::sync::RwLock;

pub struct ToolSummaryRouter {
    /// Index over enabled-plugin manifests; provides the tool-name → PluginId map.
    pub indexes: Arc<RwLock<Indexes>>,
    /// In-memory registry of plugin instances.
    pub registry: Arc<RwLock<PluginRegistry>>,
}

impl ToolSummaryRouter {
    /// Construct a router from references to the current indexes and registry.
    pub fn new(indexes: Arc<RwLock<Indexes>>, registry: Arc<RwLock<PluginRegistry>>) -> Self {
        Self { indexes, registry }
    }

    /// Look up the owning plugin for `name` and call `summarize_tool_call`.
    /// Returns `None` if no plugin claims the name or the plugin returns
    /// `None` for these args.
    pub async fn summarize_call(
        &self,
        name: &str,
        args: &serde_json::Value,
    ) -> Option<Vec<StyledSpan>> {
        let indexes_guard = self.indexes.read().await;
        let registry_guard = self.registry.read().await;
        let pid = indexes_guard.tool_summaries.get(name)?;
        let handle = registry_guard.get(pid)?;
        // Non-blocking: this runs on the GUI paint thread via `build_model`.
        // A blocking acquire here can wedge the winit loop (see the matching
        // note in `slots::SlotRouter::render`). Skip the summary this frame if
        // the plugin is momentarily locked.
        let plugin = handle.try_lock().ok()?;
        plugin.summarize_tool_call(name, args)
    }

    /// Look up the owning plugin for `name` and call `summarize_tool_result`.
    /// Returns `None` if no plugin claims the name or the plugin returns
    /// `None` (e.g. parse failure on `result_text`).
    pub async fn summarize_result(&self, name: &str, result_text: &str) -> Option<Vec<StyledSpan>> {
        let indexes_guard = self.indexes.read().await;
        let registry_guard = self.registry.read().await;
        let pid = indexes_guard.tool_summaries.get(name)?;
        let handle = registry_guard.get(pid)?;
        // Non-blocking (see `summarize_call` / `slots::SlotRouter::render`).
        let plugin = handle.try_lock().ok()?;
        plugin.summarize_tool_result(name, result_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use savvagent_plugin::{
        Contributions, Manifest, Plugin, PluginId, PluginKind, StyledSpan, TextMods, ThemeColor,
        ToolSummarySpec,
    };

    struct StaticSummarizer {
        id: String,
        tool: String,
        call_text: String,
        result_text: String,
    }

    #[async_trait]
    impl Plugin for StaticSummarizer {
        fn manifest(&self) -> Manifest {
            let mut contributions = Contributions::default();
            contributions.tool_summaries = vec![ToolSummarySpec {
                tool_name: self.tool.clone(),
            }];
            Manifest {
                id: PluginId::new(&self.id).unwrap(),
                name: self.id.clone(),
                version: "0.10.0".into(),
                description: "test".into(),
                kind: PluginKind::Core,
                contributions,
            }
        }

        fn summarize_tool_call(
            &self,
            _name: &str,
            _args: &serde_json::Value,
        ) -> Option<Vec<StyledSpan>> {
            Some(vec![StyledSpan {
                text: self.call_text.clone(),
                fg: Some(ThemeColor::Fg),
                bg: None,
                modifiers: TextMods::default(),
            }])
        }

        fn summarize_tool_result(
            &self,
            _name: &str,
            _result_text: &str,
        ) -> Option<Vec<StyledSpan>> {
            Some(vec![StyledSpan {
                text: self.result_text.clone(),
                fg: Some(ThemeColor::Muted),
                bg: None,
                modifiers: TextMods::default(),
            }])
        }
    }

    #[tokio::test]
    async fn router_dispatches_to_claiming_plugin() {
        let reg = PluginRegistry::from_plugins(vec![Box::new(StaticSummarizer {
            id: "test:fs".into(),
            tool: "read_file".into(),
            call_text: "read_file src/main.rs".into(),
            result_text: "1.2 KiB".into(),
        })]);
        let idx = Indexes::build(&reg).await.unwrap();
        let router = ToolSummaryRouter::new(
            std::sync::Arc::new(tokio::sync::RwLock::new(idx)),
            std::sync::Arc::new(tokio::sync::RwLock::new(reg)),
        );

        let call = router
            .summarize_call("read_file", &serde_json::json!({"path": "src/main.rs"}))
            .await
            .expect("plugin should claim read_file");
        assert_eq!(call[0].text, "read_file src/main.rs");

        let result = router
            .summarize_result("read_file", r#"{"bytes": 1234}"#)
            .await
            .expect("plugin should claim read_file");
        assert_eq!(result[0].text, "1.2 KiB");
    }

    #[tokio::test]
    async fn router_returns_none_for_unclaimed_tool() {
        let reg = PluginRegistry::from_plugins(vec![]);
        let idx = Indexes::build(&reg).await.unwrap();
        let router = ToolSummaryRouter::new(
            std::sync::Arc::new(tokio::sync::RwLock::new(idx)),
            std::sync::Arc::new(tokio::sync::RwLock::new(reg)),
        );
        assert!(
            router
                .summarize_call("unknown_tool", &serde_json::json!({}))
                .await
                .is_none()
        );
        assert!(
            router
                .summarize_result("unknown_tool", "{}")
                .await
                .is_none()
        );
    }
}
