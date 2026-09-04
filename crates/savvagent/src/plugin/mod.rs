//! Plugin runtime root. PR 1 ships only the empty `register_builtins()`
//! entry point; subsequent PRs add registry/screen stack/routers/effects.

/// Built-in plugin implementations shipped with the binary.
pub mod builtin;

/// Reusable UI state-machine helpers (no `Plugin`/`Screen` trait impl).
/// Wrapped by screens under [`builtin`] when a plugin needs the behaviour.
#[allow(dead_code)]
pub mod widgets;

/// Conversion helpers between `savvagent-plugin` types and ratatui types.
pub mod convert;

/// In-memory registry of constructed plugin instances and their enabled-set.
pub mod registry;

/// Derived indexes over enabled-plugin manifests (slash/slots/hooks/keybindings/screens).
pub mod manifests;

/// Slot routing: resolves a slot_id to its priority-ordered contributor list
/// and concatenates each contributor's rendered lines.
pub mod slots;

/// Tool-summary routing: resolves a tool name to its owning plugin and
/// dispatches `Plugin::summarize_tool_call` / `summarize_tool_result`,
/// falling back to the host's JSON highlighter at the call site.
pub mod tool_summaries;

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

/// External (wasm) plugin discovery + adapter wrapping. Owned by the
/// `savvagent-plugin-wasm` crate; the savvagent crate composes built-ins
/// and externals together via [`register_builtins_with_external`].
mod external;
pub(crate) use external::register_builtins_with_external;

