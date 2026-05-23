//! In-memory registry of constructed plugin instances + enabled-set.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use savvagent_mcp::ProviderClient;
use savvagent_plugin::{Plugin, PluginId};
use tokio::sync::Mutex;

use crate::plugin::builtin::provider_common::{
    BuiltinHookPlugin, BuiltinProviderPlugin, ProviderEntry,
};

/// One hook-plugin entry. The dual-Arc pattern mirrors [`ProviderEntry`]
/// so both the `Plugin` view (for `register_builtins`' plugin list) and
/// the `BuiltinHookPlugin` view (for the `RegisterPreToolGate` effect
/// apply path) share the same instance.
pub(crate) struct HookEntry {
    /// Plugin-trait view used in the registry's slash/render/hook dispatch.
    pub as_plugin: Arc<Mutex<dyn Plugin>>,
    /// Hook-trait view used by the `RegisterPreToolGate` effect handler
    /// to call `take_pre_tool_gate`.
    pub as_hook: Arc<Mutex<dyn BuiltinHookPlugin>>,
    /// Plugin id (cached from `manifest()`).
    pub id: PluginId,
}

impl HookEntry {
    /// Build a [`HookEntry`] from a concrete hook-plugin type. The two
    /// `Arc`s point at the same allocation; mutations via either view
    /// are observed by the other.
    pub fn new<T>(concrete: T) -> Self
    where
        T: BuiltinHookPlugin + 'static,
    {
        let arc: Arc<Mutex<T>> = Arc::new(Mutex::new(concrete));
        let id = {
            let g = arc.try_lock().expect("constructor not contended");
            g.manifest().id.clone()
        };
        let as_plugin: Arc<Mutex<dyn Plugin>> = arc.clone();
        let as_hook: Arc<Mutex<dyn BuiltinHookPlugin>> = arc;
        Self {
            as_plugin,
            as_hook,
            id,
        }
    }
}

/// Plugin instances + provider-plugin parallel map handed to
/// [`PluginRegistry::new`].
///
/// Returned by [`crate::plugin::register_builtins`]. Non-provider plugins
/// live in `plugins`; provider plugins live in `providers` as
/// [`ProviderEntry`] values whose two trait-object Arcs share the same
/// `Arc<Mutex<T>>`. The registry inserts the provider-plugin view into
/// the same id-keyed plugins map, so slash/render/hook dispatch and
/// `take_provider_client` operate on **one** instance per plugin.
#[derive(Default)]
pub(crate) struct BuiltinSet {
    /// Non-provider plugins (those that don't implement
    /// [`BuiltinProviderPlugin`]).
    pub plugins: Vec<Box<dyn Plugin>>,
    /// Provider plugins with their dual trait-object views.
    pub providers: Vec<ProviderEntry>,
    /// Hook-plugin entries (parallel to `providers`). Each entry's
    /// `as_plugin` view also appears in the `plugins` Vec; the dual-Arc
    /// pattern ensures they reference the same instance.
    ///
    /// Populated by B-T20/B-T21; initially empty.
    pub hook_entries: Vec<HookEntry>,
}

/// Stores plugin instances behind `Arc<Mutex<dyn Plugin>>` keyed by
/// `PluginId`; tracks an enabled-set. Provider plugins are additionally
/// indexed in a parallel map keyed by [`PluginId`] so the runtime can call
/// [`BuiltinProviderPlugin::take_client`] without trait-object downcasting.
/// Both maps reference the **same** underlying `Arc<Mutex<T>>` for each
/// provider plugin via [`ProviderEntry`], so mutations made through one
/// view are visible to the other.
///
/// Hook plugins are indexed by a third parallel map keyed by [`PluginId`]
/// so the `RegisterPreToolGate` effect handler can call
/// `take_pre_tool_gate` without downcasting.
pub struct PluginRegistry {
    plugins: HashMap<PluginId, Arc<Mutex<dyn Plugin>>>,
    providers: HashMap<PluginId, Arc<Mutex<dyn BuiltinProviderPlugin>>>,
    hooks: HashMap<PluginId, Arc<Mutex<dyn BuiltinHookPlugin>>>,
    enabled: HashSet<PluginId>,
}

