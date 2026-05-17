//! Savvagent TUI entry point.
//!
//! Bootstraps a [`savvagent_host::Host`] in one of two ways:
//!
//! 1. **In-process (default).** Each provider crate is linked as a library;
//!    the TUI builds a [`ProviderHandler`](savvagent_mcp::ProviderHandler) and
//!    wraps it in `InProcessProviderClient` — no MCP transport, no spawned
//!    binary. The TUI scans the OS keyring for a saved API key and
//!    auto-connects to the first provider it finds; otherwise the user
//!    runs `/connect`.
//! 2. **Remote MCP (opt-in).** If `SAVVAGENT_PROVIDER_URL` is set the TUI
//!    connects to that Streamable HTTP MCP server instead — useful for
//!    pointing at a long-running `savvagent-anthropic`/`savvagent-gemini`
//!    binary or a third-party MCP provider.
//!
//! Other configuration:
//!
//! - `SAVVAGENT_MODEL`          (overrides the per-provider default)
//! - `SAVVAGENT_TOOL_FS_BIN`    (default `savvagent-tool-fs` on $PATH)
//! - `SAVVAGENT_TOOL_BASH_BIN`  (default `savvagent-tool-bash` on $PATH)
//! - `SAVVAGENT_TOOL_GREP_BIN`  (default `savvagent-tool-grep` on $PATH)

rust_i18n::i18n!("locales", fallback = "en");

#[cfg(test)]
mod i18n_smoke {
    #[test]
    fn smoke_key_resolves_in_en() {
        rust_i18n::set_locale("en");
        assert_eq!(rust_i18n::t!("smoke.hello"), "hello, world");
    }
}

mod app;
mod config_file;
mod creds;
mod migration;
mod models_pref;
mod palette;
mod plugin;
mod providers;
mod splash;
#[cfg(test)]
mod test_helpers;
mod tui;
mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use app::{
    App, BashCommandError, Entry, InputMode, collect_transcript_entries, make_input_textarea,
    parse_bash_command,
};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use providers::{PROVIDERS, ProviderSpec};
use savvagent_host::{
    BashNetworkChoice, Host, HostConfig, LegacyModelResolution, PermissionDecision,
    ProviderEndpoint, ProviderRegistration, ProviderView, SandboxConfig, SandboxMode,
    ToolCallStatus, ToolEndpoint, TranscriptError, TurnEvent, resolve_legacy_model,
};
use tokio::sync::{RwLock, mpsc};

/// Worker → main-loop messages.
enum WorkerMsg {
    Event(TurnEvent),
    /// Sent if `run_turn_streaming` returned an error.
    Error(String),
    /// Sent when a `/bash` direct-invocation worker finishes (success or
    /// error). The main loop uses this to clear `app.is_loading`, mirroring
    /// the `TurnComplete` path for model-driven turns.
    BashDone,
    /// Sent when a `/disconnect` drain/force worker completes successfully.
    DisconnectCompleted {
        provider: String,
        mode: String,
    },
    /// Sent when a `/disconnect` worker encounters an error from
    /// `Host::remove_provider`.
    DisconnectFailed {
        provider: String,
        err: String,
    },
}

type HostSlot = Arc<RwLock<Option<Arc<Host>>>>;

/// Resolved paths for every bundled tool-server binary the TUI knows how to
/// register. Each field is `None` when the binary couldn't be found; the
/// host just doesn't advertise that tool's surface in `tools/list`.
#[derive(Clone, Default)]
struct ToolBins {
    fs: Option<PathBuf>,
    bash: Option<PathBuf>,
    grep: Option<PathBuf>,
}

impl ToolBins {
    /// Append every populated entry as a stdio [`ToolEndpoint`] on `config`.
    fn apply(&self, mut config: HostConfig) -> HostConfig {
        for path in [
            self.fs.as_deref(),
            self.bash.as_deref(),
            self.grep.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            config = config.with_tool(ToolEndpoint::Stdio {
                command: path.to_path_buf(),
                args: vec![],
            });
        }
        config
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let tool_bins = ToolBins {
        fs: locate_bundled_bin("savvagent-tool-fs", "SAVVAGENT_TOOL_FS_BIN"),
        bash: locate_bundled_bin("savvagent-tool-bash", "SAVVAGENT_TOOL_BASH_BIN"),
        grep: locate_bundled_bin("savvagent-tool-grep", "SAVVAGENT_TOOL_GREP_BIN"),
    };

    let config_file =
        config_file::ConfigFile::load_or_default(&config_file::ConfigFile::default_path());
    let initial = bootstrap_pool_host(&project_root, &tool_bins, &config_file).await;
    let (header_model, initial_provider, startup_notes) = match &initial {
        Some((_, model, id, notes)) => (model.clone(), *id, notes.clone()),
        None => ("(disconnected)".to_string(), None, Vec::new()),
    };

    let host_slot: HostSlot = Arc::new(RwLock::new(initial.map(|(h, _, _, _)| h)));

    let transcript_dir = transcript_dir();

    let mut terminal = tui::init()?;

    // Restore terminal on panic.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = tui::restore();
        original_hook(info);
    }));

    let initial_locale = crate::plugin::builtin::language::catalog::detect_initial();
    rust_i18n::set_locale(&initial_locale);
    let mut app = App::new(header_model, transcript_dir, initial_locale);

    {
        use crate::plugin::manifests::Indexes;
        use crate::plugin::registry::PluginRegistry;
        use savvagent_plugin::PluginKind;

        let set = plugin::register_builtins();
        let mut registry = PluginRegistry::new(set);

        // Apply persisted Optional-plugin enabled state from
        // ~/.savvagent/plugins.toml so the initial Indexes::build picks
        // up the user's saved choices. Core plugins are always enabled
        // regardless of what the file says; unknown ids are skipped.
        //
        // Both skip branches warn-log so the user can diagnose two
        // otherwise-silent scenarios:
        //   - downgrade: a previously-disabled plugin no longer exists
        //     in this binary.
        //   - hand-edit: someone added a Core plugin to plugins.toml
        //     (which the spec forbids the runtime to honour).
        let persisted = plugin::builtin::plugins_manager::persistence::load();
        for (pid, enabled) in persisted {
            let Some(plugin) = registry.get(&pid) else {
                tracing::warn!(
                    plugin = %pid.as_str(),
                    "plugins.toml: unknown plugin id; entry ignored (downgrade or removed plugin?)"
                );
                continue;
            };
            let kind = plugin.lock().await.manifest().kind;
            match kind {
                PluginKind::Optional => registry.set_enabled(&pid, enabled),
                PluginKind::Core => tracing::warn!(
                    plugin = %pid.as_str(),
                    "plugins.toml: Core plugins cannot be disabled; ignoring entry"
                ),
            }
        }

        // Honour the user's saved Optional-plugin choices for legacy
        // hardcoded subsystems. The startup splash render lives in
        // `mod splash` rather than the `internal:splash` plugin's
        // Screen; gate the hardcoded path on the plugin's enabled
        // state so toggling `splash` in /plugins actually disables it.
        let splash_id =
            savvagent_plugin::PluginId::new("internal:splash").expect("valid built-in id");
        if !registry.is_enabled(&splash_id) {
            app.show_splash = false;
        }

        let indexes = Indexes::build(&registry)
            .await
            .unwrap_or_else(|e| panic!("plugin manifest conflict at startup: {e}"));
        app.install_plugin_runtime(registry, indexes);
    }

    app.connected = host_slot.read().await.is_some();
    app.active_provider_id = initial_provider;
    // If we already have a host (e.g. saved-credentials bootstrap), align
    // the splash sandbox indicator with what the host will actually apply.
    // Otherwise it would briefly show the on-disk preference even when the
    // active host was built with a different SandboxConfig override.
    if let Some(host) = current_host(&host_slot).await {
        app.refresh_splash_sandbox_from_host(host.sandbox_config());
    }
    if !app.connected {
        app.push_note(rust_i18n::t!("notes.not-connected-startup").to_string());
    }
    // Surface any startup-timeout notes that were collected before App existed.
    for note in startup_notes {
        app.push_note(note);
    }
    if tool_bins.fs.is_none() {
        app.push_note(rust_i18n::t!("errors.tool-fs-not-found").to_string());
    }
    if tool_bins.bash.is_none() {
        app.push_note(rust_i18n::t!("errors.tool-bash-not-found").to_string());
    }
    if tool_bins.grep.is_none() {
        app.push_note(rust_i18n::t!("errors.tool-grep-not-found").to_string());
    }
    let res = run_app(
        &mut terminal,
        &mut app,
        host_slot.clone(),
        project_root,
        tool_bins,
    )
    .await;

    let _ = tui::restore();

    if let Some(host) = current_host(&host_slot).await {
        if let Err(e) = save_transcript_now(&app, &host).await {
            eprintln!("warning: could not save transcript on exit: {e}");
        }
    }
    if let Some(host) = host_slot.write().await.take() {
        host.shutdown().await;
    }

    if let Err(err) = res {
        eprintln!("{err:?}");
    }

    // If `/update` succeeded during this session, the on-disk binary is
    // a newer version than the one we're still running. Surface a hint
    // on stderr now that the alt-screen has torn down.
    if let Some((from, to)) = plugin::builtin::self_update::pending_restart_hint() {
        eprintln!("savvagent: installed v{to} (was v{from}). Restart to use the new version.");
    }

    Ok(())
}

