//! Indexes derived from each enabled plugin's manifest. Built once at
//! startup, rebuilt on enable/disable. The five indexes:
//! slash, slots, hooks, keybindings, screens.

use std::collections::HashMap;

use savvagent_plugin::{
    BoundAction, ChordPortable, HookKind, KeyScope, KeybindingSpec, PluginId, ScreenSpec,
    SlashSpec, SlotSpec, ToolSummarySpec,
};

use crate::plugin::registry::PluginRegistry;

/// All five runtime indexes derived from the manifests of currently-enabled
/// plugins. Built once at startup and rebuilt whenever a plugin is
/// enabled or disabled.
#[derive(Debug, Default)]
pub struct Indexes {
    /// Maps slash command name (without leading `/`) to the plugin that owns it.
    pub slash: HashMap<String, PluginId>,
    /// Maps slot id to contributing plugins. Sorted ascending by priority within each slot_id.
    pub slots: HashMap<String, Vec<(i32, PluginId)>>,
    /// Maps hook kind to the ordered list of plugins subscribed to it.
    pub hooks: HashMap<HookKind, Vec<PluginId>>,
    /// Maps `(scope, chord)` pair to the `(plugin, action)` that handles it.
    pub keybindings: HashMap<(KeyScope, ChordPortable), (PluginId, BoundAction)>,
    /// Maps screen id to the plugin that provides it.
    pub screens: HashMap<String, PluginId>,
    /// Maps tool name to the plugin that renders summaries for it.
    /// First-registered wins on duplicate; conflicts log a `warn!`.
    pub tool_summaries: HashMap<String, PluginId>,
}

/// Returned when two enabled plugins register the same slash command, screen
/// id, or keybinding chord+scope combination, which would be unresolvable at
/// dispatch time.
#[derive(Debug)]
#[allow(clippy::enum_variant_names)] // all variants describe conflicts; the postfix is load-bearing
pub enum IndexBuildError {
    /// Two enabled plugins register the same slash command name.
    SlashConflict {
        name: String,
        a: PluginId,
        b: PluginId,
    },
    /// Two enabled plugins declare the same screen id.
    ScreenConflict {
        id: String,
        a: PluginId,
        b: PluginId,
    },
    /// Two enabled plugins bind the same chord in the same scope.
    KeybindingConflict {
        chord: ChordPortable,
        scope: KeyScope,
        a: PluginId,
        b: PluginId,
    },
}

impl std::fmt::Display for IndexBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexBuildError::SlashConflict { name, a, b } => {
                write!(
                    f,
                    "two enabled plugins register /{name}: {} and {}",
                    a.as_str(),
                    b.as_str()
                )
            }
            IndexBuildError::ScreenConflict { id, a, b } => {
                write!(
                    f,
                    "two enabled plugins declare screen {id}: {} and {}",
                    a.as_str(),
                    b.as_str()
                )
            }
            IndexBuildError::KeybindingConflict { chord, scope, a, b } => {
                write!(
                    f,
                    "two enabled plugins bind chord {:?} in scope {:?}: {} and {}",
                    chord,
                    scope,
                    a.as_str(),
                    b.as_str()
                )
            }
        }
    }
}

impl std::error::Error for IndexBuildError {}