impl PluginRegistry {
    /// Test-only convenience: construct from a vector of plugins (no
    /// provider-plugin parallel map). Production code uses
    /// [`PluginRegistry::new`] with the [`BuiltinSet`] returned by
    /// [`crate::plugin::register_builtins`].
    #[cfg(test)]
    pub fn from_plugins(plugins: Vec<Box<dyn Plugin>>) -> Self {
        Self::new(BuiltinSet {
            plugins,
            providers: vec![],
            hook_entries: vec![],
        })
    }

    /// Construct from a [`BuiltinSet`]. Every plugin is inserted into the
    /// registry and added to the enabled set; every provider plugin is
    /// additionally indexed in the parallel provider map; every hook plugin
    /// is additionally indexed in the parallel hooks map. The enabled
    /// set is rewound at startup by reading `plugins.toml` (PR 8).
    pub(crate) fn new(set: BuiltinSet) -> Self {
        let BuiltinSet {
            plugins,
            providers,
            hook_entries,
        } = set;
        let mut map: HashMap<PluginId, Arc<Mutex<dyn Plugin>>> =
            HashMap::with_capacity(plugins.len() + providers.len() + hook_entries.len());
        let mut enabled =
            HashSet::with_capacity(plugins.len() + providers.len() + hook_entries.len());
        for p in plugins {
            let id = p.manifest().id.clone();
            enabled.insert(id.clone());
            // Wrap each non-provider Box into an Arc<Mutex<dyn Plugin>>.
            let handle: Arc<Mutex<dyn Plugin>> = Arc::new(Mutex::new(BoxedPlugin(p)));
            map.insert(id, handle);
        }
        let mut provider_map: HashMap<PluginId, Arc<Mutex<dyn BuiltinProviderPlugin>>> =
            HashMap::with_capacity(providers.len());
        for entry in providers {
            // Borrow the plugin briefly to read its id from the provider-view
            // (which gives us the same data as the plugin-view).
            //
            // We use `try_lock` synchronously here because the just-constructed
            // Arc has no other holders — uncontended by construction. If this
            // ever fires, it indicates a use-after-construct bug in `register_builtins`.
            let id = {
                let guard = entry
                    .as_provider
                    .try_lock()
                    .expect("freshly-constructed ProviderEntry is uncontended");
                guard.manifest().id.clone()
            };
            enabled.insert(id.clone());
            map.insert(id.clone(), entry.as_plugin.clone());
            provider_map.insert(id, entry.as_provider);
        }
        let mut hook_map: HashMap<PluginId, Arc<Mutex<dyn BuiltinHookPlugin>>> =
            HashMap::with_capacity(hook_entries.len());
        for entry in hook_entries {
            // `entry.id` was cached at construction time (uncontended try_lock
            // in HookEntry::new); no need to re-lock here.
            enabled.insert(entry.id.clone());
            map.insert(entry.id.clone(), entry.as_plugin.clone());
            hook_map.insert(entry.id, entry.as_hook);
        }
        Self {
            plugins: map,
            providers: provider_map,
            hooks: hook_map,
            enabled,
        }
    }

    /// Returns the total number of registered plugins (enabled and disabled).
    #[allow(dead_code)] // used by the plugins-manager screen in PR 8
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Returns the number of registered provider plugins.
    #[cfg(test)]
    pub(crate) fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// Returns `true` if no plugins are registered.
    #[allow(dead_code)] // satisfies the is_empty/len lint pair; used in PR 8
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Iterates over the [`PluginId`]s of every currently-enabled plugin.
    pub fn enabled_ids(&self) -> impl Iterator<Item = &PluginId> {
        self.plugins.keys().filter(|id| self.enabled.contains(id))
    }