/// Build the host using the provider pool path, reading startup policy from
/// `~/.savvagent/config.toml`.
///
/// Each provider plugin's `try_build_registration` is wrapped in
/// `tokio::time::timeout(connect_timeout_ms)`. Timeout and build failures are
/// warned rather than failing startup — missing providers mean the user runs
/// `/connect` later.
///
/// Returns `(host, initial_model, initial_provider_id, deferred_notes)`.
/// `deferred_notes` collects timeout warnings that should be shown in the TUI
/// once `App` exists.
///
/// The legacy `SAVVAGENT_PROVIDER_URL` path is still supported: when that env
/// var is set we skip the pool path entirely and fall back to the rmcp HTTP
/// transport (the headless debug workflow).
///
async fn bootstrap_pool_host(
    project_root: &Path,
    tool_bins: &ToolBins,
    config_file: &config_file::ConfigFile,
) -> Option<(Arc<Host>, String, Option<&'static str>, Vec<String>)> {
    // Legacy MCP-over-HTTP debug path. When this env var is set the host
    // connects to a remote provider binary instead of using the in-process
    // pool. No pool, no policy, no timeout wrapping.
    if let Ok(url) = std::env::var("SAVVAGENT_PROVIDER_URL") {
        let model =
            std::env::var("SAVVAGENT_MODEL").unwrap_or_else(|_| "claude-haiku-4-5".to_string());
        match start_host_remote(url, model.clone(), project_root.to_path_buf(), tool_bins).await {
            Ok(host) => return Some((host, model, None, Vec::new())),
            Err(e) => {
                eprintln!("warning: SAVVAGENT_PROVIDER_URL set but connect failed: {e:#}");
            }
        }
    }

    use crate::plugin::builtin::{
        provider_anthropic::ProviderAnthropicPlugin, provider_gemini::ProviderGeminiPlugin,
        provider_local::ProviderLocalPlugin, provider_openai::ProviderOpenAiPlugin,
    };

    let timeout_dur = Duration::from_millis(config_file.startup.connect_timeout_ms);
    let mut providers: Vec<ProviderRegistration> = Vec::new();
    let mut deferred_notes: Vec<String> = Vec::new();

    // Try each provider plugin in priority order. Timeout and build errors
    // are non-fatal: the user can `/connect` any provider later.
    macro_rules! try_provider {
        ($plugin:expr, $log_name:literal, $spec_id:literal) => {{
            match tokio::time::timeout(timeout_dur, $plugin.try_build_registration()).await {
                Ok(Ok(Some(reg))) => {
                    providers.push(reg);
                }
                Ok(Ok(None)) => {
                    // No credentials stored; user will /connect later.
                }
                Ok(Err(e)) => {
                    tracing::warn!(plugin = $log_name, error = %e, "provider build failed at startup");
                    deferred_notes.push(rust_i18n::t!(
                        "notes.startup-build-failed",
                        name = $log_name,
                        err = e.to_string(),
                        id = $spec_id
                    ).to_string());
                }
                Err(_elapsed) => {
                    let ms = config_file.startup.connect_timeout_ms;
                    tracing::warn!(
                        plugin = $log_name,
                        timeout_ms = ms,
                        "provider auto-connect timed out"
                    );
                    deferred_notes.push(rust_i18n::t!(
                        "notes.startup-timeout",
                        name = $log_name,
                        ms = ms.to_string(),
                        id = $spec_id
                    ).to_string());
                }
            }
        }};
    }

    try_provider!(ProviderAnthropicPlugin::new(), "Anthropic", "anthropic");
    try_provider!(ProviderGeminiPlugin::new(), "Gemini", "gemini");
    try_provider!(ProviderOpenAiPlugin::new(), "OpenAI", "openai");
    try_provider!(ProviderLocalPlugin::new(), "Local (Ollama)", "local");

    if providers.is_empty() {
        // No providers connected — return None so the TUI starts disconnected.
        return None;
    }

    // Determine the initial active provider. `Host::start` will connect the
    // first provider that passes the startup_connect filter; mirror that logic
    // here to derive the header model + provider-id hint.
    let startup_policy = config_file.to_startup_policy();
    let active_reg = {
        use savvagent_host::StartupConnectPolicy;
        // Build a predicate that mirrors Host::start's filtering logic so
        // we can compute the initial model hint without waiting for the host.
        let allow_set: Option<std::collections::HashSet<_>> = match &startup_policy {
            StartupConnectPolicy::All => None,
            StartupConnectPolicy::None => Some(std::collections::HashSet::new()),
            StartupConnectPolicy::OptIn(allow) | StartupConnectPolicy::LastUsed(allow) => {
                Some(allow.iter().cloned().collect())
            }
            // Non-exhaustive: future variants default to no filter (same as All).
            _ => None,
        };
        providers
            .iter()
            .find(|r| match &allow_set {
                None => true, // All
                Some(set) => set.contains(&r.id),
            })
            .or_else(|| providers.first())
    };

    // Run the SAVVAGENT_MODEL legacy resolver against the full provider list.
    // This handles both "provider/model" and bare-model forms and surfaces
    // diagnostics when the value is ambiguous or unknown.
    let env_model_raw = std::env::var("SAVVAGENT_MODEL").unwrap_or_default();
    let provider_views: Vec<ProviderView<'_>> = providers
        .iter()
        .map(|r| ProviderView {
            id: &r.id,
            capabilities: &r.capabilities,
        })
        .collect();
    let legacy_resolution = resolve_legacy_model(&env_model_raw, &provider_views);

    // For Resolved/ResolvedFromBare the resolver picked a specific provider;
    // override active_reg to that registration so the header and HostConfig
    // both reflect the correct initial model. For all other outcomes the
    // existing policy-filtered active_reg stands.
    let (resolved_model, resolved_reg): (Option<String>, Option<&ProviderRegistration>) =
        match &legacy_resolution {
            LegacyModelResolution::Resolved { provider, model } => {
                let reg = providers.iter().find(|r| &r.id == provider);
                (Some(model.clone()), reg)
            }
            LegacyModelResolution::ResolvedFromBare {
                provider,
                model,
                note,
            } => {
                deferred_notes.push(note.clone());
                let reg = providers.iter().find(|r| &r.id == provider);
                (Some(model.clone()), reg)
            }
            LegacyModelResolution::Ambiguous { note, .. }
            | LegacyModelResolution::Unknown { note }
            | LegacyModelResolution::UnknownProvider { note, .. } => {
                deferred_notes.push(note.clone());
                (None, None)
            }
            LegacyModelResolution::NoOverride => (None, None),
            // Non-exhaustive: future variants fall through to no override.
            _ => (None, None),
        };

    let effective_reg = resolved_reg.or(active_reg);

    let (initial_model, initial_provider_id) = match effective_reg {
        Some(reg) => {
            // Use the resolved model when the legacy resolver found one;
            // otherwise fall back to the persisted models.toml preference or
            // the provider's capability default_model.
            let model = if let Some(m) = resolved_model {
                m
            } else {
                let base = reg.capabilities.default_model_id().to_string();
                let pref = models_pref::ModelsPref::load();
                pref.get(reg.id.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or(base)
            };
            // Map the ProviderRegistration id back to a `&'static str` by
            // looking it up in the legacy PROVIDERS catalog (which already
            // exists). This is only used for the header display + active_provider_id.
            let static_id = PROVIDERS
                .iter()
                .find(|s| s.id == reg.id.as_str())
                .map(|s| s.id);
            (model, static_id)
        }
        None => ("(disconnected)".to_string(), None),
    };

    let mut config = tool_bins.apply(
        HostConfig::new(
            // Legacy `provider` field is unused when `providers` is non-empty.
            // Pass a recognisable placeholder so any accidental log lines say
            // where they came from.
            ProviderEndpoint::StreamableHttp {
                url: "inproc://pool".into(),
            },
            initial_model.clone(),
        )
        .with_project_root(project_root.to_path_buf())
        .with_app_version(env!("CARGO_PKG_VERSION")),
    );
    config.providers = providers;
    config.startup_connect = startup_policy;
    config.connect_timeout_ms = config_file.startup.connect_timeout_ms;

    match Host::start(config).await {
        Ok(host) => Some((
            Arc::new(host),
            initial_model,
            initial_provider_id,
            deferred_notes,
        )),
        Err(e) => {
            eprintln!("warning: pool host start failed: {e:#}");
            None
        }
    }
}

async fn start_host_remote(
    url: String,
    model: String,
    project_root: PathBuf,
    tool_bins: &ToolBins,
) -> Result<Arc<Host>> {
    let config = tool_bins.apply(
        HostConfig::new(ProviderEndpoint::StreamableHttp { url }, model)
            .with_project_root(project_root)
            .with_app_version(env!("CARGO_PKG_VERSION")),
    );
    let host = Host::start(config).await.context("failed to start host")?;
    Ok(Arc::new(host))
}

/// Resolve a bundled tool-server binary by name. Tries (in order):
///
/// 1. `<env_override>` env var (must point at an existing file).
/// 2. A sibling of the running TUI executable — i.e. `target/<profile>/`
///    when launched via `cargo run`, or the install dir when installed.
/// 3. Bare `<name>` resolved via `PATH`.
///
/// Returns `None` if none of the candidates exists. The caller surfaces a
/// note so the user knows that tool surface is disabled.
fn locate_bundled_bin(name: &str, env_override: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(env_override) {
        let path = PathBuf::from(p);
        return path.exists().then_some(path);
    }
    let bin_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(&bin_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(&bin_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

async fn current_host(slot: &HostSlot) -> Option<Arc<Host>> {
    slot.read().await.clone()
}

fn init_tracing() {
    // The TUI owns the terminal once it enters ratatui's alternate screen,
    // so tracing must NEVER write to stderr — a single line would corrupt
    // the rendered UI. Route everything to a daily append log under
    // `~/.savvagent/logs/`. If the file can't be opened (no $HOME, perms),
    // fall back to a sink so we still register a global subscriber (and
    // therefore still silence stderr from `tracing` macros) instead of
    // landing back on the default stderr writer.
    let writer: Box<dyn std::io::Write + Send + 'static> = match open_savvagent_log() {
        Ok(file) => Box::new(file),
        Err(_) => Box::new(std::io::sink()),
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(writer))
        .try_init();
}

fn open_savvagent_log() -> std::io::Result<std::fs::File> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is unset"))?;
    let dir = PathBuf::from(home).join(".savvagent").join("logs");
    std::fs::create_dir_all(&dir)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("savvagent.log"))
}

fn transcript_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".savvagent").join("transcripts")
}

async fn save_transcript_now(app: &App, host: &Arc<Host>) -> Result<PathBuf> {
    if app.entries.is_empty() {
        return Ok(PathBuf::new());
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = app.transcript_dir.join(format!("{ts}.json"));
    host.save_transcript(&path)
        .await
        .context("save transcript")?;
    Ok(path)
}

/// Run a slash command and apply its side effects. Used both when the user
/// types `/foo` + Enter and when they pick a no-arg command from the
/// palette. Commands that need direct host access (`/tools`, `/model`,
/// `/bash`, `/sandbox`, `/theme`, `/resume`) are dispatched here first.
/// All other slash commands are routed through the plugin SlashRouter; on
/// `SlashError::Unknown` we fall back to the legacy `App::handle_command`
/// for backwards compatibility with commands not yet ported to plugins.
async fn dispatch_slash_command(
    app: &mut App,
    cmd: &str,
    host_slot: &HostSlot,
    project_root: &Path,
    tool_bins: &ToolBins,
    worker_tx: &mpsc::Sender<WorkerMsg>,
) {
    let trimmed = cmd.trim_start();
    // `/bash` preserves the unparsed remainder verbatim (no `trim()` on
    // `rest`) so quoting like `--net   curl …` survives.
    let (head, rest_raw) = match trimmed.split_once(char::is_whitespace) {
        Some((h, r)) => (h, r),
        None => (trimmed, ""),
    };
    let rest = rest_raw.trim();

    match head {
        "/tools" => {
            show_tools(app, host_slot).await;
            return;
        }
        // Typed-arg invocations (`/model <id>`) and the no-host case still
        // flow through `handle_model_command`'s path: it validates the id
        // against the active provider's capability metadata, rebuilds the
        // host, and surfaces a useful note when nothing is connected yet.
        // With no args AND an active host, the guard is false, the arm
        // doesn't match, and control falls through to the plugin router so
        // the `internal:model` plugin can open the picker screen.
        "/model" if !rest.is_empty() || current_host(host_slot).await.is_none() => {
            handle_model_command(app, rest, host_slot).await;
            return;
        }
        "/resume" => {
            handle_resume_command(app, rest, host_slot).await;
            return;
        }
        "/sandbox" => {
            handle_sandbox_command(app, rest, host_slot).await;
            return;
        }
        "/bash" => {
            handle_bash_slash_command(app, rest_raw, host_slot, worker_tx).await;
            return;
        }
        "/disconnect" => {
            handle_disconnect_command(app, rest, host_slot, worker_tx).await;
            return;
        }
        "/use" => {
            handle_use_command(app, rest, host_slot).await;
            return;
        }
        _ => {}
    }

    // Try the plugin SlashRouter before falling through to the legacy handler.
    // Parse "/cmd arg1 arg2..." into (name, args).
    let name_str = head.strip_prefix('/').unwrap_or(head);
    let args: Vec<String> = rest.split_whitespace().map(String::from).collect();
    if let (Some(reg), Some(idx)) = (&app.plugin_registry, &app.plugin_indexes) {
        let reg = reg.clone();
        let idx = idx.clone();
        let effs_result = {
            let reg_guard = reg.read().await;
            let idx_guard = idx.read().await;
            let router = crate::plugin::slash::SlashRouter::new(&idx_guard, &reg_guard);
            router.dispatch(name_str, args).await
        };
        match effs_result {
            Ok(effs) => {
                if let Err(e) = crate::plugin::effects::apply_effects(app, effs).await {
                    tracing::warn!(error = %e, command = %name_str, "apply_effects after textarea slash dispatch failed");
                    app.push_styled_note(savvagent_plugin::StyledLine::plain(
                        rust_i18n::t!("notes.command-failed", err = format!("{e:#}")).to_string(),
                    ));
                }
                apply_pending_model_change(app, host_slot, project_root, tool_bins).await;
                return;
            }
            Err(crate::plugin::slash::SlashError::Unknown(_)) => {
                // Fall through to legacy handle_command below.
            }
            Err(e) => {
                tracing::warn!(error = %e, command = %name_str, "textarea slash dispatch failed");
                app.push_styled_note(savvagent_plugin::StyledLine::plain(
                    rust_i18n::t!("notes.command-failed", err = format!("{e:#}")).to_string(),
                ));
                return;
            }
        }
    }

    // TODO: remove legacy fallback once all slash commands are plugin-driven.
    app.handle_command(cmd);
}

/// Render `/tools` output: one note per registered tool, with the policy's
/// no-args verdict as a coarse hint.
async fn show_tools(app: &mut App, host_slot: &HostSlot) {
    let Some(host) = current_host(host_slot).await else {
        app.push_note(rust_i18n::t!("notes.not-connected-tools").to_string());
        return;
    };
    let defs = host.tool_defs().await;
    if defs.is_empty() {
        app.push_note(rust_i18n::t!("notes.no-tools-registered").to_string());
        return;
    }
    app.push_note(rust_i18n::t!("notes.tools-count", count = defs.len()).to_string());
    for def in &defs {
        let verdict = host.default_verdict_for(&def.name);
        let label = match verdict {
            savvagent_host::Verdict::Allow => "allow",
            savvagent_host::Verdict::Ask { .. } => "ask",
            savvagent_host::Verdict::Deny { .. } => "deny",
        };
        let desc = if def.description.is_empty() {
            String::new()
        } else {
            format!(" — {}", def.description)
        };
        app.push_note(
            rust_i18n::t!(
                "notes.tools-entry",
                policy = label,
                name = def.name.clone(),
                desc = desc
            )
            .to_string(),
        );
    }
}

/// Validate `requested` against the provider's advertised `models`. Returns
/// `Ok(())` when the id is in the list, `Err(known_ids)` otherwise.
///
/// Used only by unit tests — production validation now goes through
/// [`Host::active_capabilities`] instead of a live `list_models` RPC.
#[cfg(test)]
fn validate_model_id<'a>(
    requested: &str,
    models: &'a [savvagent_host::ModelInfo],
) -> Result<(), Vec<&'a str>> {
    if models.iter().any(|m| m.id == requested) {
        Ok(())
    } else {
        Err(models.iter().map(|m| m.id.as_str()).collect())
    }
}

