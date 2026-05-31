//! Slot routing: given a slot_id and region, query each enabled
//! contributor (in priority order) and concatenate their styled lines.

use savvagent_plugin::{PluginId, Region, StyledLine};

use crate::plugin::manifests::Indexes;
use crate::plugin::registry::PluginRegistry;

/// Routes a slot_id to its ordered list of contributing plugins and
/// assembles their rendered output. Holds shared references to the
/// `Indexes` and `PluginRegistry` built at startup.
pub struct SlotRouter<'a> {
    /// Derived indexes over enabled-plugin manifests; provides the
    /// priority-sorted contributor list for each slot id.
    pub indexes: &'a Indexes,
    /// In-memory registry of all registered plugin instances.
    pub registry: &'a PluginRegistry,
}

impl<'a> SlotRouter<'a> {
    /// Construct a `SlotRouter` from references to the current indexes and
    /// registry. Both references must outlive the router.
    pub fn new(indexes: &'a Indexes, registry: &'a PluginRegistry) -> Self {
        Self { indexes, registry }
    }

    /// Return the `PluginId`s of all enabled contributors for `slot_id`,
    /// in ascending priority order (lowest priority value first).
    pub fn contributors(&self, slot_id: &str) -> Vec<&'a PluginId> {
        self.indexes
            .slots
            .get(slot_id)
            .map(|v| v.iter().map(|(_, id)| id).collect())
            .unwrap_or_default()
    }

    /// Asynchronously gather a slot's rendered lines. Locks each
    /// contributor plugin briefly to call `render_slot`.
    pub async fn render(&self, slot_id: &str, region: Region) -> Vec<StyledLine> {
        let mut out = Vec::new();
        for pid in self.contributors(slot_id) {
            let Some(handle) = self.registry.get(pid) else {
                tracing::error!(
                    plugin_id = %pid.as_str(),
                    slot_id,
                    "contributor in slot index but missing from registry — index/registry divergence"
                );
                continue;
            };
            // Non-blocking acquire: the slot render runs on the GUI's
            // paint thread (via `build_model`), and a blocking `.lock().await`
            // here can deadlock the winit event loop. The render path holds
            // the registry read-lock while acquiring this plugin `Mutex`; a
            // task holding the plugin lock and waiting on `registry.write()`
            // (tokio `RwLock` is write-preferring) would otherwise wedge the
            // whole window. If the plugin is momentarily locked, skip it this
            // frame — it renders on the next one. Mirrors the deliberate
            // `try_lock` in `self_update::render_slot`.
            let Ok(plugin) = handle.try_lock() else {
                tracing::trace!(
                    plugin_id = %pid.as_str(),
                    slot_id,
                    "slot contributor busy; skipping for this frame"
                );
                continue;
            };
            out.extend(plugin.render_slot(slot_id, region));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use savvagent_plugin::{Contributions, Manifest, Plugin, PluginKind, SlotSpec, StyledLine};

    struct Stub {
        id: String,
        slot: String,
        line: String,
        priority: i32,
    }

    #[async_trait]
    impl Plugin for Stub {
        fn manifest(&self) -> Manifest {
            let mut c = Contributions::default();
            c.slots = vec![SlotSpec {
                slot_id: self.slot.clone(),
                priority: self.priority,
            }];
            Manifest {
                id: PluginId::new(&self.id).expect("valid test id"),
                name: self.id.clone(),
                version: "0".into(),
                description: "".into(),
                kind: PluginKind::Core,
                contributions: c,
            }
        }

        fn render_slot(&self, _: &str, _: Region) -> Vec<StyledLine> {
            vec![StyledLine::plain(self.line.clone())]
        }
    }

    #[tokio::test]
    async fn router_returns_contributors_in_priority_order() {
        let reg = PluginRegistry::from_plugins(vec![
            Box::new(Stub {
                id: "test:z".into(),
                slot: "home.tips".into(),
                line: "Z".into(),
                priority: 500,
            }),
            Box::new(Stub {
                id: "test:a".into(),
                slot: "home.tips".into(),
                line: "A".into(),
                priority: 100,
            }),
        ]);
        let idx = Indexes::build(&reg).await.unwrap();
        let router = SlotRouter::new(&idx, &reg);

        let lines = router
            .render(
                "home.tips",
                Region {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 1,
                },
            )
            .await;
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].text, "A");
        assert_eq!(lines[1].spans[0].text, "Z");
    }
}