impl Indexes {
    /// Build all five indexes from the currently-enabled plugins. Theme
    /// catalog conflicts emit a warning but do not fail. Slash/screen/
    /// keybinding conflicts are hard errors per the spec.
    pub async fn build(reg: &PluginRegistry) -> Result<Self, IndexBuildError> {
        let mut idx = Indexes::default();

        for id in reg.enabled_ids().cloned().collect::<Vec<_>>() {
            let Some(handle) = reg.get(&id) else {
                tracing::error!(
                    plugin_id = %id.as_str(),
                    "enabled plugin id not present in registry — index/registry divergence at build time"
                );
                continue;
            };
            let manifest = {
                let plugin = handle.lock().await;
                plugin.manifest()
            };
            let pid = manifest.id.clone();

            for s in manifest.contributions.slash_commands {
                Self::insert_slash(&mut idx, s, &pid)?;
            }
            for s in manifest.contributions.screens {
                Self::insert_screen(&mut idx, s, &pid)?;
            }
            for s in manifest.contributions.slots {
                Self::insert_slot(&mut idx, s, &pid);
            }
            for h in manifest.contributions.hooks {
                idx.hooks.entry(h).or_default().push(pid.clone());
            }
            for k in manifest.contributions.keybindings {
                Self::insert_keybinding(&mut idx, k, &pid)?;
            }
            for s in manifest.contributions.tool_summaries {
                Self::insert_tool_summary(&mut idx, s, &pid);
            }
        }

        // Sort each slot's contributors by priority ascending.
        for v in idx.slots.values_mut() {
            v.sort_by_key(|(p, _)| *p);
        }

        Ok(idx)
    }

    fn insert_slash(
        idx: &mut Indexes,
        s: SlashSpec,
        pid: &PluginId,
    ) -> Result<(), IndexBuildError> {
        if let Some(existing) = idx.slash.get(&s.name) {
            return Err(IndexBuildError::SlashConflict {
                name: s.name,
                a: existing.clone(),
                b: pid.clone(),
            });
        }
        idx.slash.insert(s.name, pid.clone());
        Ok(())
    }

    fn insert_screen(
        idx: &mut Indexes,
        s: ScreenSpec,
        pid: &PluginId,
    ) -> Result<(), IndexBuildError> {
        if let Some(existing) = idx.screens.get(&s.id) {
            return Err(IndexBuildError::ScreenConflict {
                id: s.id,
                a: existing.clone(),
                b: pid.clone(),
            });
        }
        idx.screens.insert(s.id, pid.clone());
        Ok(())
    }

    fn insert_slot(idx: &mut Indexes, s: SlotSpec, pid: &PluginId) {
        idx.slots
            .entry(s.slot_id)
            .or_default()
            .push((s.priority, pid.clone()));
    }

    fn insert_keybinding(
        idx: &mut Indexes,
        k: KeybindingSpec,
        pid: &PluginId,
    ) -> Result<(), IndexBuildError> {
        let key = (k.scope.clone(), k.chord.clone());
        if let Some((existing, _)) = idx.keybindings.get(&key) {
            return Err(IndexBuildError::KeybindingConflict {
                chord: k.chord,
                scope: k.scope,
                a: existing.clone(),
                b: pid.clone(),
            });
        }
        idx.keybindings.insert(key, (pid.clone(), k.action));
        Ok(())
    }

    fn insert_tool_summary(idx: &mut Indexes, spec: ToolSummarySpec, pid: &PluginId) {
        let ToolSummarySpec { tool_name } = spec;
        if let Some(existing) = idx.tool_summaries.get(&tool_name) {
            tracing::warn!(
                tool_name = %tool_name,
                first = %existing.as_str(),
                second = %pid.as_str(),
                "two enabled plugins claim summaries for the same tool name; first-registered wins"
            );
            return;
        }
        idx.tool_summaries.insert(tool_name, pid.clone());
    }