/// Outcome of asking the provider whether `requested` is a known model id.
///
/// Used only by unit tests — production validation now goes through
/// [`Host::active_capabilities`] instead of a live `list_models` RPC.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
enum ModelChangeOutcome {
    /// Switch to the new model. When `warning` is `Some`, push it as a note
    /// first so the user understands the validation outcome.
    Proceed { warning: Option<String> },
    /// Refuse the change. `note` describes why, including the known model list
    /// when one is available.
    Reject { note: String },
}

/// Decide what to do with a `/model <id>` request given the result of asking
/// the host for `list_models`.
///
/// Pure (no IO, no [`App`] mutation) so it can be unit-tested without standing
/// up a [`Host`] or worker channel.
///
/// Used only by unit tests — production validation now goes through
/// [`Host::active_capabilities`] instead of a live `list_models` RPC.
#[cfg(test)]
fn resolve_model_change(
    requested: &str,
    list_result: Result<&savvagent_host::ListModelsResponse, &savvagent_protocol::ProviderError>,
) -> ModelChangeOutcome {
    match list_result {
        Ok(resp) if resp.models.is_empty() => {
            // The provider advertised the tool but returned no models. Rather
            // than reject every id against an empty "Known: " list, treat
            // this as "nothing to validate against" and proceed.
            ModelChangeOutcome::Proceed {
                warning: Some(
                    rust_i18n::t!("notes.model-no-models-optimistic", model = requested)
                        .to_string(),
                ),
            }
        }
        Ok(resp) => match validate_model_id(requested, &resp.models) {
            Ok(()) => ModelChangeOutcome::Proceed { warning: None },
            Err(known) => ModelChangeOutcome::Reject {
                note: rust_i18n::t!(
                    "notes.model-unknown-id",
                    model = requested,
                    known = known.join(", ")
                )
                .to_string(),
            },
        },
        Err(e) if matches!(e.kind, savvagent_protocol::ErrorKind::NotImplemented) => {
            // The provider doesn't advertise list_models at all. Silent
            // fall-through to the optimistic path is the contract.
            ModelChangeOutcome::Proceed { warning: None }
        }
        Err(e) => {
            // Network/auth/decode failure. Surface it to the user so they can
            // tell a typo'd id apart from a misconfigured key.
            ModelChangeOutcome::Proceed {
                warning: Some(
                    rust_i18n::t!(
                        "notes.model-verify-failed",
                        model = requested,
                        err = e.message.clone()
                    )
                    .to_string(),
                ),
            }
        }
    }
}

/// `/model` (no args) shows the current model. `/model <id>` validates the
/// requested id against the active provider's capability metadata and then
/// reconnects the active provider with the new id. When the pool has no
/// models registered for the active provider the check is skipped and the
/// provider rejects an invalid id at first turn instead.
async fn handle_model_command(app: &mut App, rest: &str, host_slot: &HostSlot) {
    if rest.is_empty() {
        match app.active_provider_id {
            Some(id) => app.push_note(
                rust_i18n::t!(
                    "notes.model-current-connected",
                    provider = id,
                    model = app.model.clone()
                )
                .to_string(),
            ),
            None => app.push_note(
                rust_i18n::t!(
                    "notes.model-current-not-connected",
                    model = app.model.clone()
                )
                .to_string(),
            ),
        }
        return;
    }

    let new_model = rest.to_string();
    let Some(spec_id) = app.active_provider_id else {
        app.push_note(rust_i18n::t!("notes.model-not-connected").to_string());
        return;
    };
    if PROVIDERS.iter().all(|s| s.id != spec_id) {
        app.push_note(rust_i18n::t!("notes.model-unknown-provider", id = spec_id).to_string());
        return;
    }

    // Validate the requested id against the active provider's capability
    // metadata. When the pool has no capabilities registered for the active
    // provider (shouldn't happen in practice) we fall through optimistically.
    if let Some(host) = current_host(host_slot).await {
        if let Some(caps) = host.active_capabilities().await {
            if !caps.models().is_empty() && caps.model(&new_model).is_none() {
                let active = host.active_provider().await;
                app.push_note(
                    rust_i18n::t!(
                        "notes.model-not-in-active",
                        id = new_model,
                        provider = active.as_str()
                    )
                    .to_string(),
                );
                return;
            }
        }
    }

    // Persist immediately for the direct /model <id> command path.
    if let Some(spec) = PROVIDERS.iter().find(|s| s.id == spec_id) {
        match models_pref::save_for_provider(spec.id, &new_model).await {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(error = ?e, provider = spec.id, "models.toml save failed");
                app.push_note(
                    rust_i18n::t!("notes.model-pref-save-failed", err = format!("{e:#}"))
                        .to_string(),
                );
            }
        }
    }

    perform_model_change(new_model, host_slot, app).await;
    refresh_cached_models(app, host_slot).await;
}

/// `/resume` with no args opens the transcript picker. With a path arg,
/// loads the transcript immediately. Requires an active host connection;
/// if none exists, surfaces a clear error.
///
/// Refuses to run while a turn is in flight: `Host::run_turn_inner`
/// snapshots `state.messages` at turn start and commits its local clone
/// back at turn end, so a mid-turn `load_transcript` would be silently
/// overwritten when the in-flight turn finishes.
async fn handle_resume_command(app: &mut App, rest: &str, host_slot: &HostSlot) {
    if app.is_loading {
        app.push_note(rust_i18n::t!("notes.cannot-resume-during-turn").to_string());
        return;
    }
    if rest.is_empty() {
        // Open the picker — actual load happens when the user presses Enter
        // in `SelectingTranscript` mode (handled in `run_app`).
        app.open_transcript_picker(&app.transcript_dir.clone());
        return;
    }

    // Inline path argument: /resume <path>.
    let path = {
        let p = PathBuf::from(rest);
        if p.is_absolute() {
            p
        } else {
            // Treat bare names like "1715340000" or "1715340000.json" as
            // relative to the transcript directory.
            let candidate = app.transcript_dir.join(rest);
            if candidate.exists() {
                candidate
            } else {
                // Try adding .json extension.
                let with_ext = app.transcript_dir.join(format!("{rest}.json"));
                if with_ext.exists() { with_ext } else { p }
            }
        }
    };
    do_resume_from_path(app, host_slot, &path).await;
}

/// Load a transcript from `path` into the active host and replay it into
/// the conversation log.
async fn do_resume_from_path(app: &mut App, host_slot: &HostSlot, path: &Path) {
    let Some(host) = current_host(host_slot).await else {
        app.push_note(rust_i18n::t!("notes.cannot-resume-not-connected").to_string());
        return;
    };

    match host.load_transcript(path).await {
        Ok(record) => {
            // Warn if the transcript was from a different model.
            if record.model != host.config().model && record.saved_at > 0 {
                app.push_note(
                    rust_i18n::t!(
                        "notes.resume-model-mismatch",
                        saved = record.model.clone(),
                        current = host.config().model.clone()
                    )
                    .to_string(),
                );
            }
            let ts = if record.saved_at > 0 {
                collect_transcript_entries(path.parent().unwrap_or(path))
                    .into_iter()
                    .find(|e| e.path == path)
                    .map(|e| e.timestamp)
                    .unwrap_or_else(|| record.saved_at.to_string())
            } else {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_owned()
            };
            app.replay_transcript(&record);
            app.resumed_at = Some(ts.clone());
            app.push_note(
                rust_i18n::t!(
                    "notes.resume-resumed",
                    ts = ts,
                    count = record.messages.len()
                )
                .to_string(),
            );
        }
        Err(TranscriptError::SchemaMismatch { found, expected }) => {
            app.push_note(
                rust_i18n::t!(
                    "notes.resume-schema-mismatch",
                    found = found,
                    expected = expected
                )
                .to_string(),
            );
        }
        Err(TranscriptError::Malformed(msg)) => {
            app.push_note(rust_i18n::t!("notes.resume-malformed-json", msg = msg).to_string());
        }
        Err(TranscriptError::Io(e)) => {
            app.push_note(
                rust_i18n::t!("notes.resume-io-error", err = format!("{e:#}")).to_string(),
            );
        }
    }
}

/// Refresh [`App::cached_models`] from the active provider's capability
/// metadata. Only models from the currently-active provider are shown in
/// the picker; switching providers (`/use`) triggers a fresh refresh.
///
/// Falls back to a single-entry catalog containing the current model id when
/// no host is connected or the active provider has no models registered, so
/// the picker always has something to render.
async fn refresh_cached_models(app: &mut App, host_slot: &HostSlot) {
    let fallback = vec![savvagent_plugin::ModelEntry {
        id: app.model.clone(),
        display_name: app.model.clone(),
    }];
    let Some(host) = current_host(host_slot).await else {
        app.cached_models = fallback;
        return;
    };
    let Some(caps) = host.active_capabilities().await else {
        tracing::debug!("active_capabilities returned None; using single-entry fallback");
        app.cached_models = fallback;
        return;
    };
    if caps.models().is_empty() {
        tracing::debug!("active provider has no models in capabilities; using fallback");
        app.cached_models = fallback;
        return;
    }
    app.cached_models = caps
        .models()
        .iter()
        .map(|m| savvagent_plugin::ModelEntry {
            display_name: if m.display_name.is_empty() {
                m.id.clone()
            } else {
                m.display_name.clone()
            },
            id: m.id.clone(),
        })
        .collect();
}