    /// Iterates over every registered [`PluginId`] regardless of enabled state.
    /// Used by the plugins-manager screen to populate its row list and by
    /// `apply_effects::Effect::TogglePlugin` when collecting Optional ids for
    /// persistence.
    pub fn all_ids(&self) -> impl Iterator<Item = &PluginId> {
        self.plugins.keys()
    }

    /// Returns the shared plugin handle for `id`, or `None` if not registered.
    pub fn get(&self, id: &PluginId) -> Option<Arc<Mutex<dyn Plugin>>> {
        self.plugins.get(id).cloned()
    }

    /// Returns `true` if `id` is in the enabled set.
    #[allow(dead_code)] // used by the plugins-manager screen in PR 8
    pub fn is_enabled(&self, id: &PluginId) -> bool {
        self.enabled.contains(id)
    }

    /// Adds or removes `id` from the enabled set. Enabling an unregistered id
    /// is a no-op at query time but is otherwise stored; callers should only
    /// enable ids that are registered.
    #[allow(dead_code)] // used by the plugins-manager screen in PR 8
    pub fn set_enabled(&mut self, id: &PluginId, enabled: bool) {
        if enabled {
            self.enabled.insert(id.clone());
        } else {
            self.enabled.remove(id);
        }
    }

    /// Take the constructed provider client out of the provider plugin
    /// associated with `id`, leaving the plugin's slot empty. Returns
    /// `None` if `id` is unknown or the plugin doesn't currently hold a
    /// client. Called by [`crate::plugin::effects::apply_effects`] after
    /// observing [`savvagent_plugin::Effect::RegisterProvider`].
    ///
    /// Because [`ProviderEntry`] gives the providers and plugins maps a
    /// shared `Arc<Mutex<T>>` per provider plugin, this method observes
    /// the same state that `handle_slash`/`on_event` (called via the
    /// plugins map) mutated.
    pub async fn take_provider_client(&self, id: &PluginId) -> Option<Box<dyn ProviderClient>> {
        let handle = self.providers.get(id)?.clone();
        let mut guard = handle.lock().await;
        guard.take_client()
    }

    /// Returns the hook-plugin handle for `id`, or `None` if no hook plugin
    /// with that id is registered. Called by the `RegisterPreToolGate` effect
    /// handler to retrieve the `Arc<Mutex<dyn BuiltinHookPlugin>>` so it can
    /// call `take_pre_tool_gate`.
    pub fn get_hook(&self, id: &PluginId) -> Option<Arc<Mutex<dyn BuiltinHookPlugin>>> {
        self.hooks.get(id).cloned()
    }

    /// Returns the number of registered hook plugins.
    #[cfg(test)]
    pub(crate) fn hook_count(&self) -> usize {
        self.hooks.len()
    }
}

/// Adapter that lets a `Box<dyn Plugin>` participate in the registry's
/// `Arc<Mutex<dyn Plugin>>` storage. This adapter exists only because
/// non-provider plugins arrive as boxed trait objects; provider plugins
/// arrive pre-built via [`ProviderEntry`] which sidesteps this layer.
struct BoxedPlugin(Box<dyn Plugin>);

