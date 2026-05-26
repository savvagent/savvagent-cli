//! Keybinding routing. Scope precedence (most specific first):
//! OnScreen(active_screen_id) > OnHome > Global.

use savvagent_plugin::{BoundAction, ChordPortable, KeyEventPortable, KeyScope};

use crate::plugin::manifests::Indexes;

/// Routes a portable key event to the [`BoundAction`] registered for it,
/// applying scope precedence: `OnScreen` > `OnHome` > `Global`.
pub struct KeybindingRouter<'a> {
    indexes: &'a Indexes,
}

impl<'a> KeybindingRouter<'a> {
    /// Create a router that reads from the given pre-built [`Indexes`].
    pub fn new(indexes: &'a Indexes) -> Self {
        Self { indexes }
    }

    /// Resolve a key event in the given scope context.
    /// `active_screen` is `Some(screen_id)` when a screen is on top of
    /// the stack and `None` for the home view.
    pub fn route(
        &self,
        key: &KeyEventPortable,
        active_screen: Option<&str>,
    ) -> Option<BoundAction> {
        let chord = ChordPortable::new(key.clone());

        if let Some(screen_id) = active_screen {
            if let Some((_, action)) = self
                .indexes
                .keybindings
                .get(&(KeyScope::OnScreen(screen_id.to_string()), chord.clone()))
            {
                return Some(action.clone());
            }
        } else if let Some((_, action)) = self
            .indexes
            .keybindings
            .get(&(KeyScope::OnHome, chord.clone()))
        {
            return Some(action.clone());
        }

        self.indexes
            .keybindings
            .get(&(KeyScope::Global, chord))
            .map(|(_, action)| action.clone())
    }

    /// Resolve a key event while a canvas holds focus. Looks up the
    /// [`KeyScope::OnFocusedCanvas`] scope only — built-in canvas keys
    /// (Tab, Shift-Tab, Esc, Ctrl-J, Ctrl-K, Ctrl-O) are handled by the
    /// event loop *before* this is consulted, so they always win.
    pub fn route_canvas(&self, key: &KeyEventPortable) -> Option<BoundAction> {
        let chord = ChordPortable::new(key.clone());
        self.indexes
            .keybindings
            .get(&(KeyScope::OnFocusedCanvas, chord))
            .map(|(_, action)| action.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::registry::PluginRegistry;
    use async_trait::async_trait;
    use savvagent_plugin::{
        Contributions, Effect, KeyCodePortable, KeyMods, KeybindingSpec, Manifest, Plugin,
        PluginId, PluginKind, ScreenArgs,
    };

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
                description: "".into(),
                kind: PluginKind::Core,
                contributions,
            }
        }
    }

    fn chord(c: char) -> ChordPortable {
        ChordPortable::new(KeyEventPortable {
            code: KeyCodePortable::Char(c),
            modifiers: KeyMods::default(),
        })
    }

    #[tokio::test]
    async fn slash_chord_on_home_opens_palette() {
        let reg = PluginRegistry::from_plugins(vec![Box::new(WithBinding(
            "internal:command-palette".into(),
            KeybindingSpec {
                chord: chord('/'),
                scope: KeyScope::OnHome,
                action: BoundAction::EmitEffect(Effect::OpenScreen {
                    id: "palette".into(),
                    args: ScreenArgs::None,
                }),
            },
        ))]);
        let idx = Indexes::build(&reg).await.unwrap();
        let router = KeybindingRouter::new(&idx);
        let key = KeyEventPortable {
            code: KeyCodePortable::Char('/'),
            modifiers: KeyMods::default(),
        };
        let action = router
            .route(&key, None)
            .expect("home binding should resolve");
        match action {
            BoundAction::EmitEffect(Effect::OpenScreen { id, .. }) => {
                assert_eq!(id, "palette");
            }
            _ => panic!(),
        }
    }
}