/// Drain `app.pending_model_change` (set by `Effect::SetActiveModel`)
/// and forward the request to [`perform_model_change`]. No-op when
/// nothing is queued.
async fn apply_pending_model_change(
    app: &mut App,
    host_slot: &HostSlot,
    _project_root: &Path,
    _tool_bins: &ToolBins,
) {
    let Some(pending) = app.pending_model_change.take() else {
        return;
    };
    let Some(spec_id) = app.active_provider_id else {
        app.push_note(rust_i18n::t!("notes.model-not-connected").to_string());
        return;
    };
    let Some(spec) = PROVIDERS.iter().find(|s| s.id == spec_id) else {
        app.push_note(rust_i18n::t!("notes.model-unknown-provider", id = spec_id).to_string());
        return;
    };

    // Validate against the active provider's capability metadata. The picker
    // is already filtered to the active provider's models, so this is a
    // belt-and-suspenders check. When no capabilities are registered we
    // proceed optimistically — the provider rejects an invalid id at first
    // turn instead.
    if let Some(host) = current_host(host_slot).await {
        if let Some(caps) = host.active_capabilities().await {
            if !caps.models().is_empty() && caps.model(&pending.id).is_none() {
                let active = host.active_provider().await;
                app.push_note(
                    rust_i18n::t!(
                        "notes.model-not-in-active",
                        id = pending.id,
                        provider = active.as_str()
                    )
                    .to_string(),
                );
                return;
            }
        }
    }

    perform_model_change(pending.id.clone(), host_slot, app).await;

    // Refresh the picker's catalog cache; if the new model came with a
    // larger advertised set than the previous one, the picker should
    // see it on next open.
    refresh_cached_models(app, host_slot).await;

    // Persist only when the requesting effect asked for it.
    if pending.persist {
        match models_pref::save_for_provider(spec.id, &pending.id).await {
            Ok(()) => app
                .push_note(rust_i18n::t!("notes.model-persisted", provider = spec.id).to_string()),
            Err(e) => {
                tracing::warn!(error = ?e, provider = spec.id,
                    "models.toml save failed");
                app.push_note(
                    rust_i18n::t!("notes.model-pref-save-failed", err = format!("{e:#}"))
                        .to_string(),
                );
            }
        }
    }
}

/// Resolve the effective model id for `provider_id`, applying the
/// precedence: `SAVVAGENT_MODEL` env var (highest) > persisted in
/// `~/.savvagent/models.toml` > `spec.default_model`.
fn resolve_initial_model_for(spec: &ProviderSpec) -> String {
    if let Ok(env_model) = std::env::var("SAVVAGENT_MODEL") {
        if !env_model.is_empty() {
            return env_model;
        }
    }
    let pref = models_pref::ModelsPref::load();
    if let Some(persisted) = pref.get(spec.id) {
        return persisted.to_string();
    }
    spec.default_model.to_string()
}

/// Update the active model on the existing host without rebuilding it.
///
/// The pool is untouched — only the model field forwarded in future
/// `CompleteRequest`s changes. History and tool state are preserved.
/// Persistence to `~/.savvagent/models.toml` is the caller's responsibility
/// (both call sites — `handle_model_command` and `apply_pending_model_change`
/// — already handle that separately).
async fn perform_model_change(new_model: String, host_slot: &HostSlot, app: &mut App) {
    if let Some(host) = current_host(host_slot).await {
        host.set_model(new_model.clone()).await;
    }
    app.model = new_model;
    app.push_note(rust_i18n::t!("notes.model-is-now", model = app.model.clone()).to_string());
}

/// `/sandbox` (no args) shows current status.
/// `/sandbox on` / `/sandbox off` toggles the enabled flag and persists to
/// `~/.savvagent/sandbox.toml`.
///
/// Note: changing the setting takes effect the *next* time a host is built
/// (i.e. after `/connect`), because the sandbox is applied at tool spawn time
/// and tools are already running. The status display reflects the *current*
/// host's sandbox config (what was used when tools were spawned).
async fn handle_sandbox_command(app: &mut App, rest: &str, host_slot: &HostSlot) {
    match rest {
        "on" | "off" => {
            // Both subcommands set an *explicit* mode — the splash uses that
            // signal to suppress its v0.7-style nag banner.
            let new_mode = if rest == "on" {
                SandboxMode::On
            } else {
                SandboxMode::Off
            };
            let mut cfg = SandboxConfig::load();
            cfg.mode = new_mode;
            match cfg.save().await {
                Ok(()) => {
                    app.push_note(
                        rust_i18n::t!(
                            "notes.sandbox-enabled",
                            state = if new_mode == SandboxMode::On {
                                "enabled"
                            } else {
                                "disabled"
                            }
                        )
                        .to_string(),
                    );
                }
                Err(e) => {
                    app.push_note(
                        rust_i18n::t!("notes.sandbox-config-save-failed", err = format!("{e:#}"))
                            .to_string(),
                    );
                }
            }
        }
        "" => {
            // Show status from the currently-connected host.
            match current_host(host_slot).await {
                None => {
                    // No active host — show what's on disk instead.
                    let cfg = SandboxConfig::load();
                    app.push_note(
                        rust_i18n::t!(
                            "notes.sandbox-status-on-disk",
                            enabled = cfg.is_enabled().to_string(),
                            allow_net = cfg.allow_net.to_string(),
                            overrides = fmt_overrides(&cfg)
                        )
                        .to_string(),
                    );
                }
                Some(host) => {
                    let cfg = host.sandbox_config();
                    app.push_note(
                        rust_i18n::t!(
                            "notes.sandbox-status-active",
                            enabled = cfg.is_enabled().to_string(),
                            allow_net = cfg.allow_net.to_string(),
                            overrides = fmt_overrides(cfg)
                        )
                        .to_string(),
                    );
                    if cfg.is_enabled() {
                        #[cfg(target_os = "linux")]
                        app.push_note(rust_i18n::t!("notes.sandbox-wrapper-linux").to_string());
                        #[cfg(target_os = "macos")]
                        app.push_note(rust_i18n::t!("notes.sandbox-wrapper-macos").to_string());
                        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                        app.push_note(rust_i18n::t!("notes.sandbox-wrapper-windows").to_string());
                    }
                    if !cfg.extra_binds.is_empty() {
                        app.push_note(format!(
                            "  extra_binds: {}",
                            cfg.extra_binds
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
            }
        }
        other => {
            app.push_note(
                rust_i18n::t!("notes.sandbox-unknown-subcommand", sub = other).to_string(),
            );
        }
    }
}

/// `/disconnect <provider> [--force]` — remove a provider from the pool.
///
/// Without `--force`, uses [`DisconnectMode::Drain`]: new turns cannot
/// acquire this provider, but any in-flight turn is allowed to finish
/// naturally. With `--force`, uses [`DisconnectMode::Force`]: sends a
/// cooperative cancel and, if the grace period expires, hard-aborts every
/// registered in-flight task on that provider.
///
/// The actual `remove_provider` call is fire-and-forget via
/// `tokio::spawn` so the TUI input stays responsive during a long drain.
async fn handle_disconnect_command(
    app: &mut App,
    rest: &str,
    host_slot: &HostSlot,
    worker_tx: &mpsc::Sender<WorkerMsg>,
) {
    let mut tokens = rest.split_whitespace();
    let Some(provider) = tokens.next() else {
        app.push_note(rust_i18n::t!("notes.disconnect-needs-provider").to_string());
        return;
    };
    let force = tokens.any(|t| t == "--force");
    let Some(host) = current_host(host_slot).await else {
        app.push_note(rust_i18n::t!("notes.disconnect-no-host").to_string());
        return;
    };
    let pid = match savvagent_protocol::ProviderId::new(provider) {
        Ok(p) => p,
        Err(_) => {
            app.push_note(rust_i18n::t!("notes.disconnect-invalid-id", id = provider).to_string());
            return;
        }
    };
    if !host.is_connected(pid.as_str()).await {
        app.push_note(rust_i18n::t!("notes.disconnect-not-connected", name = provider).to_string());
        return;
    }
    let mode = if force {
        savvagent_host::DisconnectMode::Force
    } else {
        savvagent_host::DisconnectMode::Drain
    };
    let mode_label = if force { "force" } else { "drain" };
    app.push_note(
        rust_i18n::t!(
            "notes.disconnect-starting",
            name = provider,
            mode = mode_label
        )
        .to_string(),
    );
    let host_clone = std::sync::Arc::clone(&host);
    let provider_owned = provider.to_string();
    let mode_label_owned = mode_label.to_string();
    let tx = worker_tx.clone();
    tokio::spawn(async move {
        match host_clone.remove_provider(&pid, mode).await {
            Ok(()) => {
                let _ = tx
                    .send(WorkerMsg::DisconnectCompleted {
                        provider: provider_owned,
                        mode: mode_label_owned,
                    })
                    .await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "remove_provider failed in /disconnect handler");
                let _ = tx
                    .send(WorkerMsg::DisconnectFailed {
                        provider: provider_owned,
                        err: e.to_string(),
                    })
                    .await;
            }
        }
    });
}

async fn handle_use_command(app: &mut App, rest: &str, host_slot: &HostSlot) {
    let provider = rest.split_whitespace().next().unwrap_or("");
    if provider.is_empty() {
        app.push_note(rust_i18n::t!("notes.use-needs-provider").to_string());
        return;
    }
    let Some(host) = current_host(host_slot).await else {
        app.push_note(rust_i18n::t!("notes.use-no-host").to_string());
        return;
    };
    let pid = match savvagent_protocol::ProviderId::new(provider) {
        Ok(p) => p,
        Err(_) => {
            app.push_note(rust_i18n::t!("notes.use-invalid-id", id = provider).to_string());
            return;
        }
    };
    match host.set_active_provider(&pid).await {
        Ok(()) => {
            // History is already cleared on the host side; reset the
            // TUI's transcript view to match.
            app.entries.clear();
            app.live_text.clear();
            app.update_metrics();
            // Sync app.active_provider_id and app.model to the new active
            // provider so every subsequent site that branches on
            // app.active_provider_id sees the correct value.
            let spec = PROVIDERS.iter().find(|s| s.id == provider);
            if let Some(spec) = spec {
                app.active_provider_id = Some(spec.id);
                app.model = resolve_initial_model_for(spec);
                refresh_cached_models(app, host_slot).await;
            } else {
                // Shouldn't happen — set_active_provider already verified the
                // id exists in the pool — but defense-in-depth: log and skip
                // the model refresh rather than panicking.
                tracing::warn!(
                    provider = provider,
                    "handle_use_command: provider id not found in PROVIDERS; \
                     active_provider_id and model not updated"
                );
            }
            // Notify provider plugins so they can flip their active
            // marker in render_slot without polling.
            if let Err(err) = crate::plugin::effects::dispatch_host_event(
                app,
                savvagent_plugin::HostEvent::ActiveProviderChanged { id: pid.clone() },
                0,
            )
            .await
            {
                tracing::warn!(error = %err, "ActiveProviderChanged dispatch failed");
            }
            app.push_note(rust_i18n::t!("notes.use-switched", name = provider).to_string());
        }
        Err(savvagent_host::PoolError::NotRegistered(_)) => {
            app.push_note(rust_i18n::t!("notes.use-not-connected", name = provider).to_string());
        }
        Err(e) => {
            app.push_note(format!("{e}"));
        }
    }
}

/// `/bash <cmd>` — run `cmd` through `tool-bash` without round-tripping
/// through the provider. `--net` / `--no-net` flags at the front of `rest`
/// override the bash-network policy for this call only.
///
/// The call is dispatched on a worker task; its
/// [`TurnEvent`]s — most importantly any
/// [`TurnEvent::BashNetworkRequested`] the resolver emits — are forwarded
/// to the main loop's worker channel so the modal flow stays unchanged.
async fn handle_bash_slash_command(
    app: &mut App,
    rest_raw: &str,
    host_slot: &HostSlot,
    worker_tx: &mpsc::Sender<WorkerMsg>,
) {
    let parsed = match parse_bash_command(rest_raw) {
        Ok(p) => p,
        Err(BashCommandError::UnknownFlag { token }) => {
            app.push_note(rust_i18n::t!("notes.bash-flag-unknown", token = token).to_string());
            return;
        }
        Err(_) => {
            app.push_note(rust_i18n::t!("notes.bash-usage").to_string());
            return;
        }
    };
    let Some(host) = current_host(host_slot).await else {
        app.push_note(rust_i18n::t!("notes.not-connected-bash").to_string());
        return;
    };
    // Pre-flight: confirm tool-bash is actually configured. Without this
    // check the call falls through to `call_with_bash_net_override` and
    // surfaces as the opaque "unknown tool: run" error, which gives the
    // user no actionable repair path. The `run` tool is the contract
    // surface tool-bash advertises.
    let bash_configured = host.tool_defs().await.iter().any(|t| t.name == "run");
    if !bash_configured {
        app.push_note(rust_i18n::t!("notes.bash-not-configured").to_string());
        return;
    }
    if app.is_loading {
        app.push_note(rust_i18n::t!("notes.cannot-bash-during-turn").to_string());
        return;
    }

    // Surface the invocation in the transcript so its eventual result is
    // attached to a visible Tool entry (matches how model-driven calls
    // render).
    app.entries.push(Entry::Tool {
        name: "run".to_string(),
        arguments: format!("/bash {}", parsed.command),
        status: None,
        result_preview: None,
    });
    app.is_loading = true;
    app.update_metrics();

    let tx = worker_tx.clone();
    let command = parsed.command.clone();
    let net_override = parsed.net_override;
    tokio::spawn(async move {
        let (ev_tx, mut ev_rx) = mpsc::channel::<TurnEvent>(8);
        let host_for_run = host.clone();
        let runner = tokio::spawn(async move {
            host_for_run
                .run_bash_command(&command, net_override, Some(ev_tx))
                .await
        });
        // Forward bash-network prompt events into the main loop. The
        // forwarder exits when `ev_tx` is dropped (i.e. the runner
        // returns).
        let forwarder_tx = tx.clone();
        let forwarder = tokio::spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                if forwarder_tx.send(WorkerMsg::Event(ev)).await.is_err() {
                    break;
                }
            }
        });
        let outcome = runner.await;
        // Forwarder will drain on ev_tx drop; join it so we don't race a
        // dangling event past the final ToolCallFinished.
        let _ = forwarder.await;
        match outcome {
            Ok(Ok((is_error, payload))) => {
                let status = if is_error {
                    ToolCallStatus::Errored
                } else {
                    ToolCallStatus::Ok
                };
                let _ = tx
                    .send(WorkerMsg::Event(TurnEvent::ToolCallFinished {
                        name: "run".into(),
                        status,
                        result: payload,
                    }))
                    .await;
                let _ = tx.send(WorkerMsg::BashDone).await;
            }
            Ok(Err(msg)) => {
                let _ = tx.send(WorkerMsg::Error(format!("/bash: {msg}"))).await;
                let _ = tx.send(WorkerMsg::BashDone).await;
            }
            Err(join_err) => {
                let _ = tx
                    .send(WorkerMsg::Error(format!("/bash worker failed: {join_err}")))
                    .await;
                let _ = tx.send(WorkerMsg::BashDone).await;
            }
        }
    });
}

