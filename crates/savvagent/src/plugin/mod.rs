//! Plugin runtime root. PR 1 ships only the empty `register_builtins()`
//! entry point; subsequent PRs add registry/screen stack/routers/effects.

/// Built-in plugin implementations shipped with the binary.
pub mod builtin;

/// Conversion helpers between `savvagent-plugin` types and ratatui types.
pub mod convert;

/// In-memory registry of constructed plugin instances and their enabled-set.
pub mod registry;

/// Derived indexes over enabled-plugin manifests (slash/slots/hooks/keybindings/screens).
pub mod manifests;

/// Slot routing: resolves a slot_id to its priority-ordered contributor list
/// and concatenates each contributor's rendered lines.
pub mod slots;

/// Slash command routing: resolves bare command names to their owning plugin
/// and dispatches `handle_slash`, with a re-entrancy depth cap.
#[allow(dead_code)]
pub mod slash;

/// Keybinding routing: resolves a portable key event to its [`savvagent_plugin::BoundAction`]
/// using scope precedence `OnScreen` > `OnHome` > `Global`.
#[allow(dead_code)]
pub mod keybindings;

/// LIFO stack of `(Box<dyn Screen>, ScreenLayout)` pairs driven by
/// `Effect::OpenScreen` / `Effect::CloseScreen`; replaces the v0.8
/// `InputMode` flat-field state machine.
#[allow(dead_code)]
pub mod screen_stack;

/// Single mutation surface: maps each `Effect` variant to the corresponding
/// `App` method. The event loop calls this after dispatching key events, hook
/// events, or slash commands.
#[allow(dead_code)]
pub mod effects;

/// Sequential awaited dispatch of [`savvagent_plugin::HostEvent`]s to subscribed
/// plugins. The TUI event loop and `effects::dispatch_host_event` both go
/// through this to fan out a single event to every plugin that subscribed
/// to its [`savvagent_plugin::HookKind`].
pub mod hooks;

/// Re-export so callers don't have to reach into the registry submodule
/// for the type returned from [`register_builtins`].
pub(crate) use registry::BuiltinSet;