#[async_trait::async_trait]
impl Plugin for BoxedPlugin {
    fn manifest(&self) -> savvagent_plugin::Manifest {
        self.0.manifest()
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<savvagent_plugin::Effect>, savvagent_plugin::PluginError> {
        self.0.handle_slash(name, args).await
    }

    async fn on_event(
        &mut self,
        event: savvagent_plugin::HostEvent,
    ) -> Result<Vec<savvagent_plugin::Effect>, savvagent_plugin::PluginError> {
        self.0.on_event(event).await
    }

    fn render_slot(
        &self,
        slot_id: &str,
        region: savvagent_plugin::Region,
    ) -> Vec<savvagent_plugin::StyledLine> {
        self.0.render_slot(slot_id, region)
    }

    fn create_screen(
        &self,
        id: &str,
        args: savvagent_plugin::ScreenArgs,
    ) -> Result<Box<dyn savvagent_plugin::Screen>, savvagent_plugin::PluginError> {
        self.0.create_screen(id, args)
    }

    fn summarize_tool_call(
        &self,
        name: &str,
        args: &serde_json::Value,
    ) -> Option<Vec<savvagent_plugin::StyledSpan>> {
        self.0.summarize_tool_call(name, args)
    }

    fn summarize_tool_result(
        &self,
        name: &str,
        result_text: &str,
    ) -> Option<Vec<savvagent_plugin::StyledSpan>> {
        self.0.summarize_tool_result(name, result_text)
    }

    fn themes(&self) -> Vec<savvagent_plugin::ThemeEntry> {
        self.0.themes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use savvagent_plugin::{Contributions, Manifest, PluginKind};

    /// Test plugin that takes a vendor-prefixed id and produces a Manifest
    /// using PluginId::new() (validated).
    struct Empty(String);

    #[async_trait]
    impl Plugin for Empty {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: PluginId::new(&self.0).expect("test ids must be valid"),
                name: self.0.clone(),
                version: "0.0.0".into(),
                description: "test".into(),
                kind: PluginKind::Optional,
                contributions: Contributions::default(),
            }
        }
    }

    #[test]
    fn registry_indexes_by_id() {
        let reg = PluginRegistry::from_plugins(vec![
            Box::new(Empty("test:a".into())),
            Box::new(Empty("test:b".into())),
        ]);
        assert_eq!(reg.len(), 2);
        let id_a = PluginId::new("test:a").unwrap();
        let id_b = PluginId::new("test:b").unwrap();
        assert!(reg.is_enabled(&id_a));
        assert!(reg.is_enabled(&id_b));
    }

    #[test]
    fn disable_removes_from_enabled_set() {
        let mut reg = PluginRegistry::from_plugins(vec![Box::new(Empty("test:x".into()))]);
        let id = PluginId::new("test:x").unwrap();
        assert!(reg.is_enabled(&id));
        reg.set_enabled(&id, false);
        assert!(!reg.is_enabled(&id));
    }
}

#[cfg(test)]
mod hook_entry_tests {
    use super::*;
    use async_trait::async_trait;
    use savvagent_plugin::{Contributions, Manifest, PluginId, PluginKind};

    struct StubHookPlugin;

    #[async_trait]
    impl Plugin for StubHookPlugin {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: PluginId::new("internal:hook-stub").unwrap(),
                name: "stub".into(),
                version: "0".into(),
                description: "stub".into(),
                kind: PluginKind::Core,
                contributions: Contributions::default(),
            }
        }
    }

    impl BuiltinHookPlugin for StubHookPlugin {
        fn take_pre_tool_gate(
            &mut self,
        ) -> Option<std::sync::Arc<dyn savvagent_host::PreToolUseGate>> {
            None
        }
    }

    #[test]
    fn hook_entry_dual_arc_shares_allocation() {
        let entry = HookEntry::new(StubHookPlugin);
        // Both Arcs should be the same strong count after construction.
        // (We hold two clones of the same Arc.)
        let p_count = std::sync::Arc::strong_count(&entry.as_plugin);
        let h_count = std::sync::Arc::strong_count(&entry.as_hook);
        assert_eq!(p_count, h_count);
        assert_eq!(p_count, 2);
        assert_eq!(entry.id.as_str(), "internal:hook-stub");
    }

    #[test]
    fn registry_indexes_hook_entry() {
        let entry = HookEntry::new(StubHookPlugin);
        let id = entry.id.clone();
        let set = BuiltinSet {
            plugins: vec![],
            providers: vec![],
            hook_entries: vec![entry],
        };
        let reg = PluginRegistry::new(set);
        assert_eq!(reg.hook_count(), 1);
        assert!(reg.get_hook(&id).is_some(), "hook must resolve by id");
        // The plugin-view is also in the main dispatch map.
        assert!(reg.get(&id).is_some(), "hook must appear in plugins map");
        assert!(reg.is_enabled(&id), "hook must start enabled");
    }
}