fn fmt_overrides(cfg: &SandboxConfig) -> String {
    if cfg.tool_overrides.is_empty() {
        return "(none)".into();
    }
    cfg.tool_overrides
        .iter()
        .map(|(k, v)| {
            let net = match v.allow_net {
                Some(true) => "net=yes",
                Some(false) => "net=no",
                None => "net=inherit",
            };
            format!("{k}({net})")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Persist the key (if required), build the in-process handler, swap the host.
async fn perform_connect(
    spec: &'static ProviderSpec,
    api_key: String,
    host_slot: &HostSlot,
    project_root: &Path,
    tool_bins: &ToolBins,
    app: &mut App,
) {
    use crate::plugin::builtin::{
        provider_anthropic::ProviderAnthropicPlugin, provider_gemini::ProviderGeminiPlugin,
        provider_local::ProviderLocalPlugin, provider_openai::ProviderOpenAiPlugin,
    };

    // 1. Persist the key so the plugin can read it back via keyring.
    if spec.api_key_required {
        if let Err(e) = creds::save(spec.id, &api_key) {
            app.push_note(
                rust_i18n::t!("notes.keyring-store-failed", err = format!("{e:#}")).to_string(),
            );
            return;
        }
    }

    app.push_note(rust_i18n::t!("notes.connecting-to", name = spec.display_name).to_string());

    // 2. Build the ProviderRegistration via the matching plugin. The plugin
    //    reads the key from the keyring; we just saved it above so the read
    //    will succeed. Surface any build failure with a note.
    let reg_result = match spec.id {
        "anthropic" => {
            ProviderAnthropicPlugin::new()
                .try_build_registration()
                .await
        }
        "gemini" => ProviderGeminiPlugin::new().try_build_registration().await,
        "openai" => ProviderOpenAiPlugin::new().try_build_registration().await,
        "local" => ProviderLocalPlugin::new().try_build_registration().await,
        other => {
            app.push_note(rust_i18n::t!("notes.connect-unknown-provider", id = other).to_string());
            return;
        }
    };
    let reg = match reg_result {
        Ok(Some(r)) => r,
        Ok(None) => {
            // Keyring read returned None despite the just-saved key — likely
            // a backend issue.
            app.push_note(
                rust_i18n::t!("notes.connect-keyring-not-found", id = spec.id).to_string(),
            );
            return;
        }
        Err(e) => {
            app.push_note(
                rust_i18n::t!("notes.connect-failed", id = spec.id, err = format!("{e:#}"))
                    .to_string(),
            );
            return;
        }
    };

    // 3. Add to the pool, or build a first host when no host exists yet.
    //    The pool is additive — no history clear, no host replacement.
    let is_first_connect = current_host(host_slot).await.is_none();
    if is_first_connect {
        // No host yet — startup produced no registrations (e.g. user
        // dismissed the migration picker with startup_providers = []).
        // Build a fresh single-entry pool host.
        let mut cfg = tool_bins.apply(
            HostConfig::new(
                ProviderEndpoint::StreamableHttp {
                    url: "inproc://pool".into(),
                },
                resolve_initial_model_for(spec),
            )
            .with_project_root(project_root.to_path_buf())
            .with_app_version(env!("CARGO_PKG_VERSION")),
        );
        cfg.providers = vec![reg];
        cfg.startup_connect = savvagent_host::StartupConnectPolicy::All;
        match Host::start(cfg).await {
            Ok(h) => {
                *host_slot.write().await = Some(Arc::new(h));
            }
            Err(e) => {
                app.push_note(
                    rust_i18n::t!("notes.connect-failed", id = spec.id, err = format!("{e:#}"))
                        .to_string(),
                );
                return;
            }
        }
    } else {
        // Pool already exists — add this provider to it additively.
        let host = current_host(host_slot).await.expect("checked above");
        match host.add_provider(reg).await {
            Ok(()) => {}
            Err(savvagent_host::PoolError::AlreadyRegistered(_)) => {
                app.push_note(
                    rust_i18n::t!("notes.connect-already", name = spec.display_name).to_string(),
                );
                return;
            }
            Err(e) => {
                app.push_note(
                    rust_i18n::t!("notes.connect-failed", id = spec.id, err = format!("{e}"))
                        .to_string(),
                );
                return;
            }
        }
    }

    // 4. Update TUI state. Pool is additive — do NOT clear app.entries,
    //    live_text, or call clear_history. The conversation continues on the
    //    existing active provider.
    app.connected = true;
    if app.active_provider_id.is_none() {
        // First connection — set this provider as active, resolve the model,
        // and refresh caches.
        app.active_provider_id = Some(spec.id);
        app.model = resolve_initial_model_for(spec);
        refresh_cached_models(app, host_slot).await;
        // Align the splash sandbox indicator with the newly-started host.
        if let Some(host) = current_host(host_slot).await {
            app.refresh_splash_sandbox_from_host(host.sandbox_config());
        }
        // Tell provider plugins to flip their active marker.
        if let Ok(pid) = savvagent_plugin::ProviderId::new(spec.id) {
            if let Err(err) = crate::plugin::effects::dispatch_host_event(
                app,
                savvagent_plugin::HostEvent::ActiveProviderChanged { id: pid },
                0,
            )
            .await
            {
                tracing::warn!(error = %err, "ActiveProviderChanged dispatch from perform_connect failed");
            }
        }
    }
    app.push_note(rust_i18n::t!("notes.connected-to", name = spec.display_name).to_string());

    // 5. Dispatch ProviderRegistered + Connect for plugin subscribers.
    //
    // The `/connect <provider>` slash path emits `ProviderRegistered` +
    // `Connect` via `Effect::RegisterProvider`, but this legacy in-TUI
    // provider-picker flow registers directly — so without these dispatches
    // the splash HUD never flips to "Connected" and anything else subscribed
    // to `Connect` (telemetry, status indicators) is silently skipped.
    // Errors are warn-only so a buggy subscriber can't tank the connect.
    match savvagent_plugin::ProviderId::new(spec.id) {
        Ok(provider_id) => {
            if let Err(err) = crate::plugin::effects::dispatch_host_event(
                app,
                savvagent_plugin::HostEvent::ProviderRegistered {
                    id: provider_id.clone(),
                    display_name: spec.display_name.to_string(),
                },
                0,
            )
            .await
            {
                tracing::warn!(error = %err,
                    "ProviderRegistered dispatch from perform_connect failed");
            }
            if let Err(err) = crate::plugin::effects::dispatch_host_event(
                app,
                savvagent_plugin::HostEvent::Connect { provider_id },
                0,
            )
            .await
            {
                tracing::warn!(error = %err,
                    "Connect dispatch from perform_connect failed");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, provider_id = spec.id,
                "perform_connect: invalid provider id; skipping HostEvent dispatch");
        }
    }
}

/// Translate a streaming [`TurnEvent`] from the host into the corresponding
/// [`savvagent_plugin::HostEvent`], if any. Several `TurnEvent` variants
/// have no host-event analog (`IterationStarted` after the first,
/// `TextDelta`, `PermissionRequested`, `BashNetworkRequested`,
/// `ToolCallDenied`) and return `None`.
///
/// Mutates the four event-loop counters to keep turn/tool-call ids
/// monotonic and matched across Start/End pairs. The strict-sequential
/// nature of `Host::run_turn_inner` (tool calls don't interleave per
/// turn) makes a single `last_tool_call_id` slot sufficient.
fn translate_turn_event_to_host_event(
    event: &TurnEvent,
    next_turn_id: &mut u32,
    current_turn_id: &mut Option<u32>,
    next_tool_call_id: &mut u64,
    last_tool_call_id: &mut Option<u64>,
) -> Option<savvagent_plugin::HostEvent> {
    match event {
        TurnEvent::IterationStarted { iteration } => {
            if *iteration == 1 && current_turn_id.is_none() {
                *next_turn_id = next_turn_id.saturating_add(1);
                *current_turn_id = Some(*next_turn_id);
                Some(savvagent_plugin::HostEvent::TurnStart {
                    turn_id: *next_turn_id,
                })
            } else {
                None
            }
        }
        TurnEvent::ToolCallStarted { name, .. } => {
            *next_tool_call_id = next_tool_call_id.saturating_add(1);
            *last_tool_call_id = Some(*next_tool_call_id);
            Some(savvagent_plugin::HostEvent::ToolCallStart {
                call_id: next_tool_call_id.to_string(),
                tool: name.clone(),
            })
        }
        TurnEvent::ToolCallFinished { status, .. } => {
            // If we never saw a matching `ToolCallStarted`, this is a
            // synthetic `ToolCallFinished` synthesized by the `/bash`
            // direct-invocation path (`handle_bash_slash_command` skips
            // emitting `ToolCallStarted` because the host doesn't go
            // through `run_turn_streaming` there). Don't fabricate a
            // `call_id: "0"` orphan HostEvent — warn-log and skip
            // emission entirely.
            let Some(call_id) = last_tool_call_id.take() else {
                tracing::warn!(
                    "ToolCallFinished with no matching ToolCallStarted — skipping HostEvent \
                     emission (likely a /bash direct invocation)"
                );
                return None;
            };
            Some(savvagent_plugin::HostEvent::ToolCallEnd {
                call_id: call_id.to_string(),
                success: matches!(status, ToolCallStatus::Ok),
            })
        }
        TurnEvent::TurnComplete { .. } => {
            // Clear stale per-turn tool-call state so a future
            // `ToolCallFinished` without a matching `ToolCallStarted`
            // (e.g. via `/bash`) can't pick up a leaked id from the
            // previous turn.
            *last_tool_call_id = None;
            let turn_id = current_turn_id.take().unwrap_or(0);
            Some(savvagent_plugin::HostEvent::TurnEnd {
                turn_id,
                success: true,
            })
        }
        // No analog — these stay TUI-private.
        TurnEvent::RouteSelected { .. }
        | TurnEvent::TextDelta { .. }
        | TurnEvent::PermissionRequested { .. }
        | TurnEvent::BashNetworkRequested { .. }
        | TurnEvent::ToolCallDenied { .. }
        | TurnEvent::Cancelled { .. }
        | TurnEvent::AbortedAfterGrace { .. } => None,
    }
}

/// If `app.context_size` (the chars/4 estimate) has moved since the last
/// emission, fire `HostEvent::ContextSizeChanged` so footer/status
/// plugins can rerender their `~N ctx` segment without polling. Called
/// once per event-loop iteration from `run_app`, just before render.
///
/// Errors are warn-only — a buggy subscriber must not block the loop.
async fn maybe_emit_context_changed(app: &mut App, last_emitted: &mut u32) {
    let current = app.context_size as u32;
    if current == *last_emitted {
        return;
    }
    *last_emitted = current;
    if let Err(err) = crate::plugin::effects::dispatch_host_event(
        app,
        savvagent_plugin::HostEvent::ContextSizeChanged { tokens: current },
        0,
    )
    .await
    {
        tracing::warn!(error = %err, "ContextSizeChanged dispatch failed");
    }
}

async fn run_app(
    terminal: &mut tui::Tui,
    app: &mut App,
    host_slot: HostSlot,
    project_root: PathBuf,
    tool_bins: ToolBins,
) -> Result<()> {
    let (worker_tx, mut worker_rx) = mpsc::channel::<WorkerMsg>(128);

    // PR 7: HostEvent emission state. These counters live here, not on
    // `App`, because they're a property of the host→plugin event stream
    // and only the event-loop driver knows the right moment to mint a
    // fresh id. They're plain `u32`/`u64`; nothing else mutates them.
    //
    // - `next_turn_id`: incremented at each new turn (the first
    //   `IterationStarted` after `current_turn_id` is `None`).
    // - `current_turn_id`: the id assigned to the in-flight turn, used to
    //   match `TurnStart`/`TurnEnd` payloads. Cleared on TurnComplete /
    //   WorkerMsg::Error.
    // - `next_tool_call_id`: minted per `ToolCallStarted`.
    // - `last_tool_call_id`: tracks the most recent unfinished tool call so
    //   that the matching `ToolCallFinished` emits the same `call_id`.
    //   Tool calls are serialized per-turn inside `run_turn_inner`, so a
    //   single "last" slot is sufficient — no interleaving to worry about.
    // - `last_emitted_ctx`: tracks the most recent `ContextSizeChanged`
    //   payload so we only emit when the value actually moves.
    let mut next_turn_id: u32 = 0;
    let mut current_turn_id: Option<u32> = None;
    let mut next_tool_call_id: u64 = 0;
    let mut last_tool_call_id: Option<u64> = None;
    let mut last_emitted_ctx: u32 = 0;

    // Emit `HostEvent::HostStarting` exactly once. Subscribers (e.g.
    // future providers' auto-probe wiring) get one shot at startup.
    if let Err(e) = crate::plugin::effects::dispatch_host_event(
        app,
        savvagent_plugin::HostEvent::HostStarting,
        0,
    )
    .await
    {
        tracing::warn!(error = %e, "HostStarting dispatch failed");
    }
    // If there is an initial active provider (bootstrap succeeded),
    // notify plugins immediately so their render_slot shows the `▸`
    // marker before the user's first interaction. Without this,
    // `ActiveProviderChanged` would only fire on the first `/use`
    // invocation, leaving the footer unmarked on startup.
    if let Some(initial_pid) = app.active_provider_id {
        match savvagent_plugin::ProviderId::new(initial_pid) {
            Ok(id) => {
                if let Err(e) = crate::plugin::effects::dispatch_host_event(
                    app,
                    savvagent_plugin::HostEvent::ActiveProviderChanged { id },
                    0,
                )
                .await
                {
                    tracing::warn!(error = %e, "startup ActiveProviderChanged dispatch failed");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "startup ActiveProviderChanged: invalid provider id");
            }
        }
    }

    loop {
        // Fire ContextSizeChanged whenever the chars/4 estimate moves so
        // home_footer (and any future status-line subscriber) can refresh
        // its `~N ctx` segment without polling. Cheap when unchanged.
        maybe_emit_context_changed(app, &mut last_emitted_ctx).await;

        let frame_area = terminal.get_frame().area();
        let frame_data = ui::compute_home_frame_data(app, frame_area).await;
        terminal.draw(|f| ui::render(app, f, &frame_data))?;

        while let Ok(msg) = worker_rx.try_recv() {
            match msg {
                WorkerMsg::Event(e) => {
                    let was_complete = matches!(e, TurnEvent::TurnComplete { .. });
                    // Translate the streaming TurnEvent before
                    // `apply_turn_event` (which consumes `e` by value)
                    // so the translator and the App mutation each get
                    // their own borrow. The hook fires AFTER
                    // `apply_turn_event` + `update_metrics`, so
                    // subscribers observe the post-mutation App view —
                    // which is what telemetry/render/transcript
                    // subscribers actually want (they need the latest
                    // text buffer, metrics, and entry list).
                    let host_event = translate_turn_event_to_host_event(
                        &e,
                        &mut next_turn_id,
                        &mut current_turn_id,
                        &mut next_tool_call_id,
                        &mut last_tool_call_id,
                    );
                    app.apply_turn_event(e);
                    app.update_metrics();
                    if let Some(he) = host_event {
                        if let Err(err) =
                            crate::plugin::effects::dispatch_host_event(app, he, 0).await
                        {
                            tracing::warn!(error = %err,
                                "host-event dispatch (from TurnEvent) failed");
                        }
                    }
                    if was_complete {
                        if let Some(host) = current_host(&host_slot).await {
                            if let Ok(path) = save_transcript_now(app, &host).await {
                                if !path.as_os_str().is_empty() {
                                    let saved_path = path.to_string_lossy().into_owned();
                                    app.last_transcript = Some(path);
                                    if let Err(err) = crate::plugin::effects::dispatch_host_event(
                                        app,
                                        savvagent_plugin::HostEvent::TranscriptSaved {
                                            path: saved_path,
                                        },
                                        0,
                                    )
                                    .await
                                    {
                                        tracing::warn!(error = %err,
                                            "TranscriptSaved dispatch failed");
                                    }
                                }
                            }
                        }
                    }
                }
                WorkerMsg::Error(msg) => {
                    app.is_loading = false;
                    app.entries.push(Entry::Note(format!("Error: {msg}")));
                    app.update_metrics();
                    // A runner error terminates the turn without a
                    // TurnComplete; emit TurnEnd { success: false } so
                    // subscribers see symmetry with successful turns.
                    // If the provider errored before producing
                    // `IterationStarted { iteration: 1 }` (auth fail,
                    // network glitch on first request), `current_turn_id`
                    // is None — synthesize a TurnStart first so
                    // subscribers see a complete `PromptSubmitted ->
                    // TurnStart -> TurnEnd` shape instead of a missing
                    // turn frame for those error modes.
                    let turn_id = match current_turn_id.take() {
                        Some(id) => id,
                        None => {
                            next_turn_id = next_turn_id.saturating_add(1);
                            let synthetic = next_turn_id;
                            if let Err(err) = crate::plugin::effects::dispatch_host_event(
                                app,
                                savvagent_plugin::HostEvent::TurnStart { turn_id: synthetic },
                                0,
                            )
                            .await
                            {
                                tracing::warn!(error = %err,
                                    "synthetic TurnStart dispatch failed");
                            }
                            synthetic
                        }
                    };
                    // Clear any stale per-turn tool-call state so the
                    // next turn starts clean.
                    last_tool_call_id = None;
                    if let Err(err) = crate::plugin::effects::dispatch_host_event(
                        app,
                        savvagent_plugin::HostEvent::TurnEnd {
                            turn_id,
                            success: false,
                        },
                        0,
                    )
                    .await
                    {
                        tracing::warn!(error = %err,
                            "TurnEnd(failure) dispatch failed");
                    }
                }
                WorkerMsg::BashDone => {
                    app.is_loading = false;
                    app.update_metrics();
                }
                WorkerMsg::DisconnectCompleted { provider, mode } => {
                    app.push_note(
                        rust_i18n::t!("notes.disconnect-completed", name = provider, mode = mode)
                            .to_string(),
                    );
                }
                WorkerMsg::DisconnectFailed { provider, err } => {
                    app.push_note(
                        rust_i18n::t!("notes.disconnect-worker-failed", name = provider, err = err)
                            .to_string(),
                    );
                }
            }
        }

        if app.should_quit {
            // If the user requested quit mid-turn, subscribers would
            // otherwise see a TurnStart with no matching TurnEnd. Emit
            // TurnEnd { success: false } so the turn frame closes
            // cleanly. (We dispatch before the host-slot drain so
            // subscribers still have App state to react to. We don't
            // bother resetting `last_tool_call_id` here — we're
            // returning from `run_app` and the variable goes out of
            // scope.)
            if let Some(turn_id) = current_turn_id.take() {
                if let Err(err) = crate::plugin::effects::dispatch_host_event(
                    app,
                    savvagent_plugin::HostEvent::TurnEnd {
                        turn_id,
                        success: false,
                    },
                    0,
                )
                .await
                {
                    tracing::warn!(error = %err, "TurnEnd(quit) dispatch failed");
                }
            }
            drain_pending_bash_net(app, &host_slot).await;
            return Ok(());
        }

        if app.show_splash && app.splash_shown_at.elapsed() >= splash::SPLASH_DURATION {
            app.show_splash = false;
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let evt = event::read()?;
        let Event::Key(key) = &evt else { continue };
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            // Ctrl-C mid-turn: emit TurnEnd { success: false } so
            // subscribers see a complete turn frame instead of a
            // dangling TurnStart. (No need to reset
            // `last_tool_call_id` — we're about to return from
            // `run_app`.)
            if let Some(turn_id) = current_turn_id.take() {
                if let Err(err) = crate::plugin::effects::dispatch_host_event(
                    app,
                    savvagent_plugin::HostEvent::TurnEnd {
                        turn_id,
                        success: false,
                    },
                    0,
                )
                .await
                {
                    tracing::warn!(error = %err, "TurnEnd(ctrl-c) dispatch failed");
                }
            }
            drain_pending_bash_net(app, &host_slot).await;
            return Ok(());
        }

        if app.show_splash {
            app.show_splash = false;
            continue;
        }

        // Screen-stack routing: if any screen is on top, route the key there.
        // Reserved shortcuts (Ctrl-C already caught above; Ctrl-D quits) are
        // handled before forwarding to the screen.
        //
        // PR 3 limitation: KeyScope::OnScreen keybinding contributions are not
        // dispatched here. When a screen is on top, key events route directly
        // to its on_key(). PR 6 or later may add a KeybindingRouter::route
        // pass with active_screen=Some(top.id()) before falling through to
        // on_key, once a built-in screen actually needs OnScreen bindings.
        if !app.screen_stack.is_empty() {
            let portable = crate::plugin::convert::key_event_to_portable(*key);
            if portable.modifiers.ctrl
                && matches!(portable.code, savvagent_plugin::KeyCodePortable::Char('d'))
            {
                // Ctrl-D on a screen-stacked view is a quit: same
                // symmetric-TurnEnd treatment as the top-level Ctrl-C
                // path above. (No need to reset `last_tool_call_id`
                // — we're about to return from `run_app`.)
                if let Some(turn_id) = current_turn_id.take() {
                    if let Err(err) = crate::plugin::effects::dispatch_host_event(
                        app,
                        savvagent_plugin::HostEvent::TurnEnd {
                            turn_id,
                            success: false,
                        },
                        0,
                    )
                    .await
                    {
                        tracing::warn!(error = %err, "TurnEnd(ctrl-d) dispatch failed");
                    }
                }
                drain_pending_bash_net(app, &host_slot).await;
                return Ok(());
            }
            // view-file / edit-file are marker screens — most keys are
            // routed straight to the ratatui-code-editor instance in
            // `App::editor`. Only Esc (close), `q` (view-file only),
            // and Ctrl-S (edit-file only) go through `Screen::on_key`
            // to produce CloseScreen / SaveActiveFile effects.
            let top_id = app
                .screen_stack
                .top()
                .map(|(s, _)| s.id())
                .unwrap_or_default();
            if top_id == "view-file" || top_id == "edit-file" {
                let is_close = matches!(key.code, KeyCode::Esc)
                    || (top_id == "view-file" && key.code == KeyCode::Char('q'));
                let is_save_in_edit = top_id == "edit-file"
                    && matches!(key.code, KeyCode::Char('s'))
                    && key.modifiers.contains(KeyModifiers::CONTROL);
                if !is_close && !is_save_in_edit {
                    if let Some(editor) = app.editor.as_mut() {
                        let term_area = terminal.size()?;
                        let popup = ui::centered_rect(80, 80, term_area.into());
                        let inner = popup.inner(ratatui::layout::Margin {
                            horizontal: 1,
                            vertical: 1,
                        });
                        let _ = editor.input(*key, &inner);
                    }
                    continue;
                }
            }
            let effs = {
                let (top_screen, _layout) =
                    app.screen_stack.top_mut().expect("just checked non-empty");
                match top_screen.on_key(portable).await {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(error = %e, "screen on_key error");
                        continue;
                    }
                }
            };
            if let Err(e) = crate::plugin::effects::apply_effects(app, effs).await {
                tracing::warn!(error = %e, "apply_effects from screen failed");
            }
            apply_pending_model_change(app, &host_slot, &project_root, &tool_bins).await;
            continue;
        }

        match app.input_mode {
            InputMode::Editing => {
                if app.is_file_picker_active {
                    match key.code {
                        KeyCode::Enter => {
                            let file = app.file_explorer.current();
                            if file.is_dir {
                                app.file_explorer.handle(&evt)?;
                            } else {
                                app.file_picker_select();
                            }
                        }
                        KeyCode::Esc => app.close_file_picker(),
                        _ => {
                            app.file_explorer.handle(&evt)?;
                        }
                    }
                } else {
                    match key.code {
                        // Shift+Enter inserts a newline in the prompt. Only
                        // terminals that speak the Kitty keyboard protocol
                        // (pushed in `tui::init`) report the SHIFT modifier
                        // on Enter — on others, Shift+Enter is
                        // indistinguishable from Enter and the arm below
                        // submits. Documented as a tradeoff in `tui.rs`.
                        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                            app.input_textarea.insert_newline();
                        }
                        // Ctrl+Z → undo, Ctrl+Y → redo. Mirrors the
                        // Windows/Linux desktop convention. tui-textarea
                        // ships undo on Ctrl+U / redo on Ctrl+R (which
                        // still work via fallthrough); Ctrl+Y otherwise
                        // pastes in tui-textarea, but desktop muscle
                        // memory of Ctrl+Y=redo wins here.
                        KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.input_textarea.undo();
                        }
                        KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.input_textarea.redo();
                        }
                        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                            let value = app.input_textarea.lines().join("\n");
                            if value.is_empty() || app.is_loading {
                                continue;
                            }
                            if value.starts_with('/') {
                                app.input_textarea = make_input_textarea(Vec::<String>::new());
                                dispatch_slash_command(
                                    app,
                                    &value,
                                    &host_slot,
                                    &project_root,
                                    &tool_bins,
                                    &worker_tx,
                                )
                                .await;
                                continue;
                            }
                            let Some(host) = current_host(&host_slot).await else {
                                app.push_note(
                                    rust_i18n::t!("notes.not-connected-connect-first").to_string(),
                                );
                                app.input_textarea = make_input_textarea(Vec::<String>::new());
                                continue;
                            };
                            app.push_user(value.clone());
                            app.input_textarea = make_input_textarea(Vec::<String>::new());
                            app.is_loading = true;
                            // Fire HostEvent::PromptSubmitted so hook
                            // subscribers (transcript loggers, telemetry,
                            // future custom prompt-rewriters) see the
                            // submission before the host begins streaming
                            // turn events. Errors are warn-only — a buggy
                            // subscriber must not block the turn.
                            if let Err(err) = crate::plugin::effects::dispatch_host_event(
                                app,
                                savvagent_plugin::HostEvent::PromptSubmitted {
                                    text: value.clone(),
                                },
                                0,
                            )
                            .await
                            {
                                tracing::warn!(error = %err,
                                    "PromptSubmitted dispatch failed");
                            }

                            let tx = worker_tx.clone();
                            tokio::spawn(async move {
                                let (ev_tx, mut ev_rx) = mpsc::channel(64);
                                let host_for_run = host.clone();
                                let prompt = value;
                                let runner = tokio::spawn(async move {
                                    host_for_run.run_turn_streaming(prompt, ev_tx).await
                                });
                                while let Some(ev) = ev_rx.recv().await {
                                    if tx.send(WorkerMsg::Event(ev)).await.is_err() {
                                        break;
                                    }
                                }
                                match runner.await {
                                    Ok(Ok(_)) => {}
                                    Ok(Err(e)) => {
                                        let _ = tx.send(WorkerMsg::Error(e.to_string())).await;
                                    }
                                    Err(join_err) => {
                                        let _ = tx
                                            .send(WorkerMsg::Error(format!(
                                                "worker task failed: {join_err}"
                                            )))
                                            .await;
                                    }
                                }
                            });
                        }
                        KeyCode::Esc => {
                            app.input_textarea = make_input_textarea(Vec::<String>::new());
                        }
                        KeyCode::Char('@') => {
                            app.input_textarea.input(evt);
                            app.open_file_picker();
                        }
                        _ => {
                            // Try keybinding router (OnHome → Global) before
                            // falling through to the textarea. This is what
                            // fires `/` → OpenScreen("palette") when the
                            // prompt is empty.
                            let portable = crate::plugin::convert::key_event_to_portable(*key);
                            let mut handled = false;
                            if let (Some(_reg), Some(idx)) =
                                (&app.plugin_registry, &app.plugin_indexes)
                            {
                                let action = {
                                    let idx_guard = idx.read().await;
                                    let router = crate::plugin::keybindings::KeybindingRouter::new(
                                        &idx_guard,
                                    );
                                    router.route(&portable, None)
                                };
                                if let Some(action) = action {
                                    dispatch_bound_action(app, action).await;
                                    apply_pending_model_change(
                                        app,
                                        &host_slot,
                                        &project_root,
                                        &tool_bins,
                                    )
                                    .await;
                                    handled = true;
                                }
                            }
                            if !handled {
                                app.input_textarea.input(evt);
                            }
                        }
                    }
                }
            }
            InputMode::SelectingProvider => match key.code {
                KeyCode::Esc => app.input_mode = InputMode::Editing,
                KeyCode::Up if app.provider_index > 0 => app.provider_index -= 1,
                KeyCode::Down if app.provider_index + 1 < PROVIDERS.len() => {
                    app.provider_index += 1
                }
                KeyCode::Enter => {
                    let idx = app.provider_index;
                    if let Some(spec) = PROVIDERS.get(idx) {
                        if !spec.api_key_required {
                            // Keyless provider — connect immediately without a
                            // stored or prompted API key.
                            app.input_mode = InputMode::Editing;
                            perform_connect(
                                spec,
                                String::new(),
                                &host_slot,
                                &project_root,
                                &tool_bins,
                                app,
                            )
                            .await;
                        } else {
                            match creds::load(spec.id) {
                                Ok(Some(key)) => {
                                    app.input_mode = InputMode::Editing;
                                    app.push_note(
                                        rust_i18n::t!(
                                            "notes.using-stored-key",
                                            name = spec.display_name
                                        )
                                        .to_string(),
                                    );
                                    perform_connect(
                                        spec,
                                        key,
                                        &host_slot,
                                        &project_root,
                                        &tool_bins,
                                        app,
                                    )
                                    .await;
                                }
                                Ok(None) => app.enter_api_key_for(idx),
                                Err(e) => {
                                    app.push_note(
                                        rust_i18n::t!(
                                            "notes.keyring-error",
                                            err = format!("{e:#}")
                                        )
                                        .to_string(),
                                    );
                                    app.enter_api_key_for(idx);
                                }
                            }
                        }
                    } else {
                        app.input_mode = InputMode::Editing;
                    }
                }
                _ => {}
            },
            InputMode::EnteringApiKey => match key.code {
                KeyCode::Esc => app.cancel_connect(),
                KeyCode::Enter => match app.take_pending_api_key() {
                    Some((spec, Some(key))) => {
                        app.input_mode = InputMode::Editing;
                        perform_connect(spec, key, &host_slot, &project_root, &tool_bins, app)
                            .await;
                    }
                    Some((spec, None)) => {
                        // Empty submit — fall back to the stored
                        // credential if one exists. This is the
                        // one-keystroke "use stored key" path that the
                        // placeholder advertises.
                        match creds::load(spec.id) {
                            Ok(Some(stored)) => {
                                app.cancel_connect();
                                perform_connect(
                                    spec,
                                    stored,
                                    &host_slot,
                                    &project_root,
                                    &tool_bins,
                                    app,
                                )
                                .await;
                            }
                            _ => {
                                // No stored key — stay in the modal so
                                // the user can keep typing.
                                app.push_note(rust_i18n::t!("notes.api-key-empty").to_string());
                            }
                        }
                    }
                    None => {}
                },
                _ => {
                    app.api_key_textarea.input(evt);
                }
            },
            InputMode::ViewingFile => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.input_mode = InputMode::Editing;
                    app.active_file_path = None;
                    app.editor = None;
                }
                _ => {
                    if let Some(editor) = &mut app.editor {
                        let area = terminal.size()?;
                        let popup = ui::centered_rect(80, 80, area.into());
                        let inner = popup.inner(ratatui::layout::Margin {
                            horizontal: 1,
                            vertical: 1,
                        });
                        editor.input(*key, &inner)?;
                    }
                }
            },
            InputMode::EditingFile => match key.code {
                KeyCode::Esc => {
                    app.save_file();
                    app.input_mode = InputMode::Editing;
                    app.active_file_path = None;
                    app.editor = None;
                }
                _ => {
                    if let Some(editor) = &mut app.editor {
                        let area = terminal.size()?;
                        let popup = ui::centered_rect(80, 80, area.into());
                        let inner = popup.inner(ratatui::layout::Margin {
                            horizontal: 1,
                            vertical: 1,
                        });
                        editor.input(*key, &inner)?;
                    }
                }
            },
            InputMode::PermissionPrompt => {
                let action = match key.code {
                    KeyCode::Char('y') => Some((PermissionDecision::Allow, false)),
                    KeyCode::Char('n') | KeyCode::Esc => Some((PermissionDecision::Deny, false)),
                    KeyCode::Char('a') => Some((PermissionDecision::Allow, true)),
                    KeyCode::Char('N') => Some((PermissionDecision::Deny, true)),
                    _ => None,
                };
                if let Some((decision, persist)) = action {
                    resolve_pending_permission(app, &host_slot, decision, persist).await;
                }
            }
            InputMode::BashNetworkPrompt { id, .. } => {
                let choice = match key.code {
                    KeyCode::Char('o') | KeyCode::Char('O') => Some(BashNetworkChoice::Once),
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        Some(BashNetworkChoice::AlwaysThisSession)
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') => Some(BashNetworkChoice::DenyOnce),
                    KeyCode::Char('f')
                    | KeyCode::Char('F')
                    | KeyCode::Char('n')
                    | KeyCode::Char('N') => Some(BashNetworkChoice::DenyAlways),
                    // Esc → Cancelled (policy-equivalent to DenyOnce, but
                    // labelled distinctly so the user sees that backing
                    // out implied a deny rather than reading their Esc
                    // as an active "deny" decision.
                    KeyCode::Esc => Some(BashNetworkChoice::Cancelled),
                    _ => None,
                };
                if let Some(choice) = choice {
                    if let Some(host) = current_host(&host_slot).await {
                        let host = host.clone();
                        tokio::spawn(async move {
                            host.resolve_bash_network_decision(id, choice).await;
                        });
                    }
                    app.input_mode = InputMode::Editing;
                    let label = match choice {
                        BashNetworkChoice::Once => {
                            rust_i18n::t!("bash.net-allowed-once").to_string()
                        }
                        BashNetworkChoice::AlwaysThisSession => {
                            rust_i18n::t!("bash.net-always-allowed").to_string()
                        }
                        BashNetworkChoice::DenyOnce => {
                            rust_i18n::t!("bash.net-denied-once").to_string()
                        }
                        BashNetworkChoice::DenyAlways => {
                            rust_i18n::t!("bash.net-never").to_string()
                        }
                        BashNetworkChoice::Cancelled => {
                            rust_i18n::t!("bash.net-cancelled").to_string()
                        }
                    };
                    app.push_note(label);
                }
            }
            InputMode::SelectingTranscript => match key.code {
                KeyCode::Esc => app.close_transcript_picker(),
                KeyCode::Up if app.transcript_index > 0 => {
                    app.transcript_index -= 1;
                }
                KeyCode::Down => {
                    let count = app.transcript_entries.len();
                    if app.transcript_index + 1 < count {
                        app.transcript_index += 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some(path) = app.selected_transcript_path().map(|p| p.to_path_buf()) {
                        app.close_transcript_picker();
                        if app.is_loading {
                            app.push_note(
                                rust_i18n::t!("notes.cannot-resume-during-turn").to_string(),
                            );
                        } else {
                            do_resume_from_path(app, &host_slot, &path).await;
                        }
                    }
                }
                _ => {}
            },
        }
    }
}