/// Returns the set of built-in plugin instances and provider-plugin shims.
///
/// PR 2 adds: home-footer, home-tips.
/// PR 3 adds: splash, command-palette.
/// PR 4 adds: view-file, edit-file.
/// PR 5 adds: connect, resume, model, save, clear.
/// PR 6 adds: themes + 4 providers (anthropic / openai / gemini / local).
/// PR 8 adds: plugins-manager.
/// Task 9 adds: migration-picker.
/// Task 13 adds: html-canvas.
///
/// Provider plugins are stored exactly once per plugin in
/// [`crate::plugin::builtin::provider_common::ProviderEntry`], which exposes
/// the same instance via two trait-object Arcs (`dyn Plugin` and
/// `dyn BuiltinProviderPlugin`). The registry inserts the plugin-view
/// into the slash/render/hook dispatch map and the provider-view into the
/// `take_client` map, so both code paths mutate the same state — the
/// dual-instance bug that previously broke `/connect <provider>` is now
/// architecturally impossible.
pub(crate) fn register_builtins(
    trust_levels: builtin::user_slash_commands::TrustMap,
    user_hooks_index: std::sync::Arc<
        tokio::sync::RwLock<crate::plugin::builtin::user_hooks::discovery::HooksIndex>,
    >,
    session_id: String,
    project_root: std::path::PathBuf,
    transcript_path: std::sync::Arc<tokio::sync::RwLock<std::path::PathBuf>>,
) -> BuiltinSet {
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
        Box::new(builtin::lsp_installer::LspInstallerPlugin::new()),
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
        Box::new(builtin::tool_bash_summary::ToolBashSummaryPlugin::new()),
        Box::new(builtin::user_agents::UserAgentsPlugin::new()),
        Box::new(builtin::user_slash_commands::UserSlashCommandsPlugin::new(
            trust_levels,
        )),
        Box::new(builtin::tool_fs_summary::ToolFsSummaryPlugin::new()),
        Box::new(builtin::tool_grep_summary::ToolGrepSummaryPlugin::new()),
        Box::new(builtin::tool_task_summary::ToolTaskSummaryPlugin::new()),
        Box::new(builtin::tool_web_summary::ToolWebSummaryPlugin::new()),
        Box::new(builtin::view_file::ViewFilePlugin::new()),
        Box::new(builtin::html_canvas::HtmlCanvasPlugin::new()),
    ];

    // Hook plugins live in a parallel Vec so the registry can index them
    // both as `dyn Plugin` (for slash/render/hook dispatch) and as
    // `dyn BuiltinHookPlugin` (for `RegisterPreToolGate` apply). Mirrors
    // the `ProviderEntry` pattern; both views share the same `Arc<Mutex<T>>`.
    let hook_entries: Vec<crate::plugin::registry::HookEntry> =
        vec![crate::plugin::registry::HookEntry::new(
            builtin::user_hooks::UserHooksPlugin::new(
                user_hooks_index.clone(),
                session_id.clone(),
                project_root.clone(),
                transcript_path.clone(),
            ),
        )];

    BuiltinSet {
        plugins,
        providers,
        hook_entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::registry::PluginRegistry;
    use savvagent_plugin::PluginId;

    /// Reproduce the exact runtime canvas-renderer resolution path:
    /// full builtin set → registry → indexes → `content_renderer_for("html")`
    /// → `registry.get(id).create_renderer("html")`. This guards against the
    /// `ContentRendererNotFound("html")` runtime failure seen in the field by
    /// proving the index entry and the registered plugin instance agree.
    #[tokio::test]
    async fn html_renderer_resolves_through_registry_and_indexes() {
        use crate::plugin::manifests::Indexes;
        use crate::plugin::registry::PluginRegistry;
        use std::collections::BTreeMap;
        use std::sync::Arc;
        let set = register_builtins(
            Arc::new(tokio::sync::RwLock::new(BTreeMap::new())),
            Arc::new(tokio::sync::RwLock::new(
                crate::plugin::builtin::user_hooks::discovery::HooksIndex::default(),
            )),
            "test-session".into(),
            std::path::PathBuf::from("/tmp"),
            Arc::new(tokio::sync::RwLock::new(std::path::PathBuf::from(
                "/t.json",
            ))),
        );
        let registry = PluginRegistry::new(set);
        let indexes = Indexes::build(&registry).await.expect("indexes build");
        let pid = indexes
            .content_renderer_for("html")
            .expect("html renderer must be indexed")
            .clone();
        assert_eq!(pid.as_str(), "internal:html-canvas");
        let handle = registry.get(&pid).expect("plugin must be in registry");
        let guard = handle.lock().await;
        let r = guard.create_renderer("html", savvagent_plugin::ContentBlockId(0), "<p>x</p>");
        assert!(
            r.is_ok(),
            "registry-resolved plugin must create an html renderer, got {:?}",
            r.err()
        );
    }

    #[tokio::test]
    async fn register_builtins_pr8_complete() {
        use std::collections::BTreeMap;
        use std::sync::Arc;
        let set = register_builtins(
            Arc::new(tokio::sync::RwLock::new(BTreeMap::new())),
            Arc::new(tokio::sync::RwLock::new(
                crate::plugin::builtin::user_hooks::discovery::HooksIndex::default(),
            )),
            "test-session".into(),
            std::path::PathBuf::from("/tmp"),
            Arc::new(tokio::sync::RwLock::new(std::path::PathBuf::from(
                "/t.json",
            ))),
        );
        // Non-provider plugins from PR 1..PR 5 + themes (PR 6) + plugins-manager (PR 8)
        // + migration-picker (Task 9) + tool-web-summary (web tools).
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
            "internal:lsp-installer",
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
            "internal:tool-bash-summary",
            "internal:tool-fs-summary",
            "internal:tool-grep-summary",
            "internal:tool-task-summary",
            "internal:tool-web-summary",
            "internal:user-agents",
            "internal:user-slash-commands",
            "internal:view-file",
            "internal:html-canvas",
        ] {
            assert!(
                plugin_ids.contains(&expected.to_string()),
                "missing non-provider plugin id: {expected}"
            );
        }
        assert_eq!(set.plugins.len(), 30);

        // `internal:user-hooks` lives in `hook_entries`, not the `plugins`
        // Vec. The dual-Arc HookEntry pattern means it still appears in
        // the registry's plugins map (see assertion below) AND the hooks
        // map, so slash/render/hook dispatch and `RegisterPreToolGate`
        // apply both find the same instance.
        let hook_ids: Vec<_> = set
            .hook_entries
            .iter()
            .map(|e| e.id.as_str().to_string())
            .collect();
        assert_eq!(hook_ids, vec!["internal:user-hooks".to_string()]);
        assert_eq!(set.hook_entries.len(), 1);

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

        // Registry shape: non-provider plugins PLUS 4 provider plugins
        // PLUS 1 hook plugin (HookEntry's `as_plugin` view is inserted
        // into the same id-keyed plugins map by `PluginRegistry::new`).
        //
        // Task 9 adds migration-picker, bringing non-provider count to 20;
        // Task 6 adds route, bringing non-provider count to 21;
        // Task 11 adds tool-bash/fs/grep-summary, bringing non-provider count to 24;
        // v0.16.0 adds lsp-installer, bringing non-provider count to 25;
        // user-slash-commands adds 1 more, bringing non-provider count to 26;
        // html-canvas adds 1 more, bringing non-provider count to 27;
        // sub-project C (user-agents) adds 1 more, bringing non-provider count to 28;
        // tool-task-summary adds 1 more, bringing non-provider count to 29;
        // tool-web-summary adds 1 more, bringing non-provider count to 30;
        // sub-project B (user-hooks) moves to `hook_entries` (not counted
        // in the plugins Vec) but still surfaces in the registry's plugins
        // map via the dual-Arc HookEntry, contributing 1 more registry
        // entry; total registry size is 30 + 4 + 1 = 35.
        let reg = PluginRegistry::new(set);
        assert_eq!(
            reg.len(),
            35,
            "registry should have 30 non-provider + 4 provider + 1 hook plugin"
        );
        assert_eq!(
            reg.provider_count(),
            4,
            "registry should have 4 provider plugins"
        );

        // The user-hooks plugin still resolves through `reg.get(&pid)`
        // because the HookEntry's `as_plugin` view is inserted into the
        // plugins HashMap by `PluginRegistry::new`.
        let user_hooks_pid = PluginId::new("internal:user-hooks").unwrap();
        assert!(
            reg.get(&user_hooks_pid).is_some(),
            "hook plugin must resolve via reg.get() (Plugin-view of HookEntry)"
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

    #[tokio::test]
    async fn register_builtins_includes_user_hooks_hook_entry() {
        use std::collections::BTreeMap;
        use std::sync::Arc;
        let set = register_builtins(
            Arc::new(tokio::sync::RwLock::new(BTreeMap::new())),
            Arc::new(tokio::sync::RwLock::new(
                crate::plugin::builtin::user_hooks::discovery::HooksIndex::default(),
            )),
            "test-session".into(),
            std::path::PathBuf::from("/tmp"),
            Arc::new(tokio::sync::RwLock::new(std::path::PathBuf::from(
                "/t.json",
            ))),
        );
        assert_eq!(set.hook_entries.len(), 1);
        // The hook-view exposes the same manifest as the plugin-view
        // (it's the same `Arc<Mutex<T>>`); confirms wiring.
        let id = {
            let guard = set.hook_entries[0].as_hook.try_lock().unwrap();
            guard.manifest().id.as_str().to_string()
        };
        assert_eq!(id, "internal:user-hooks");
    }
}
