//! `AgentIndex` — async-friendly shared map from agent name to spec.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::plugin::builtin::user_agents::spec::AgentSpec;

#[derive(Clone, Default)]
pub struct AgentIndex {
    inner: Arc<RwLock<HashMap<String, Arc<AgentSpec>>>>,
}

impl AgentIndex {
    pub fn empty() -> Self {
        Self::default()
    }

    pub async fn replace(&self, agents: Vec<AgentSpec>) {
        let map: HashMap<String, Arc<AgentSpec>> = agents
            .into_iter()
            .map(|spec| (spec.name.clone(), Arc::new(spec)))
            .collect();
        *self.inner.write().await = map;
    }

    pub async fn get(&self, name: &str) -> Option<Arc<AgentSpec>> {
        self.inner.read().await.get(name).cloned()
    }

    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }

    pub async fn names_snapshot(&self) -> Vec<String> {
        let mut names: Vec<String> = self.inner.read().await.keys().cloned().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::builtin::user_agents::spec::{AgentSpec, ToolsScope};

    fn agent(name: &str) -> AgentSpec {
        AgentSpec {
            name: name.into(),
            description: format!("{name} agent"),
            tools: ToolsScope::Inherit,
            model: None,
            body: format!("you are {name}"),
        }
    }

    #[tokio::test]
    async fn replace_makes_agents_visible() {
        let index = AgentIndex::empty();
        index.replace(vec![agent("a"), agent("b")]).await;
        assert_eq!(index.len().await, 2);
        let a = index.get("a").await.expect("agent a");
        assert_eq!(a.description, "a agent");
    }

    #[tokio::test]
    async fn names_snapshot_returns_sorted_list() {
        let index = AgentIndex::empty();
        index
            .replace(vec![agent("b"), agent("a"), agent("c")])
            .await;
        let names = index.names_snapshot().await;
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn empty_index_reports_is_empty() {
        let index = AgentIndex::empty();
        assert!(index.is_empty().await);
        assert_eq!(index.len().await, 0);
    }
}