/// Dispatch a [`BoundAction`][savvagent_plugin::BoundAction] produced by the
/// keybinding router. Logs and surfaces errors to the user via
/// `push_styled_note` so a malformed binding or runtime error doesn't
/// silently no-op a keystroke.
async fn dispatch_bound_action(app: &mut App, action: savvagent_plugin::BoundAction) {
    match action {
        savvagent_plugin::BoundAction::EmitEffect(effect) => {
            if let Err(e) = crate::plugin::effects::apply_effects(app, vec![effect]).await {
                tracing::warn!(error = %e, "apply_effects from keybinding failed");
                app.push_styled_note(savvagent_plugin::StyledLine::plain(format!(
                    "Action failed: {e}"
                )));
            }
        }
        savvagent_plugin::BoundAction::RunSlash { name, args } => {
            let (reg, idx) = match (&app.plugin_registry, &app.plugin_indexes) {
                (Some(r), Some(i)) => (r.clone(), i.clone()),
                _ => {
                    tracing::warn!("dispatch_bound_action: plugin runtime not installed");
                    return;
                }
            };
            let effs_result = {
                let reg_guard = reg.read().await;
                let idx_guard = idx.read().await;
                let router = crate::plugin::slash::SlashRouter::new(&idx_guard, &reg_guard);
                router.dispatch(&name, args).await
            };
            match effs_result {
                Ok(effs) => {
                    if let Err(e) = crate::plugin::effects::apply_effects(app, effs).await {
                        tracing::warn!(error = %e, command = %name, "apply_effects after slash dispatch failed");
                        app.push_styled_note(savvagent_plugin::StyledLine::plain(
                            rust_i18n::t!("notes.command-failed", err = format!("{e:#}"))
                                .to_string(),
                        ));
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, command = %name, "slash dispatch failed");
                    app.push_styled_note(savvagent_plugin::StyledLine::plain(
                        rust_i18n::t!("notes.command-failed", err = format!("{e:#}")).to_string(),
                    ));
                }
            }
        }
        _ => {}
    }
}

