//! Built-in plugin implementations. Each subdirectory hosts one plugin.

/// `internal:changelog` plugin: streams CHANGELOG.md and renders it
/// via tui-markdown in a dedicated `/changelog` screen. Closes #68.
pub mod changelog;

/// Clears the conversation log; registered as `/clear`.
pub mod clear;

/// Filterable slash-command picker; opened via `/` from the home view.
pub mod command_palette;

/// Provider connection picker; opened via `/connect` with no args.
pub mod connect;

/// Basic in-TUI file editor; opened via `/edit <path>`.
pub mod edit_file;

/// Footer widget: sandbox state + turn state + working dir + key reminder.
pub mod home_footer;

/// Tips widget: one-line hint above the prompt; switches text after Connect.
pub mod home_tips;

/// Scrollable, sectioned keybindings help screen reused by the
/// `prompt-keybindings` and `editor-keybindings` plugins. Owns the
/// rendering / scrolling logic so the per-plugin modules just
/// supply their section data.
pub mod keybindings_view;

/// `/editor-keybindings` slash + viewer modal listing the keybindings
/// active inside the ratatui-code-editor (`view-file` / `edit-file`)
/// screens.
pub mod editor_keybindings;

/// `/prompt-keybindings` slash + viewer modal listing the keybindings
/// active in the main prompt input. Includes a dynamic section sourced
/// from [`crate::plugin::manifests::Indexes`] so plugin-contributed
/// bindings show up automatically.
pub mod prompt_keybindings;

/// Language plugin: rust-i18n catalog + /language picker.
pub mod language;

/// `/lsp` slash command: multi-select picker over a curated LSP catalog,
/// downloads pinned binaries (or `npm i -g`s), then merges entries into
/// `~/.savvagent/lsp.toml`.
#[allow(dead_code)]
pub mod lsp_installer;

/// Cycles to the next model on the active provider; registered as `/model`.
pub mod model;

/// Manages enabled/disabled state of Optional plugins; opened via `/plugins`.
/// Persists per-user state in `~/.savvagent/plugins.toml`.
pub mod plugins_manager;

/// Shuts down the TUI; registered as `/quit`. Core plugin (non-disableable).
pub mod quit;

/// Transcript picker; opened via `/resume` with an in-memory cache backed
/// by the [`HookKind::TranscriptSaved`] hook.
pub mod resume;

/// Saves the active transcript to disk; registered as `/save [path]`.
pub mod save;

/// Routes turns via `~/.savvagent/routing.toml`; registered as `/route [reload | show]`.
pub mod route;

/// Self-update plugin: version check, update banner, and `/update` slash.
/// v0.11.0 PR 1 ships only the plugin shell + install-method detection;
/// later PRs add the network check, banner slot, and apply path.
pub mod self_update;

/// Startup HUD screen with connect status; responds to HostStarting + Connect.
pub mod splash;

/// Theme catalog + `/theme` slash + theme picker modal.
pub mod themes;

/// `internal:user-agents` — discovers user-defined subagent definitions
/// and exposes them via an in-process `task` tool.
pub mod user_agents;

/// `internal:user-hooks` — Claude-Code-compatible user shell hooks from
/// `settings.json` files. PreToolUse gating + observe-only events.
pub mod user_hooks;

/// `internal:user-slash-commands` — discovers user-authored slash commands
/// from `.savvagent/commands/` / `.claude/commands/` and dispatches them
/// with templating expansion.
pub mod user_slash_commands;

/// `internal:tool-bash-summary` — renders one-line summaries for the
/// `tool-bash` `run` tool.
pub mod tool_bash_summary;

/// `internal:tool-fs-summary` — renders one-line summaries for the four
/// `tool-fs` tool names (`read_file`, `write_file`, `list_dir`, `glob`).
pub mod tool_fs_summary;

/// `internal:tool-grep-summary` — renders one-line summaries for the
/// `tool-grep` `search` tool.
pub mod tool_grep_summary;

/// `internal:tool-task-summary` — renders one-line summaries for the
/// `task` tool (in-process subagent dispatch from `internal:user-agents`).
pub mod tool_task_summary;

/// Fullscreen read-only file viewer; opened via `/view <path>`.
pub mod view_file;

/// Savvagent-internal [`provider_common::BuiltinProviderPlugin`] trait —
/// the explicit non-WIT-portable seam where `Box<dyn ProviderClient>` is
/// handed off from a provider plugin to the runtime.
pub mod provider_common;

/// Anthropic provider shim: keyring-backed `internal:provider-anthropic`.
pub mod provider_anthropic;

/// OpenAI provider shim: keyring-backed `internal:provider-openai`.
pub mod provider_openai;

/// Google Gemini provider shim: keyring-backed `internal:provider-gemini`.
pub mod provider_gemini;

/// Local (Ollama) provider shim: keyless `internal:provider-local`.
pub mod provider_local;

/// First-launch migration picker for `startup_providers`.
/// Runs once on `HostStarting`; opens a multi-select modal when multiple
/// keyring credentials are detected.
pub mod migration_picker;