/// Returns the set of built-in plugin instances and provider-plugin shims.
///
/// PR 2 adds: home-footer, home-tips.
/// PR 3 adds: splash, command-palette.
/// PR 4 adds: view-file, edit-file.
/// PR 5 adds: connect, resume, model, save, clear.
/// PR 6 adds: themes + 4 providers (anthropic / openai / gemini / local).
/// PR 8 adds: plugins-manager.
/// Task 9 adds: migration-picker.
///
/// Provider plugins are stored exactly once per plugin in
/// [`crate::plugin::builtin::provider_common::ProviderEntry`], which exposes
/// the same instance via two trait-object Arcs (`dyn Plugin` and
/// `dyn BuiltinProviderPlugin`). The registry inserts the plugin-view
/// into the slash/render/hook dispatch map and the provider-view into the
/// `take_client` map, so both code paths mutate the same state — the
/// dual-instance bug that previously broke `/connect <provider>` is now
/// architecturally impossible.
pub(crate) fn register_builtins() -> BuiltinSet {
    use builtin::provider_common::ProviderEntry;

    let providers: Vec<ProviderEntry> = vec![
        ProviderEntry::new(builtin::provider_anthropic::ProviderAnthropicPlugin::new()),
        ProviderEntry::new(builtin::provider_openai::ProviderOpenAiPlugin::new()),
        ProviderEntry::new(builtin::provider_gemini::ProviderGeminiPlugin::new()),
        ProviderEntry::new(builtin::provider_local::ProviderLocalPlugin::new()),
    ];

    let plugins: Vec<Box<dyn savvagent_plugin::Plugin>> = vec![
        Box::new(builtin::changelog::ChangelogPlugin::new()),
        Box::new(builtin::clear::ClearPlugin::new()),
        Box::new(builtin::command_palette::CommandPalettePlugin::new()),
        Box::new(builtin::connect::ConnectPlugin::new()),
        Box::new(builtin::edit_file::EditFilePlugin::new()),
        Box::new(builtin::editor_keybindings::EditorKeybindingsPlugin::new()),
        Box::new(builtin::home_footer::HomeFooterPlugin::new()),
        Box::new(builtin::home_tips::HomeTipsPlugin::new()),
        Box::new(builtin::language::LanguagePlugin::new()),
        Box::new(builtin::migration_picker::MigrationPickerPlugin::new()),
        Box::new(builtin::model::ModelPlugin::new()),
        Box::new(builtin::plugins_manager::PluginsManagerPlugin::new()),
        Box::new(builtin::prompt_keybindings::PromptKeybindingsPlugin::new()),
        Box::new(builtin::quit::QuitPlugin::new()),
        Box::new(builtin::resume::ResumePlugin::new()),
        Box::new(builtin::route::RoutePlugin::new()),
        Box::new(builtin::save::SavePlugin::new()),
        Box::new(builtin::self_update::SelfUpdatePlugin::new()),
        Box::new(builtin::splash::SplashPlugin::new()),
        Box::new(builtin::themes::ThemesPlugin::new()),
        Box::new(builtin::view_file::ViewFilePlugin::new()),
    ];

    BuiltinSet { plugins, providers }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::registry::PluginRegistry;
    use savvagent_plugin::PluginId;

    #[tokio::test]
    async fn register_builtins_pr8_complete() {
        let set = register_builtins();
        // Non-provider plugins from PR 1..PR 5 + themes (PR 6) + plugins-manager (PR 8)
        // + migration-picker (Task 9).
        let plugin_ids: Vec<_> = set
            .plugins
            .iter()
            .map(|p| p.manifest().id.as_str().to_string())
            .collect();
        for expected in [
            "internal:changelog",
            "internal:clear",
            "internal:command-palette",
            "internal:connect",
            "internal:edit-file",
            "internal:editor-keybindings",
            "internal:home-footer",
            "internal:home-tips",
            "internal:language",
            "internal:migration-picker",
            "internal:model",
            "internal:plugins-manager",
            "internal:prompt-keybindings",
            "internal:quit",
            "internal:resume",
            "internal:route",
            "internal:save",
            "internal:self-update",
            "internal:splash",
            "internal:themes",
            "internal:view-file",
        ] {
            assert!(
                plugin_ids.contains(&expected.to_string()),
                "missing non-provider plugin id: {expected}"
            );
        }
        assert_eq!(set.plugins.len(), 21);

        // PR 6 adds the 4 provider shims — exactly once each.
        let provider_ids: Vec<_> = {
            let mut ids = Vec::new();
            for entry in &set.providers {
                let guard = entry.as_provider.try_lock().unwrap();
                ids.push(guard.manifest().id.as_str().to_string());
            }
            ids
        };
        for expected in [
            "internal:provider-anthropic",
            "internal:provider-openai",
            "internal:provider-gemini",
            "internal:provider-local",
        ] {
            assert!(
                provider_ids.contains(&expected.to_string()),
                "missing provider id: {expected}"
            );
        }
        assert_eq!(set.providers.len(), 4);

        // Registry shape: non-provider plugins PLUS 4 provider plugins.
        // Task 9 adds migration-picker, bringing non-provider count to 20;
        // Task 6 adds route, bringing non-provider count to 21;
        // total registry size is 21 + 4 = 25.
        let reg = PluginRegistry::new(set);
        assert_eq!(
            reg.len(),
            25,
            "registry should have 21 non-provider + 4 provider plugins"
        );
        assert_eq!(
            reg.provider_count(),
            4,
            "registry should have 4 provider plugins"
        );

        // And every provider id resolves through `get` (proves the
        // Plugin-view side of the ProviderEntry is wired in).
        for pid_str in [
            "internal:provider-anthropic",
            "internal:provider-openai",
            "internal:provider-gemini",
            "internal:provider-local",
        ] {
            let pid = PluginId::new(pid_str).unwrap();
            assert!(
                reg.get(&pid).is_some(),
                "provider {pid_str} missing from plugins map"
            );
        }
    }
}