/// On graceful exit, if the user is mid-modal on a bash-network prompt,
/// resolve it as [`BashNetworkChoice::DenyOnce`] so any worker awaiting
/// the corresponding `oneshot` doesn't hang while the runtime tears
/// down. Without this, the worker would only unblock once the `Host`
/// (and the Sender inside `pending_bash_network`) is finally dropped —
/// which happens *after* `run_app` returns. That gap is small but real,
/// and on shutdown we want the worker to finish promptly and let
/// `Host::shutdown` drain cleanly.
async fn drain_pending_bash_net(app: &App, host_slot: &HostSlot) {
    if let InputMode::BashNetworkPrompt { id, .. } = app.input_mode {
        if let Some(host) = current_host(host_slot).await {
            host.resolve_bash_network_decision(id, BashNetworkChoice::DenyOnce)
                .await;
        }
    }
}

/// Pop the pending permission off the app and resolve it on the active
/// host. With `persist = true`, also records a session rule so future
/// requests with identical args short-circuit the modal.
async fn resolve_pending_permission(
    app: &mut App,
    host_slot: &HostSlot,
    decision: PermissionDecision,
    persist: bool,
) {
    let Some(req) = app.pending_permission.take() else {
        app.input_mode = InputMode::Editing;
        return;
    };
    let host = current_host(host_slot).await;
    if let Some(host) = &host {
        if persist {
            host.add_session_rule(&req.name, &req.args, decision).await;
        }
        host.resolve_permission(req.id, decision).await;
    } else {
        // Host swapped while modal was up — the old host's gate will return
        // Err on its dropped oneshot, which is the cleanup path. Nothing
        // for us to do here.
    }
    app.input_mode = InputMode::Editing;

    let label = match (decision, persist) {
        (PermissionDecision::Allow, false) => rust_i18n::t!("permission.allowed-once").to_string(),
        (PermissionDecision::Allow, true) => rust_i18n::t!("permission.always-allowed").to_string(),
        (PermissionDecision::Deny, false) => rust_i18n::t!("permission.denied").to_string(),
        (PermissionDecision::Deny, true) => rust_i18n::t!("permission.always-denied").to_string(),
    };
    app.push_note(format!("{}: {label}", req.name));
}