    /// Re-read `plugin_id`'s manifest and rebuild every derived index entry it
    /// owns. All other plugins' contributions are untouched. Used by
    /// `/reload-commands` (via `Effect::ReindexPlugin`).
    ///
    /// The method is infallible by design: if the refreshed manifest would
    /// introduce a slash/screen/keybinding conflict with another plugin's
    /// already-registered contribution, the conflicting new entry is skipped
    /// and logged at `warn!` level rather than returning an error. This keeps
    /// the indexes coherent even when a user edits a command file to use a
    /// taken name — `/reload-commands` will note the conflict and leave the
    /// existing winner's entry in place.
    pub async fn reindex_plugin(
        &mut self,
        plugin_id: &savvagent_plugin::PluginId,
        registry: &crate::plugin::registry::PluginRegistry,
    ) {
        // Step 1: remove all entries currently owned by plugin_id.
        self.slash.retain(|_, owner| owner != plugin_id);
        self.screens.retain(|_, owner| owner != plugin_id);
        self.tool_summaries.retain(|_, owner| owner != plugin_id);

        // hooks: retain entries in each vec that don't belong to plugin_id;
        // drop the whole key if the vec becomes empty.
        self.hooks.retain(|_, subscribers| {
            subscribers.retain(|id| id != plugin_id);
            !subscribers.is_empty()
        });

        // slots: retain slot entries whose owner != plugin_id; drop empty slot keys.
        self.slots.retain(|_, contributors| {
            contributors.retain(|(_, id)| id != plugin_id);
            !contributors.is_empty()
        });

        // keybindings: retain entries whose owning plugin != plugin_id.
        self.keybindings
            .retain(|_, (owner, _action)| owner != plugin_id);

        // Step 2: re-insert from the plugin's fresh manifest.
        let Some(handle) = registry.get(plugin_id) else {
            tracing::warn!(
                plugin_id = %plugin_id.as_str(),
                "reindex_plugin: plugin_id not present in registry; removal completed but no re-insert"
            );
            return;
        };
        let manifest = {
            let plugin = handle.lock().await;
            plugin.manifest()
        };
        let pid = manifest.id.clone();

        for s in manifest.contributions.slash_commands {
            if let Err(e) = Self::insert_slash(self, s, &pid) {
                tracing::warn!(error = %e, "reindex_plugin: slash conflict skipped");
            }
        }
        for s in manifest.contributions.screens {
            if let Err(e) = Self::insert_screen(self, s, &pid) {
                tracing::warn!(error = %e, "reindex_plugin: screen conflict skipped");
            }
        }
        for s in manifest.contributions.slots {
            Self::insert_slot(self, s, &pid);
        }
        for h in manifest.contributions.hooks {
            self.hooks.entry(h).or_default().push(pid.clone());
        }
        for k in manifest.contributions.keybindings {
            if let Err(e) = Self::insert_keybinding(self, k, &pid) {
                tracing::warn!(error = %e, "reindex_plugin: keybinding conflict skipped");
            }
        }
        for s in manifest.contributions.tool_summaries {
            Self::insert_tool_summary(self, s, &pid);
        }

        // Re-sort each slot's contributors by priority ascending (mirrors build).
        for v in self.slots.values_mut() {
            v.sort_by_key(|(p, _)| *p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use savvagent_plugin::{Contributions, Manifest, Plugin, PluginKind};

    /// Test plugin with optionally a slash and/or slot contribution.
    struct WithSlash(String, String);

    #[async_trait]
    impl Plugin for WithSlash {
        fn manifest(&self) -> Manifest {
            let mut contributions = Contributions::default();
            contributions.slash_commands = vec![SlashSpec {
                name: self.1.clone(),
                summary: "".into(),
                args_hint: None,
                requires_arg: false,
            }];
            Manifest {
                id: PluginId::new(&self.0).expect("valid test id"),
                name: self.0.clone(),
                version: "0".into(),
                description: "t".into(),
                kind: PluginKind::Optional,
                contributions,
            }
        }
    }

    #[tokio::test]
    async fn slash_conflict_is_hard_error() {
        let reg = PluginRegistry::from_plugins(vec![
            Box::new(WithSlash("test:a".into(), "theme".into())),
            Box::new(WithSlash("test:b".into(), "theme".into())),
        ]);
        let err = Indexes::build(&reg).await.unwrap_err();
        assert!(matches!(err, IndexBuildError::SlashConflict { ref name, .. } if name == "theme"));
    }

    struct WithSlots(String, Vec<(String, i32)>);

    #[async_trait]
    impl Plugin for WithSlots {
        fn manifest(&self) -> Manifest {
            let mut contributions = Contributions::default();
            contributions.slots = self
                .1
                .iter()
                .map(|(id, p)| SlotSpec {
                    slot_id: id.clone(),
                    priority: *p,
                })
                .collect();
            Manifest {
                id: PluginId::new(&self.0).expect("valid test id"),
                name: self.0.clone(),
                version: "0".into(),
                description: "".into(),
                kind: PluginKind::Core,
                contributions,
            }
        }
    }

    struct WithScreen(String, String);

    #[async_trait]
    impl Plugin for WithScreen {
        fn manifest(&self) -> Manifest {
            use savvagent_plugin::ScreenLayout;
            let mut contributions = Contributions::default();
            contributions.screens = vec![ScreenSpec {
                id: self.1.clone(),
                layout: ScreenLayout::Fullscreen { hide_chrome: false },
            }];
            Manifest {
                id: PluginId::new(&self.0).expect("valid test id"),
                name: self.0.clone(),
                version: "0".into(),
                description: "t".into(),
                kind: PluginKind::Optional,
                contributions,
            }
        }
    }

    #[tokio::test]
    async fn screen_conflict_is_hard_error() {
        let reg = PluginRegistry::from_plugins(vec![
            Box::new(WithScreen("test:a".into(), "themes.picker".into())),
            Box::new(WithScreen("test:b".into(), "themes.picker".into())),
        ]);
        let err = Indexes::build(&reg).await.unwrap_err();
        assert!(
            matches!(err, IndexBuildError::ScreenConflict { ref id, .. } if id == "themes.picker")
        );
    }

    struct WithBinding(String, KeybindingSpec);

    #[async_trait]
    impl Plugin for WithBinding {
        fn manifest(&self) -> Manifest {
            let mut contributions = Contributions::default();
            contributions.keybindings = vec![self.1.clone()];
            Manifest {
                id: PluginId::new(&self.0).expect("valid test id"),
                name: self.0.clone(),
                version: "0".into(),
                description: "t".into(),
                kind: PluginKind::Optional,
                contributions,
            }
        }
    }

    #[tokio::test]
    async fn keybinding_conflict_is_hard_error() {
        use savvagent_plugin::{Effect, KeyCodePortable, KeyEventPortable, KeyMods};
        let chord = ChordPortable::new(KeyEventPortable {
            code: KeyCodePortable::Char('s'),
            modifiers: KeyMods {
                ctrl: true,
                alt: false,
                shift: false,
                meta: false,
            },
        });
        let spec = KeybindingSpec {
            scope: KeyScope::Global,
            chord: chord.clone(),
            action: BoundAction::EmitEffect(Effect::Quit),
        };
        let reg = PluginRegistry::from_plugins(vec![
            Box::new(WithBinding("test:a".into(), spec.clone())),
            Box::new(WithBinding("test:b".into(), spec)),
        ]);
        let err = Indexes::build(&reg).await.unwrap_err();
        assert!(matches!(err, IndexBuildError::KeybindingConflict { .. }));
    }

    #[tokio::test]
    async fn slots_sort_by_priority() {
        let reg = PluginRegistry::from_plugins(vec![
            Box::new(WithSlots("test:a".into(), vec![("home.tips".into(), 200)])),
            Box::new(WithSlots("test:b".into(), vec![("home.tips".into(), 100)])),
        ]);
        let idx = Indexes::build(&reg).await.unwrap();
        let tips = idx.slots.get("home.tips").unwrap();
        assert_eq!(tips[0].0, 100);
        assert_eq!(tips[1].0, 200);
    }

    /// `reindex_plugin` idempotency: build with two plugins (A contributes
    /// `cmd-a`, B contributes `cmd-b`), then call `reindex_plugin` for A.
    /// After the call:
    /// - A's slash entry must still be present (removed then re-inserted).
    /// - B's slash entry must be untouched.
    /// - The total slash index size is unchanged.
    #[tokio::test]
    async fn reindex_plugin_is_idempotent_and_preserves_other_plugin() {
        let reg = PluginRegistry::from_plugins(vec![
            Box::new(WithSlash("test:a".into(), "cmd-a".into())),
            Box::new(WithSlash("test:b".into(), "cmd-b".into())),
        ]);
        let mut idx = Indexes::build(&reg).await.unwrap();

        // Pre-conditions.
        assert_eq!(idx.slash.len(), 2);
        let pid_a = PluginId::new("test:a").unwrap();
        let pid_b = PluginId::new("test:b").unwrap();
        assert_eq!(idx.slash.get("cmd-a"), Some(&pid_a));
        assert_eq!(idx.slash.get("cmd-b"), Some(&pid_b));

        // Reindex A.
        idx.reindex_plugin(&pid_a, &reg).await;

        // Post-conditions: both entries still present, owners correct.
        assert_eq!(
            idx.slash.len(),
            2,
            "reindex must not alter total slash entry count"
        );
        assert_eq!(
            idx.slash.get("cmd-a"),
            Some(&pid_a),
            "A's slash entry must survive reindex"
        );
        assert_eq!(
            idx.slash.get("cmd-b"),
            Some(&pid_b),
            "B's slash entry must not be disturbed by reindexing A"
        );
    }
}

#[cfg(test)]
mod tool_summary_index_tests {
    use super::*;
    use crate::plugin::registry::PluginRegistry;
    use async_trait::async_trait;
    use savvagent_plugin::{Contributions, Manifest, Plugin, PluginKind, ToolSummarySpec};

    struct ClaimsTool {
        id: String,
        tools: Vec<String>,
    }

    #[async_trait]
    impl Plugin for ClaimsTool {
        fn manifest(&self) -> Manifest {
            let mut contributions = Contributions::default();
            contributions.tool_summaries = self
                .tools
                .iter()
                .map(|t| ToolSummarySpec {
                    tool_name: t.clone(),
                })
                .collect();
            Manifest {
                id: PluginId::new(&self.id).unwrap(),
                name: self.id.clone(),
                version: "0.10.0".into(),
                description: "test".into(),
                kind: PluginKind::Core,
                contributions,
            }
        }
    }

    #[tokio::test]
    async fn tool_summary_index_maps_names_to_plugin() {
        let reg = PluginRegistry::from_plugins(vec![Box::new(ClaimsTool {
            id: "test:fs".into(),
            tools: vec!["read_file".into(), "write_file".into()],
        })]);
        let idx = Indexes::build(&reg).await.unwrap();
        assert_eq!(idx.tool_summaries.len(), 2);
        assert_eq!(
            idx.tool_summaries.get("read_file").unwrap().as_str(),
            "test:fs"
        );
        assert_eq!(
            idx.tool_summaries.get("write_file").unwrap().as_str(),
            "test:fs"
        );
    }

    #[tokio::test]
    async fn tool_summary_first_registered_wins_on_conflict() {
        let reg = PluginRegistry::from_plugins(vec![
            Box::new(ClaimsTool {
                id: "test:first".into(),
                tools: vec!["read_file".into()],
            }),
            Box::new(ClaimsTool {
                id: "test:second".into(),
                tools: vec!["read_file".into()],
            }),
        ]);
        let idx = Indexes::build(&reg).await.unwrap();
        // First-registered wins. `PluginRegistry::from_plugins` stores plugins
        // in a `HashMap`, so registration order is not deterministic — assert
        // that *some* plugin wins, not which one, and that it's exactly one
        // entry (not two).
        assert_eq!(idx.tool_summaries.len(), 1);
        let winner = idx.tool_summaries.get("read_file").unwrap().as_str();
        assert!(
            winner == "test:first" || winner == "test:second",
            "winner must be one of the two registered plugins; got {winner}"
        );
    }
}