#[cfg(test)]
mod model_validation_tests {
    use super::{ModelChangeOutcome, resolve_model_change, validate_model_id};
    use savvagent_host::{ListModelsResponse, ModelInfo};
    use savvagent_protocol::{ErrorKind, ProviderError};

    fn info(id: &str) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            display_name: None,
            context_window: None,
        }
    }

    fn resp(ids: &[&str]) -> ListModelsResponse {
        ListModelsResponse {
            models: ids.iter().map(|id| info(id)).collect(),
            default_model_id: None,
        }
    }

    fn err(kind: ErrorKind, msg: &str) -> ProviderError {
        ProviderError {
            kind,
            message: msg.to_string(),
            retry_after_ms: None,
            provider_code: None,
        }
    }

    #[test]
    fn validate_model_id_known() {
        let models = vec![info("a"), info("b")];
        assert!(validate_model_id("a", &models).is_ok());
    }

    #[test]
    fn validate_model_id_unknown_returns_known_set() {
        let models = vec![info("a"), info("b")];
        let err = validate_model_id("c", &models).unwrap_err();
        assert_eq!(err, vec!["a", "b"]);
    }

    #[test]
    fn validate_model_id_empty_list_always_rejects() {
        let models: Vec<ModelInfo> = vec![];
        let err = validate_model_id("anything", &models).unwrap_err();
        assert!(err.is_empty());
    }

    #[test]
    fn resolve_proceeds_silently_for_known_id() {
        let r = resp(&["a", "b"]);
        let outcome = resolve_model_change("a", Ok(&r));
        assert_eq!(outcome, ModelChangeOutcome::Proceed { warning: None });
    }

    #[test]
    fn resolve_rejects_unknown_id_with_known_set() {
        use crate::test_helpers::HOME_LOCK;
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let r = resp(&["a", "b"]);
        let outcome = resolve_model_change("c", Ok(&r));
        match outcome {
            ModelChangeOutcome::Reject { note } => {
                assert!(note.contains("Unknown model `c`"), "note: {note}");
                assert!(note.contains("a, b"), "note: {note}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn resolve_empty_list_proceeds_with_warning() {
        use crate::test_helpers::HOME_LOCK;
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let r = resp(&[]);
        let outcome = resolve_model_change("anything", Ok(&r));
        match outcome {
            ModelChangeOutcome::Proceed { warning: Some(w) } => {
                assert!(w.contains("Provider advertises no models"), "w: {w}");
                assert!(w.contains("`anything`"), "w: {w}");
            }
            other => panic!("expected Proceed with warning, got {other:?}"),
        }
    }

    #[test]
    fn resolve_not_implemented_proceeds_silently() {
        let e = err(ErrorKind::NotImplemented, "list_models not implemented");
        let outcome = resolve_model_change("anything", Err(&e));
        assert_eq!(outcome, ModelChangeOutcome::Proceed { warning: None });
    }

    #[test]
    fn resolve_network_error_proceeds_with_warning() {
        use crate::test_helpers::HOME_LOCK;
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let e = err(ErrorKind::Network, "HTTP 401: invalid_api_key");
        let outcome = resolve_model_change("gpt-x", Err(&e));
        match outcome {
            ModelChangeOutcome::Proceed { warning: Some(w) } => {
                assert!(w.contains("Could not verify"), "w: {w}");
                assert!(w.contains("`gpt-x`"), "w: {w}");
                assert!(w.contains("invalid_api_key"), "w: {w}");
            }
            other => panic!("expected Proceed with warning, got {other:?}"),
        }
    }
}
