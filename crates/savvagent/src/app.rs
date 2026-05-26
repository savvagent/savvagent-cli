//! TUI state. The app holds a shared [`Host`] and a render-friendly
//! conversation log built incrementally from streaming [`TurnEvent`]s.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use savvagent_plugin::{ContentBlockId, ContentRenderer, PixelFormat, PixelSize};

/// Lives inside [`App`]. Owns one renderer per live canvas block.
///
/// The image picker (`ratatui_image::Picker`) is constructed once at
/// startup via a stdio terminal query. Subsequent frames reuse the
/// picker to produce [`ratatui_image::protocol::StatefulProtocol`]
/// instances for each canvas, which the render path passes to
/// `ratatui_image::StatefulImage`.
pub(crate) struct CanvasRegistry {
    next_id: u32,
    renderers: HashMap<ContentBlockId, Box<dyn ContentRenderer>>,
    image_picker: Option<ratatui_image::picker::Picker>,
    image_states: HashMap<ContentBlockId, ratatui_image::protocol::StatefulProtocol>,
}

impl CanvasRegistry {
    pub fn new() -> Self {
        Self {
            next_id: 0,
            renderers: HashMap::new(),
            image_picker: ratatui_image::picker::Picker::from_query_stdio().ok(),
            image_states: HashMap::new(),
        }
    }

    /// Allocate a fresh [`ContentBlockId`] for a newly-arrived canvas.
    pub fn allocate_id(&mut self) -> ContentBlockId {
        let id = ContentBlockId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Insert a renderer instance for `id`.
    pub fn insert(&mut self, id: ContentBlockId, renderer: Box<dyn ContentRenderer>) {
        self.renderers.insert(id, renderer);
    }

    /// Drop every renderer + cached image protocol and reset the id
    /// counter to 0. Called at the top of [`App::replay_transcript`] so a
    /// `/resume` rebuilds the registry from scratch.
    ///
    /// Resetting `next_id` to 0 is load-bearing: replay re-allocates ids in
    /// stream order, and those ids must match the ordinals Task 26 saved
    /// each canvas's interactive-state blob under (the n-th top-level `Html`
    /// block was saved as ordinal n). The image picker is preserved — it's a
    /// terminal capability, not per-conversation state.
    pub fn clear(&mut self) {
        self.next_id = 0;
        self.renderers.clear();
        self.image_states.clear();
    }

    /// Look up the renderer for `id`.
    #[allow(dead_code)]
    pub fn get_mut(&mut self, id: ContentBlockId) -> Option<&mut Box<dyn ContentRenderer>> {
        self.renderers.get_mut(&id)
    }

    /// Iterate over `(id, renderer)` pairs for every live canvas. Used by
    /// the transcript-save bridge to collect `snapshot_state()` blobs
    /// keyed by each canvas's [`ContentBlockId`] (whose `.0` equals the
    /// canvas's stream ordinal among top-level `Html` blocks).
    pub fn iter_renderers(&self) -> impl Iterator<Item = (ContentBlockId, &dyn ContentRenderer)> {
        self.renderers.iter().map(|(id, r)| (*id, r.as_ref()))
    }

    /// Freeze the renderer for `id` (no-op if no such renderer). Used by
    /// the focus state machine when focus leaves a canvas.
    pub fn freeze(&mut self, id: ContentBlockId) {
        if let Some(r) = self.renderers.get_mut(&id) {
            r.freeze();
        }
    }

    /// Thaw the renderer for `id` (no-op if no such renderer). Used by the
    /// focus state machine when focus enters a canvas.
    pub fn thaw(&mut self, id: ContentBlockId) {
        if let Some(r) = self.renderers.get_mut(&id) {
            r.thaw();
        }
    }

    /// Expose the image picker for rendering (Task 16 uses this to produce
    /// `StatefulProtocol` instances from rendered `Frame`s).
    #[allow(dead_code)]
    pub fn image_picker_mut(&mut self) -> Option<&mut ratatui_image::picker::Picker> {
        self.image_picker.as_mut()
    }

    /// Expose the image states map for rendering (Task 16).
    #[allow(dead_code)]
    pub fn image_states_mut(
        &mut self,
    ) -> &mut HashMap<ContentBlockId, ratatui_image::protocol::StatefulProtocol> {
        &mut self.image_states
    }

    /// `true` iff this terminal supports an image protocol.
    pub fn image_protocol_available(&self) -> bool {
        self.image_picker.is_some()
    }

    /// Terminal cell dimensions in pixels — `(width, height)` — as
    /// reported by the picker. Returns `None` when there's no image
    /// protocol. Used by the renderer to size the requested `Frame` so
    /// the image scales sensibly into the available cell rect.
    pub fn image_cell_size(&self) -> Option<(u16, u16)> {
        let picker = self.image_picker.as_ref()?;
        let fs = picker.font_size();
        Some((fs.width, fs.height))
    }

    /// Look up — and lazily build — the `StatefulProtocol` for canvas `id`.
    ///
    /// The first call drives the renderer at `pixel_width`, converts the
    /// returned `Frame` into a `DynamicImage`, and asks the picker for a
    /// `StatefulProtocol`. Subsequent calls reuse the cached protocol; the
    /// stateful widget re-encodes internally when the render area changes.
    ///
    /// Returns `None` when:
    /// * the terminal has no image protocol (`image_picker` is `None`), or
    /// * no renderer is registered for `id`, or
    /// * the produced `Frame` is empty / mis-sized.
    pub fn image_protocol_mut(
        &mut self,
        id: ContentBlockId,
        pixel_width: u32,
    ) -> Option<&mut ratatui_image::protocol::StatefulProtocol> {
        let picker = self.image_picker.as_ref()?;
        if !self.image_states.contains_key(&id) {
            let renderer = self.renderers.get_mut(&id)?;
            let frame = renderer.render(PixelSize {
                width: pixel_width,
                height: 0,
            });
            let image = frame_to_dynamic_image(&frame)?;
            let protocol = picker.new_resize_protocol(image);
            self.image_states.insert(id, protocol);
        }
        self.image_states.get_mut(&id)
    }
}

/// Build an `image::DynamicImage` from a plugin-emitted [`savvagent_plugin::Frame`].
///
/// Frames are RGBA8 by contract (see `crates/savvagent-canvas/src/canvas.rs`),
/// but we accept BGRA8 by swapping byte channels rather than rejecting the
/// frame outright. Returns `None` for empty frames or when the byte buffer's
/// length doesn't match `width * height * 4`.
fn frame_to_dynamic_image(frame: &savvagent_plugin::Frame) -> Option<image::DynamicImage> {
    if frame.width == 0 || frame.height == 0 {
        return None;
    }
    let expected = (frame.width as usize)
        .checked_mul(frame.height as usize)?
        .checked_mul(4)?;
    if frame.bytes.len() != expected {
        return None;
    }
    let mut bytes = frame.bytes.clone();
    if matches!(frame.format, PixelFormat::Bgra8) {
        for px in bytes.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }
    let buf = image::RgbaImage::from_raw(frame.width, frame.height, bytes)?;
    Some(image::DynamicImage::ImageRgba8(buf))
}

impl std::fmt::Debug for CanvasRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasRegistry")
            .field("next_id", &self.next_id)
            .field("renderer_count", &self.renderers.len())
            .field("image_protocol", &self.image_protocol_available())
            .finish_non_exhaustive()
    }
}

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui_code_editor::editor::Editor;
use ratatui_explorer::{FileExplorer, FileExplorerBuilder, Theme};
use savvagent_host::{NetOverride, SandboxConfig, ToolCallStatus, TranscriptFile, TurnEvent};
use serde_json::Value;
use tui_textarea::{TextArea, WrapMode};

use crate::prompt_history::PromptHistory;
use crate::providers::{PROVIDERS, ProviderSpec};

/// Minimum height (rows, including borders) for the main prompt input.
/// 1 visible content row + 2 border rows.
pub const INPUT_MIN_ROWS: u16 = 3;
/// Maximum height (rows, including borders) for the main prompt input
/// before further content scrolls. ~8 visible content rows + 2 border rows.
pub const INPUT_MAX_ROWS: u16 = 10;
/// Undo/redo history depth for the main prompt. Default in
/// `tui-textarea` is 50; we raise it so users editing large multi-line
/// prompts can scrub back through more revisions.
pub const INPUT_MAX_HISTORIES: usize = 1000;

/// Build an owned ratatui-code-editor theme — `Vec<(token, hex)>` —
/// from the app's active TUI theme. Callers borrow into the
/// `Vec<(&str, &str)>` form via [`borrow_editor_theme`] at the
/// `Editor::new` call site so the upstream constructor sees a clean
/// slice of references without anything escaping.
///
/// The viewer/editor is short-lived, so we rebuild per-open rather
/// than caching on `App`: catches `/theme`-switches between opens
/// without a cache-invalidation step.
pub fn editor_theme_for_active(app: &App) -> Vec<(String, String)> {
    let palette = crate::palette::Palette::for_theme(app.active_theme);
    crate::plugin::builtin::themes::editor_theme::build_editor_theme(&palette)
}

/// Convert an owned editor theme into the borrowed shape
/// `Editor::new` accepts. The returned slice borrows from `owned`;
/// keep `owned` alive across the `Editor::new` call.
pub fn borrow_editor_theme(owned: &[(String, String)]) -> Vec<(&str, &str)> {
    owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

/// Map a file path's extension to the language id ratatui-code-editor
/// uses for syntax highlighting. Falls back to `"text"` for unrecognized
/// extensions so the editor still loads without highlighting.
pub fn language_for_path(path: &std::path::Path) -> &'static str {
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("txt");
    match extension {
        "rs" => "rust",
        "py" => "python",
        "js" => "javascript",
        "ts" => "typescript",
        "json" => "json",
        "toml" => "toml",
        "yml" | "yaml" => "yaml",
        "md" => "markdown",
        _ => "text",
    }
}

/// Build a fresh, properly-configured main-input [`TextArea`].
///
/// Wrap mode is `WordOrGlyph` (soft-wraps long lines at word boundaries,
/// falls back to graphemes for very long unbroken tokens), the row
/// range is `INPUT_MIN_ROWS..=INPUT_MAX_ROWS` so `TextArea::measure`
/// drives a dynamic input box that grows with multi-line / wrapped
/// content, and undo/redo depth is `INPUT_MAX_HISTORIES`. Used
/// everywhere we reset or rebuild the prompt textarea so the settings
/// can't drift across reset paths.
pub fn make_input_textarea<I, S>(lines: I) -> TextArea<'static>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let collected: Vec<String> = lines.into_iter().map(Into::into).collect();
    let mut ta = if collected.is_empty() {
        TextArea::default()
    } else {
        TextArea::from(collected)
    };
    ta.set_wrap_mode(WrapMode::WordOrGlyph);
    ta.set_min_rows(INPUT_MIN_ROWS);
    ta.set_max_rows(INPUT_MAX_ROWS);
    ta.set_max_histories(INPUT_MAX_HISTORIES);
    ta
}

/// Input mode — which sub-widget consumes the next key.
pub enum InputMode {
    /// Editing the prompt textarea.
    Editing,
    /// Browsing a read-only file in the legacy popup editor. Replaced by
    /// the `internal:view-file` Screen plugin; retained until a follow-up
    /// PR rips out the legacy file-popup mechanism.
    #[allow(dead_code)]
    ViewingFile,
    /// Editing a file in the legacy popup editor. Replaced by the
    /// `internal:edit-file` Screen plugin; retained until a follow-up
    /// PR rips out the legacy file-popup mechanism.
    #[allow(dead_code)]
    EditingFile,
    /// Provider selection list — first step of `/connect`.
    SelectingProvider,
    /// API-key input — second step of `/connect`. Masked.
    EnteringApiKey,
    /// Tool-permission modal up; the turn loop is paused on a `oneshot`.
    PermissionPrompt,
    /// Bash-network prompt modal up; the lazy bash spawn is paused on
    /// a `oneshot` keyed by `id`. The user picks Once /
    /// AlwaysThisSession / DenyOnce / DenyAlways via a single-key
    /// hotkey; the choice is forwarded to
    /// [`savvagent_host::Host::resolve_bash_network_decision`].
    BashNetworkPrompt {
        /// Opaque host-side request id; pass back when resolving.
        id: u64,
        /// Human-readable summary from the policy.
        summary: String,
    },
    /// Transcript picker open — selecting a file for `/resume`.
    SelectingTranscript,
    /// Focus is inside an inline HTML canvas. Key/mouse events route to
    /// the canvas's renderer (Tasks 21-25). `element_idx` is the index
    /// into the canvas's `focusable_elements()` list (None = focused the
    /// canvas but no specific element yet).
    Canvas {
        /// Which canvas holds focus (key into `App::canvas_registry`).
        id: savvagent_plugin::ContentBlockId,
        /// Currently focused element index within the canvas, if any.
        ///
        /// `#[allow(dead_code)]`: stored now, read by the key/mouse
        /// routing in Tasks 22-25; `-D warnings` flags the unused field
        /// until then.
        #[allow(dead_code)]
        element_idx: Option<u32>,
    },
}

/// Queued model-change request emitted by the model picker. The `run_app`
/// loop drains this field after each `apply_effects` call because
/// `apply_effects` doesn't have the `host_slot` / `project_root` /
/// `tool_bins` arguments [`crate::perform_model_change`] needs.
#[derive(Debug, Clone)]
pub struct PendingModelChange {
    /// Bare model id requested by the picker (no `models/` prefix).
    pub id: String,
    /// Whether the change should be persisted to `~/.savvagent/models.toml`.
    pub persist: bool,
}

/// Queued pool-add request emitted by `Effect::RegisterProvider` when a
/// provider plugin's silent-connect path (stored keyring credential or
/// keyless local provider) fires. `apply_effects` only has access to
/// `App`, so it stuffs the constructed client into `App::registered_providers`
/// for the TUI's per-plugin view but can't reach `host_slot` to add the
/// provider to the host's pool. The drainer in
/// `main.rs::apply_pending_pool_add` does the host-side work: dispatches
/// on the provider id to rebuild a fresh `ProviderRegistration` via the
/// matching plugin's `try_build_registration`, calls `host.add_provider`,
/// and refreshes the `/model` picker.
///
/// Without this, `/connect <provider>` via the silent path (key already
/// stored) was a silent failure: the client landed in
/// `App::registered_providers` but never in `Host::pool`, so `/model`
/// didn't see the provider and turns couldn't route to it.
///
/// Carrying only `id` (no client) is intentional. The drainer rebuilds a
/// fresh client via `try_build_registration` — cheap, and it sidesteps
/// the `Box<dyn ProviderClient>` → `Arc<dyn ProviderClient + Send + Sync>`
/// conversion that the host pool requires. The take-client side-effect
/// in `Effect::RegisterProvider` is preserved so `App::registered_providers`
/// stays populated for sites that read it (currently `render_routing_show`).
pub struct PendingPoolAdd {
    /// Provider id (`"anthropic"`, `"gemini"`, etc.) — drains into the
    /// per-id dispatch in `apply_pending_pool_add`.
    pub id: savvagent_plugin::ProviderId,
    /// Human-readable display name carried in the effect payload; used
    /// for the "Connected to …" note when the add succeeds.
    pub display_name: String,
}

/// Queued routing-rules action emitted by `Effect::ReloadRoutingRules`
/// or `Effect::ShowRoutingRules`. The `run_app` loop drains these
/// flags after each `apply_effects` call because `apply_effects`
/// doesn't have host access.
#[derive(Debug, Clone, Copy, Default)]
pub struct PendingRoutingAction;

/// Snapshot of a pending [`TurnEvent::PermissionRequested`] used to render
/// the modal and resolve the host's outstanding `oneshot`.
#[derive(Debug, Clone)]
pub struct PendingPermission {
    /// Opaque host-side request id; pass back to `Host::resolve_permission`.
    pub id: u64,
    /// Tool the model wants to invoke.
    pub name: String,
    /// Short human-readable summary from the policy.
    pub summary: String,
    /// Full argument JSON, rendered (truncated) below the summary.
    pub args: Value,
}

/// One row in the transcript picker list.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    /// Full path to the `.json` file.
    pub path: PathBuf,
    /// Human-readable timestamp label (e.g. `2026-05-10 14:32:01`).
    pub timestamp: String,
    /// First user message text, truncated for preview.
    pub preview: String,
    /// Total number of messages in the transcript.
    pub message_count: usize,
}

/// One row in the conversation log.
#[derive(Debug, Clone)]
pub enum Entry {
    /// Submitted user prompt.
    User(String),
    /// Finalized assistant text.
    Assistant(String),
    /// Tool the model is calling (or just called). `status = None` means in-flight.
    Tool {
        /// Tool name.
        name: String,
        /// Raw JSON arguments the model passed to the tool. Passed to
        /// `ToolSummaryRouter::summarize_call` during the async pre-render
        /// step; falls back to `savvagent_plugin::styled::json_spans` when
        /// no plugin claims this tool name.
        args: serde_json::Value,
        /// Outcome (None while running).
        status: Option<ToolCallStatus>,
        /// Raw result payload from the tool (only set after completion).
        /// Passed to `ToolSummaryRouter::summarize_result` during pre-render.
        result_text: Option<String>,
    },
    /// Per-turn routing badge — rendered as a muted single line above
    /// the assistant entry that follows it. Source: `TurnEvent::RouteSelected`.
    /// Format: `"provider/model — Reason"` (e.g. `"anthropic/claude-opus-4-7 — Override"`).
    RouteBadge(String),
    /// Local notice — file ops, errors, transcript notifications.
    Note(String),
    /// A model-emitted HTML block to be rendered inline as a canvas.
    ///
    /// `source_preview` is `Some(...)` while the block is still
    /// streaming (each `HtmlSourceDelta` appends to it); on
    /// `ContentBlockStop` the preview is moved into `source` and
    /// `source_preview` is set back to `None`. The renderer instance
    /// lives in `App::canvas_registry`.
    Canvas {
        /// Host-assigned id, matching the renderer key in the
        /// canvas registry.
        id: savvagent_plugin::ContentBlockId,
        /// Final HTML source (after streaming completes).
        source: String,
        /// In-flight source buffer during streaming, swapped to
        /// `source` and reset to `None` on completion.
        source_preview: Option<String>,
    },
}

/// Slash command shown in the palette.
// Fields are used by tests and will be wired into the plugin-driven
// palette in PR 8. Suppress dead_code until then.
#[allow(dead_code)]
pub struct Command {
    /// Including the leading slash.
    pub name: String,
    /// One-liner shown in the palette.
    pub description: String,
    /// `true` for commands that take an argument (e.g. `/view <path>`). When
    /// the user picks one of these from the palette we prefill the prompt
    /// instead of executing it; commands without args run on Enter.
    pub needs_arg: bool,
}

/// Parsed `/bash` slash-command suffix. The TUI uses this to thread a
/// per-call network override down to
/// [`savvagent_host::Host::run_bash_command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashCommand {
    /// Per-call override of `tool-bash`'s network access. See
    /// [`NetOverride`] for the 3-state semantics — [`NetOverride::Inherit`]
    /// is the "no flag" case and defers to the resolved permission
    /// decision.
    pub net_override: NetOverride,
    /// The shell command itself, stripped of recognised flags.
    pub command: String,
}

/// Error returned by [`parse_bash_command`].
#[derive(Debug, PartialEq, Eq)]
pub enum BashCommandError {
    /// The user typed `/bash` (or `/bash --net`) with nothing after.
    EmptyCommand,
    /// The user typed a dashed token at the start of the command that
    /// wasn't `--net` or `--no-net`. We surface these as errors so a
    /// typo can't silently fall through to being treated as a literal
    /// shell command — important for a security-relevant opt-in flag.
    UnknownFlag {
        /// The exact token we couldn't recognise (e.g. `-net`, `--Net`,
        /// `--net=true`, `--quiet`).
        token: String,
    },
}

impl std::fmt::Display for BashCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BashCommandError::EmptyCommand => write!(f, "bash command is empty"),
            BashCommandError::UnknownFlag { token } => write!(
                f,
                "unknown bash flag `{token}` — only `--net` and `--no-net` are recognised"
            ),
        }
    }
}

impl std::error::Error for BashCommandError {}

/// Parse the suffix of a `/bash` slash command. Recognises a leading
/// `--net` / `--no-net` flag and returns the rest verbatim as `command`.
///
/// The flag must appear *first* — `echo --net hi` is a literal command,
/// not a flag-prefixed invocation. This keeps quoting simple: anything
/// after the (optional) leading flag is forwarded as-is to `bash -c`.
///
/// Strict-flag rule: when the input starts with `-`, the first
/// whitespace-separated token MUST be exactly `--net` or `--no-net`.
/// Anything else (`-net`, `--Net`, `--net=true`, `--quiet`, …) is
/// returned as [`BashCommandError::UnknownFlag`] so a typo on this
/// security-relevant opt-in flag can never silently degrade into "run
/// the typo as a literal command".
pub fn parse_bash_command(input: &str) -> Result<BashCommand, BashCommandError> {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return Err(BashCommandError::EmptyCommand);
    }

    // If the input starts with `-`, the first token must be exactly
    // `--net` or `--no-net`. Any other dashed token is a typo we want
    // to surface rather than silently treat as a shell command.
    if trimmed.starts_with('-') {
        let (token, rest) = match trimmed.split_once(char::is_whitespace) {
            Some((t, r)) => (t, r.trim_start()),
            None => (trimmed, ""),
        };
        let net_override = match token {
            "--net" => NetOverride::ForceAllow,
            "--no-net" => NetOverride::ForceDeny,
            other => {
                return Err(BashCommandError::UnknownFlag {
                    token: other.to_string(),
                });
            }
        };
        if rest.is_empty() {
            return Err(BashCommandError::EmptyCommand);
        }
        return Ok(BashCommand {
            net_override,
            command: rest.to_string(),
        });
    }

    Ok(BashCommand {
        net_override: NetOverride::Inherit,
        command: trimmed.to_string(),
    })
}

/// Outcome of [`App::select_command`].
// Used by tests and the legacy command-palette integration tests in main.rs.
// Will be wired into the plugin-driven palette in PR 8.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSelection {
    /// The command takes no argument — caller should run it now.
    Execute(String),
    /// The command takes an argument — the prompt has been prefilled with
    /// `"<name> "` and we're back in editing mode for the user to type it.
    Prefill(String),
}

/// TUI app state.
pub struct App {
    pub input_textarea: TextArea<'static>,
    pub input_mode: InputMode,
    pub model: String,
    pub transcript_dir: PathBuf,

    /// Finalized + in-progress conversation entries.
    pub entries: Vec<Entry>,
    /// Live token buffer for the assistant turn currently streaming.
    pub live_text: String,
    /// True while a turn is in flight.
    pub is_loading: bool,
    /// Set by `/quit` or Ctrl-C to break the event loop.
    pub should_quit: bool,
    /// Approximate context size (chars / 4) — naive token estimate.
    pub context_size: usize,
    /// Most recent transcript path written this session.
    pub last_transcript: Option<PathBuf>,

    pub is_file_picker_active: bool,
    pub file_explorer: FileExplorer,
    pub editor: Option<Editor>,
    pub active_file_path: Option<PathBuf>,

    pub commands: Vec<Command>,
    pub command_index: usize,
    /// Live filter typed after `/` while the command palette is open,
    /// without the leading slash.
    pub palette_filter: String,

    /// True once `/connect` has linked the TUI to a running provider.
    pub connected: bool,
    /// Provider id currently in use (`anthropic`, `gemini`, …).
    pub active_provider_id: Option<&'static str>,
    /// Cursor in the provider selector.
    pub provider_index: usize,
    /// Masked input for the API key (only populated during `EnteringApiKey`).
    pub api_key_textarea: TextArea<'static>,
    /// Provider chosen in the selector and being keyed in now.
    pub pending_provider: Option<&'static ProviderSpec>,

    /// Whether the startup splash banner is still being shown. Cleared on the
    /// first key press the main loop sees (any key, including modifiers), or
    /// after [`SPLASH_DURATION`] elapses since [`splash_shown_at`].
    pub show_splash: bool,
    /// When the splash was first painted. Used by the main loop to auto-dismiss
    /// after [`SPLASH_DURATION`] when the user doesn't press a key.
    pub splash_shown_at: Instant,

    /// Active permission request, if the host is paused on a `oneshot`. Set
    /// when `TurnEvent::PermissionRequested` arrives, cleared when the user
    /// answers the modal.
    pub pending_permission: Option<PendingPermission>,

    // --- /resume transcript picker ---
    /// Transcript files available for resumption, sorted newest-first.
    pub transcript_entries: Vec<TranscriptEntry>,
    /// Highlighted row in the transcript picker.
    pub transcript_index: usize,

    /// When the current session was resumed from a saved transcript, this
    /// holds a human-readable timestamp string shown in the header.
    pub resumed_at: Option<String>,

    /// Theme applied to the render path. Loaded from
    /// `~/.savvagent/theme.toml` at startup; mutated by the
    /// `internal:themes` plugin via `Effect::SetActiveTheme` and
    /// persisted (when `persist = true`) by `apply_effects`.
    pub active_theme: crate::plugin::builtin::themes::catalog::Theme,

    /// Currently-active locale code (e.g. `"en"`). Loaded at startup
    /// from `~/.savvagent/language.toml` (or env detection); mutated by
    /// `apply_effects` on `Effect::SetActiveLocale`.
    pub active_language: String,

    /// Cached classification of the sandbox state for the startup splash.
    /// Loaded once at `App::new` via `SandboxConfig::load_with_status` so
    /// the splash render path doesn't re-read disk on every frame; refreshed
    /// from `host.sandbox_config()` once a host materializes so the banner
    /// matches what the host will actually apply, not whatever was on disk
    /// at TUI launch time.
    pub splash_sandbox: crate::splash::SandboxSplashState,

    /// Plugin registry (populated at startup via `install_plugin_runtime`).
    pub plugin_registry:
        Option<std::sync::Arc<tokio::sync::RwLock<crate::plugin::registry::PluginRegistry>>>,
    /// Indexes built from each enabled plugin's manifest.
    pub plugin_indexes:
        Option<std::sync::Arc<tokio::sync::RwLock<crate::plugin::manifests::Indexes>>>,

    /// LIFO stack of active plugin-provided screens. Driven by
    /// `Effect::OpenScreen` / `Effect::CloseScreen` via `apply_effects`.
    pub screen_stack: crate::plugin::screen_stack::ScreenStack,

    /// Provider clients announced by provider plugins via
    /// [`savvagent_plugin::Effect::RegisterProvider`], keyed by stable
    /// provider id. PR 6 only stores the clients here; PR 7 wires them
    /// into [`savvagent_host::Host`] so the tool loop can talk through
    /// them. Boxed-trait-object so the same map can hold the
    /// per-provider client implementations side by side.
    pub registered_providers:
        std::collections::HashMap<String, Box<dyn savvagent_mcp::ProviderClient>>,

    /// Model catalog cache for the `/model` picker. Refreshed after
    /// each `/connect` and `/model <id>` by calling `host.list_models()`
    /// and translating its `models` field into `Vec<ModelEntry>`. Empty
    /// when no host is up or when the active provider's `list_models`
    /// failed; the picker handles both gracefully.
    pub cached_models: Vec<savvagent_plugin::ModelEntry>,

    /// Queued by `Effect::SetActiveModel` (emitted by the model picker).
    /// The `run_app` loop drains this after each `apply_effects` call
    /// and forwards the request to [`crate::perform_model_change`],
    /// which owns the `host_slot` / `project_root` / `tool_bins` the
    /// effect-application layer doesn't have access to.
    pub pending_model_change: Option<PendingModelChange>,

    /// Queued by `Effect::RegisterProvider`; drained by
    /// `main.rs::apply_pending_pool_add` which has host-slot access.
    /// Carries the constructed [`savvagent_mcp::ProviderClient`] over
    /// the no-host-slot boundary so the silent-connect path can add the
    /// provider to the host pool (not just the TUI's `registered_providers`
    /// view).
    pub pending_pool_add: Option<PendingPoolAdd>,

    /// Queued by `Effect::RegisterPreToolGate`; drained by
    /// `main.rs::apply_pending_gate` which has host-slot access.
    /// `None` when no gate is queued.
    pub pending_gate: Option<std::sync::Arc<dyn savvagent_host::PreToolUseGate>>,

    /// Queued by `Effect::RegisterInProcessTool`; drained by
    /// `main.rs::apply_pending_in_process_tool` which has host-slot
    /// access. Holds `(ToolDef, InProcessToolHandlerArc)` pairs so
    /// multiple plugins can register tools in the same startup pass
    /// (currently only `internal:user-agents` registers `task`, but the
    /// vector keeps the door open for future registrants without a
    /// follow-up refactor).
    pub pending_in_process_tools: Vec<(
        savvagent_protocol::ToolDef,
        savvagent_plugin::InProcessToolHandlerArc,
    )>,

    /// Queued by `Effect::ReloadRoutingRules`; drained by
    /// `main.rs::apply_pending_routing_reload`.
    pub pending_routing_reload: Option<PendingRoutingAction>,
    /// Queued by `Effect::ShowRoutingRules`; drained by
    /// `main.rs::apply_pending_routing_show`.
    pub pending_routing_show: Option<PendingRoutingAction>,

    /// Prompt text accumulated by `UserPromptSubmit` hooks before
    /// dispatch. Each `Effect::PrependToPendingPrompt` adds to the
    /// front; when the worker spawn fires, the full text becomes
    /// `accumulated\n\n<user typed prompt>`.
    pub pending_prompt_prefix: Option<String>,
    /// If `Some`, the next attempted turn dispatch aborts and `reason`
    /// is surfaced as a `[blocked]` PushNote. Set by
    /// `Effect::CancelPendingTurn`; cleared after the abort fires.
    pub pending_turn_cancellation: Option<String>,

    /// Per-project shell-style prompt history. Up at an empty input recalls
    /// the most recent entry; Up/Down then navigate while the recalled text
    /// is still the live textarea content (any edit cancels the browse).
    /// Loaded after `App::new` via [`App::load_prompt_history`] once
    /// `project_root` is known. Appended in the Enter-submit path.
    pub prompt_history: PromptHistory,

    /// Conversation-log scroll position, expressed as "rows hidden BELOW the
    /// viewport." `None` means auto-tail — newly arriving messages stay
    /// visible at the bottom. `Some(n)` means the user has scrolled back and
    /// wants to keep the same window of lines visible even as new content
    /// streams in. Reset to `None` by `End`/`Esc` and by submitting a new
    /// prompt. Driven by `PageUp`/`PageDown`/`Home`/`End` on the home screen.
    pub log_scroll_offset_from_bottom: Option<u16>,

    /// Live renderer instances keyed by [`ContentBlockId`], plus the
    /// terminal image protocol picker. Populated when an
    /// `Entry::Canvas` is created; Task 16 reads this during the render
    /// pass to produce ratatui-image frames.
    pub(crate) canvas_registry: CanvasRegistry,

    /// On-screen cell rects of canvases from the most recent render, for
    /// mouse hit-testing. Refreshed every frame by `ui::render`; keyed by
    /// canvas id. The mouse handler in `main.rs::run_app` runs outside the
    /// render pass, so it reads these persisted rects rather than the
    /// transient overlays produced during `render_log`.
    pub(crate) canvas_click_targets: Vec<(savvagent_plugin::ContentBlockId, ratatui::layout::Rect)>,

    /// Maps streaming block index → [`ContentBlockId`] for in-flight
    /// HTML blocks. Populated on `TurnEvent::HtmlBlockStart`, consumed
    /// on `TurnEvent::HtmlBlockStop`.
    pub(crate) html_block_index_to_id: HashMap<u32, savvagent_plugin::ContentBlockId>,

    /// One-turn model override populated by
    /// [`savvagent_plugin::Effect::SetNextTurnModelOverride`] and consumed
    /// by the worker spawn at the start of the next turn. `None`
    /// means "use the provider's currently-active model."
    pub next_turn_model_override: Option<String>,
    /// `(command_name, args)` that should re-dispatch after the trust
    /// modal resolves. Set by `internal:user-slash-commands` before
    /// emitting `Effect::OpenScreen("trust.modal")`; cleared by
    /// `apply_effects` after the re-dispatch (or on cancel).
    pub pending_slash_after_trust: Option<(String, Vec<String>)>,

    /// In-memory trust state for the session, shared with the
    /// `internal:user-slash-commands` plugin so `handle_slash` can
    /// read trust without going through `App`. Loaded from
    /// `~/.savvagent/trusted-projects.json` at startup; `Always`
    /// decisions persist back via `Effect::SetTrustLevel`.
    pub trust_levels: std::sync::Arc<
        tokio::sync::RwLock<
            std::collections::BTreeMap<
                std::path::PathBuf,
                crate::plugin::builtin::user_slash_commands::trust::TrustLevel,
            >,
        >,
    >,

    /// Shared user-hooks index. Initialized to an empty `HooksIndex` by
    /// `App::new`; populated by `main.rs` immediately after construction
    /// once `project_root` is known; mutated thereafter by
    /// `/reload-hooks`. Cloned into the `internal:user-hooks` plugin so
    /// both views (App-side and plugin-side) see the same data.
    pub user_hooks_index: std::sync::Arc<
        tokio::sync::RwLock<crate::plugin::builtin::user_hooks::discovery::HooksIndex>,
    >,
    /// Canonical per-session transcript path
    /// (`<transcript_dir>/<session_id>.json`). Initialized by `App::new`
    /// and read by:
    ///
    /// * `save_transcript_now` — the destination for auto-saves at
    ///   `TurnComplete` and manual `/save`. Multiple saves overwrite the
    ///   same file by design (one transcript per session).
    /// * the `internal:user-hooks` plugin — included as the
    ///   `transcript_path` field of every hook stdin payload so user
    ///   scripts can locate the transcript even before the first save.
    ///
    /// Wrapped in `Arc<RwLock>` so future features (e.g. mid-session
    /// relocation) can mutate it; today there is no writer.
    pub transcript_path: std::sync::Arc<tokio::sync::RwLock<std::path::PathBuf>>,
    /// Per-process session id, generated at startup. Used as the
    /// `session_id` field of every user-hook stdin payload.
    pub session_id: String,
}

/// Compute the `scroll_y` value (number of wrapped rows hidden ABOVE the
/// viewport) for the conversation log. `total` is the wrapped line count
/// from [`ratatui::widgets::Paragraph::line_count`]; `viewport` is the
/// inner area height in rows; `offset_from_bottom` mirrors
/// [`App::log_scroll_offset_from_bottom`].
///
/// Cascade:
/// 1. If `total <= viewport` everything fits — return `0` (top-anchored).
/// 2. `None` → `max_scroll` so the newest row lands on the bottom of the
///    viewport (auto-tail).
/// 3. `Some(n)` → `max_scroll - n`, clamped to `0` so the `u16::MAX` "scroll
///    to top" sentinel collapses correctly.
///
/// The return is `u16` because `Paragraph::scroll` takes `(u16, u16)`. A
/// wrapped-line count above `u16::MAX` clamps at the top of history — the
/// alternative is silently truncating the offset to a wrong row.
pub(crate) fn log_scroll_y(total: usize, viewport: usize, offset_from_bottom: Option<u16>) -> u16 {
    let max_scroll = total.saturating_sub(viewport);
    let scroll_y = match offset_from_bottom {
        None => max_scroll,
        Some(off) => max_scroll.saturating_sub(off as usize),
    };
    u16::try_from(scroll_y).unwrap_or(u16::MAX)
}

/// Apply one mouse-wheel tick to a [`App::log_scroll_offset_from_bottom`].
/// `step` is the number of wrapped rows to move per tick.
///
/// Up enters scrollback from `None` (auto-tail) by adding `step` rows; further
/// ups keep accumulating. Down decrements and snaps back to `None` (auto-tail)
/// when the offset would reach 0, which matches what `PageDown` does today and
/// keeps the "I'm reading live" state cleanly distinguishable from "I happen
/// to be parked at the bottom of scrollback."
pub(crate) fn log_scroll_offset_after_wheel(
    current: Option<u16>,
    direction: WheelDirection,
    step: u16,
) -> Option<u16> {
    match direction {
        WheelDirection::Up => Some(current.unwrap_or(0).saturating_add(step)),
        WheelDirection::Down => current.and_then(|n| n.checked_sub(step)).filter(|n| *n > 0),
    }
}

/// Hit-test a terminal-cell mouse coordinate against the canvases' on-screen
/// rects (from [`App::canvas_click_targets`]) and translate it to a
/// frame-relative pixel offset within the hit canvas.
///
/// Returns `(canvas id, x_pixel, y_pixel)` for the first matching rect, or
/// `None` if the cell is outside every canvas. Pure so the routing logic in
/// `main.rs::run_app` (which can't easily be unit-tested) is exercised here.
pub(crate) fn canvas_hit(
    targets: &[(savvagent_plugin::ContentBlockId, ratatui::layout::Rect)],
    col: u16,
    row: u16,
    cell: savvagent_canvas::CellPixelSize,
) -> Option<(savvagent_plugin::ContentBlockId, u32, u32)> {
    for (id, rect) in targets {
        let cell_rect = savvagent_canvas::CellRect {
            col: rect.x,
            row: rect.y,
            width: rect.width,
            height: rect.height,
        };
        if let Some((px, py)) = savvagent_canvas::cell_to_pixel(cell_rect, cell, col, row) {
            return Some((*id, px, py));
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WheelDirection {
    Up,
    Down,
}

impl App {
    /// True iff input focus is on the given canvas.
    ///
    /// `#[allow(dead_code)]`: only exercised by tests until the focus
    /// routing lands in Tasks 21-25, but `-D warnings` treats unused
    /// non-test items in this binary crate as errors.
    #[allow(dead_code)]
    pub(crate) fn is_canvas_focused(&self, id: savvagent_plugin::ContentBlockId) -> bool {
        matches!(self.input_mode, InputMode::Canvas { id: x, .. } if x == id)
    }

    /// Move focus into a canvas. Freezes any previously-focused (different)
    /// canvas and thaws the incoming one.
    #[allow(dead_code)]
    pub(crate) fn focus_canvas(
        &mut self,
        id: savvagent_plugin::ContentBlockId,
        element_idx: Option<u32>,
    ) {
        if let InputMode::Canvas { id: prev, .. } = self.input_mode {
            if prev != id {
                self.canvas_registry.freeze(prev);
            }
        }
        self.canvas_registry.thaw(id);
        self.input_mode = InputMode::Canvas { id, element_idx };
    }

    /// Leave canvas focus, returning to the prompt editor. Freezes the canvas.
    #[allow(dead_code)]
    pub(crate) fn unfocus_canvas(&mut self) {
        if let InputMode::Canvas { id, .. } = self.input_mode {
            self.canvas_registry.freeze(id);
        }
        self.input_mode = InputMode::Editing;
    }

    /// Update the focused element index without a freeze/thaw cycle. No-op
    /// unless a canvas currently holds focus. Used by Tab/Shift-Tab
    /// traversal, which moves within the already-focused canvas.
    #[allow(dead_code)]
    pub(crate) fn set_canvas_element(&mut self, idx: Option<u32>) {
        if let InputMode::Canvas { id, .. } = self.input_mode {
            self.input_mode = InputMode::Canvas {
                id,
                element_idx: idx,
            };
        }
    }

    /// Build TUI state. The host runs out-of-band; the app only carries the
    /// model name (for the header), the directory transcripts get written
    /// into, and the conversation log it builds from streaming events.
    pub fn new(model: String, transcript_dir: PathBuf, initial_language: String) -> Self {
        // Generate the per-process session id ONCE so it can seed both
        // `session_id` and the canonical transcript path. The path is
        // `<transcript_dir>/<session_id>.json` — deterministic, stable for
        // the session's lifetime, and shared with user-hooks payloads so
        // their `transcript_path` field points at a real file.
        let session_id = format!(
            "savvagent-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        let session_transcript_path = transcript_dir.join(format!("{session_id}.json"));

        let theme = Theme::default()
            .add_default_title()
            .with_block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded),
            )
            .with_highlight_item_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .with_highlight_dir_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .with_highlight_symbol("> ");

        let file_explorer = FileExplorerBuilder::build_with_theme(theme)
            .expect("failed to initialize file explorer");

        let mut app = Self {
            input_textarea: make_input_textarea(Vec::<String>::new()),
            input_mode: InputMode::Editing,
            model,
            transcript_dir,
            entries: Vec::new(),
            live_text: String::new(),
            is_loading: false,
            should_quit: false,
            context_size: 0,
            last_transcript: None,
            is_file_picker_active: false,
            file_explorer,
            editor: None,
            active_file_path: None,
            commands: Vec::new(),
            command_index: 0,
            palette_filter: String::new(),
            connected: false,
            active_provider_id: None,
            provider_index: 0,
            api_key_textarea: TextArea::default(),
            pending_provider: None,
            show_splash: true,
            splash_shown_at: Instant::now(),
            pending_permission: None,
            transcript_entries: Vec::new(),
            transcript_index: 0,
            resumed_at: None,
            active_theme: crate::plugin::builtin::themes::catalog::load(),
            active_language: initial_language,
            splash_sandbox: {
                let (cfg, status) = SandboxConfig::load_with_status();
                crate::splash::SandboxSplashState::from_load(&cfg, &status)
            },
            plugin_registry: None,
            plugin_indexes: None,
            screen_stack: crate::plugin::screen_stack::ScreenStack::new(),
            registered_providers: std::collections::HashMap::new(),
            cached_models: Vec::new(),
            pending_model_change: None,
            pending_pool_add: None,
            pending_gate: None,
            pending_in_process_tools: Vec::new(),
            pending_routing_reload: None,
            pending_routing_show: None,
            pending_prompt_prefix: None,
            pending_turn_cancellation: None,
            prompt_history: PromptHistory::default(),
            log_scroll_offset_from_bottom: None,
            canvas_registry: CanvasRegistry::new(),
            canvas_click_targets: Vec::new(),
            html_block_index_to_id: HashMap::new(),
            next_turn_model_override: None,
            pending_slash_after_trust: None,
            trust_levels: {
                // Load persisted trust decisions from disk. Missing file is OK —
                // `trust::load` returns an empty map in that case.
                let loaded = if let Some(home) = dirs::home_dir() {
                    let (map, warn) =
                        crate::plugin::builtin::user_slash_commands::trust::load(&home);
                    if let Some(w) = warn {
                        tracing::warn!("user-slash-commands: {w}");
                    }
                    map
                } else {
                    std::collections::BTreeMap::new()
                };
                std::sync::Arc::new(tokio::sync::RwLock::new(loaded))
            },
            user_hooks_index: std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::plugin::builtin::user_hooks::discovery::HooksIndex::default(),
            )),
            transcript_path: std::sync::Arc::new(tokio::sync::RwLock::new(session_transcript_path)),
            session_id,
        };
        app.refresh_commands();
        app
    }

    /// Load prompt history for `project_root`. Called from `main.rs` once
    /// the project root is known. Idempotent — calling it twice just
    /// reloads from disk.
    pub fn load_prompt_history(&mut self, project_root: &std::path::Path) {
        self.prompt_history = PromptHistory::load(project_root);
    }

    /// Replace the prompt textarea with `text` and park the cursor past the
    /// last character. Splits on `\n` so a multi-line recalled history
    /// entry restores as multiple textarea lines rather than one line with
    /// embedded newline glyphs. Used by the Up/Down history-recall path.
    pub fn set_input_text_for_history(&mut self, text: &str) {
        let lines: Vec<String> = if text.is_empty() {
            Vec::new()
        } else {
            text.split('\n').map(str::to_string).collect()
        };
        self.input_textarea = make_input_textarea(lines);
        let row = self.input_textarea.lines().len().saturating_sub(1) as u16;
        let col = self
            .input_textarea
            .lines()
            .last()
            .map(|l| l.len())
            .unwrap_or(0) as u16;
        self.input_textarea
            .move_cursor(tui_textarea::CursorMove::Jump(row, col));
    }

    /// Install the plugin runtime. Called once at startup from `main`.
    pub fn install_plugin_runtime(
        &mut self,
        registry: crate::plugin::registry::PluginRegistry,
        indexes: crate::plugin::manifests::Indexes,
    ) {
        use std::sync::Arc;
        use tokio::sync::RwLock;
        self.plugin_registry = Some(Arc::new(RwLock::new(registry)));
        self.plugin_indexes = Some(Arc::new(RwLock::new(indexes)));
    }

    /// Refresh the splash sandbox indicator from a connected host. Called
    /// at startup if the TUI launched with a saved-credentials host already
    /// running, and from the `/connect` success path. Once a host is up,
    /// its [`SandboxConfig`] is the source of truth for what will actually
    /// be applied to tool spawns — anything the on-disk file said after
    /// that point would be a lie.
    pub fn refresh_splash_sandbox_from_host(&mut self, host_cfg: &SandboxConfig) {
        self.splash_sandbox = crate::splash::SandboxSplashState::from_host_config(host_cfg);
    }

    /// Apply one streaming event from the host into the conversation log.
    pub fn apply_turn_event(&mut self, event: TurnEvent) {
        match event {
            TurnEvent::RouteSelected {
                provider_id,
                model_id,
                reason,
            } => {
                self.flush_live_text();
                self.entries.push(Entry::RouteBadge(format!(
                    "{}/{} — {}",
                    provider_id.as_str(),
                    model_id,
                    reason
                )));
            }
            TurnEvent::ModalityWarning { message } => {
                self.flush_live_text();
                self.entries.push(Entry::Note(message));
            }
            TurnEvent::IterationStarted { .. } => {}
            TurnEvent::TextDelta { text } => {
                self.live_text.push_str(&text);
            }
            TurnEvent::ToolCallStarted { name, arguments } => {
                // If we have buffered streaming text from this iteration,
                // commit it as a finalized assistant entry first.
                self.flush_live_text();
                self.entries.push(Entry::Tool {
                    name,
                    args: arguments,
                    status: None,
                    result_text: None,
                });
            }
            TurnEvent::ToolCallFinished {
                name: _,
                status,
                result,
            } => {
                if let Some(Entry::Tool {
                    status: s,
                    result_text: r,
                    ..
                }) = self
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, Entry::Tool { status: None, .. }))
                {
                    *s = Some(status);
                    *r = Some(result);
                }
            }
            TurnEvent::PermissionRequested {
                id,
                name,
                summary,
                args,
            } => {
                self.flush_live_text();
                self.pending_permission = Some(PendingPermission {
                    id,
                    name,
                    summary,
                    args,
                });
                self.input_mode = InputMode::PermissionPrompt;
            }
            TurnEvent::BashNetworkRequested { id, summary } => {
                self.flush_live_text();
                self.entries.push(Entry::Note(format!(
                    "bash network access requested — see modal ({summary})"
                )));
                self.input_mode = InputMode::BashNetworkPrompt { id, summary };
            }
            TurnEvent::ToolCallDenied { name, reason } => {
                self.flush_live_text();
                self.entries
                    .push(Entry::Note(format!("denied {name}: {reason}")));
            }
            TurnEvent::TurnComplete { outcome } => {
                // If streaming delivered text deltas, flush them. Otherwise
                // fall back to the authoritative final text on the outcome —
                // a non-streaming provider, or a dropped delta, would
                // otherwise leave the user with no visible reply.
                if !self.live_text.is_empty() {
                    self.flush_live_text();
                } else if !outcome.text.is_empty() {
                    self.entries.push(Entry::Assistant(outcome.text));
                } else {
                    self.entries.push(Entry::Note(format!(
                        "(turn ended with no text · iterations={} · tool_calls={})",
                        outcome.iterations,
                        outcome.tool_calls.len()
                    )));
                }
                self.is_loading = false;
            }
            TurnEvent::Cancelled { reason } => {
                self.flush_live_text();
                self.entries
                    .push(Entry::Note(format!("turn cancelled: {reason}")));
                self.is_loading = false;
            }
            TurnEvent::AbortedAfterGrace { reason } => {
                self.flush_live_text();
                self.entries.push(Entry::Note(format!(
                    "turn aborted (grace expired): {reason}"
                )));
                self.is_loading = false;
            }
            TurnEvent::ResourceUpdated { uri, summary, .. } => {
                self.flush_live_text();
                self.entries
                    .push(Entry::Note(format!("resource updated: {uri} — {summary}")));
            }
            TurnEvent::HtmlBlockStart { index } => {
                let id = self.handle_html_block_start();
                self.html_block_index_to_id.insert(index, id);
            }
            TurnEvent::HtmlBlockDelta { index, source } => {
                if let Some(&id) = self.html_block_index_to_id.get(&index) {
                    self.handle_html_block_delta(id, &source);
                }
            }
            TurnEvent::HtmlBlockStop { index } => {
                if let Some(&id) = self.html_block_index_to_id.get(&index) {
                    self.handle_html_block_stop(id);
                    self.html_block_index_to_id.remove(&index);
                }
            }
            TurnEvent::SubagentStop { .. } => {
                // Translated to HostEvent::SubagentStop by
                // translate_turn_event_to_host_event so the user_hooks
                // plugin sees it. No TUI-side rendering needed (the
                // collapsible block work lands in Task 22).
            }
        }
    }

    /// Move any buffered streaming text into a finalized assistant entry.
    fn flush_live_text(&mut self) {
        if self.live_text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.live_text);
        self.entries.push(Entry::Assistant(text));
    }

    // ── HTML canvas streaming handlers ───────────────────────────────────────

    /// Called when a `TurnEvent::HtmlBlockStart` arrives.
    ///
    /// Flushes any buffered live text, allocates a fresh
    /// [`savvagent_plugin::ContentBlockId`], and pushes an `Entry::Canvas`
    /// with an empty `source_preview` (the streaming accumulator). Returns
    /// the allocated id so the caller can record the `index → id` mapping.
    pub fn handle_html_block_start(&mut self) -> savvagent_plugin::ContentBlockId {
        self.flush_live_text();
        let id = self.canvas_registry.allocate_id();
        self.entries.push(Entry::Canvas {
            id,
            source: String::new(),
            source_preview: Some(String::new()),
        });
        id
    }

    /// Called when a `TurnEvent::HtmlBlockDelta` arrives.
    ///
    /// Appends `fragment` to the `source_preview` buffer of the
    /// `Entry::Canvas` that was created for `id`. No-op if the entry
    /// is not found or has already been finalized (`source_preview` is `None`).
    pub fn handle_html_block_delta(
        &mut self,
        id: savvagent_plugin::ContentBlockId,
        fragment: &str,
    ) {
        if let Some(Entry::Canvas {
            source_preview,
            id: entry_id,
            ..
        }) = self
            .entries
            .iter_mut()
            .rfind(|e| matches!(e, Entry::Canvas { id: eid, .. } if *eid == id))
        {
            if let Some(buf) = source_preview {
                buf.push_str(fragment);
            }
            let _ = entry_id;
        }
    }

    /// Called when a `TurnEvent::HtmlBlockStop` arrives (sync half).
    ///
    /// Moves `source_preview` into `source` and sets `source_preview` to
    /// `None`, marking the entry as fully received. Renderer creation is
    /// async and handled separately in `main.rs` via
    /// [`App::try_create_canvas_renderer`].
    pub fn handle_html_block_stop(&mut self, id: savvagent_plugin::ContentBlockId) {
        if let Some(Entry::Canvas {
            source,
            source_preview,
            ..
        }) = self
            .entries
            .iter_mut()
            .rfind(|e| matches!(e, Entry::Canvas { id: eid, .. } if *eid == id))
            && let Some(preview) = source_preview.take()
        {
            *source = preview;
        }
    }

    /// Return all finalized canvases (not in-flight previews) in
    /// transcript order. Used by /save-canvas.
    pub fn canvas_sources_in_order(&self) -> Vec<(savvagent_plugin::ContentBlockId, String)> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Canvas {
                    id,
                    source,
                    source_preview,
                    ..
                } if source_preview.is_none() => Some((*id, source.clone())),
                _ => None,
            })
            .collect()
    }

    /// Helper for tests — return the last `Entry` in the conversation log.
    #[cfg(test)]
    pub(crate) fn last_entry(&self) -> Option<&Entry> {
        self.entries.last()
    }

    /// Convenience: append a user-visible note (file ops, errors, system messages).
    pub fn push_note(&mut self, msg: impl Into<String>) {
        self.entries.push(Entry::Note(msg.into()));
        self.update_metrics();
    }

    /// Append a user prompt to the log (call before spawning the streaming task).
    pub fn push_user(&mut self, text: String) {
        self.entries.push(Entry::User(text));
        self.update_metrics();
    }

    /// Recompute the rough context-size estimate.
    pub fn update_metrics(&mut self) {
        let chars: usize = self
            .entries
            .iter()
            .map(|e| match e {
                Entry::User(t) | Entry::Assistant(t) | Entry::Note(t) | Entry::RouteBadge(t) => {
                    t.len()
                }
                Entry::Tool {
                    args, result_text, ..
                } => {
                    // Approximate the JSON args by their compact serialization.
                    let args_len = serde_json::to_string(args).map(|s| s.len()).unwrap_or(0);
                    args_len + result_text.as_deref().map(str::len).unwrap_or(0)
                }
                Entry::Canvas {
                    source,
                    source_preview,
                    ..
                } => source.len() + source_preview.as_deref().map(str::len).unwrap_or(0),
            })
            .sum::<usize>()
            + self.live_text.len();
        self.context_size = chars / 4;
    }

    /// Slash commands surfaced in the palette.
    pub fn refresh_commands(&mut self) {
        self.commands = vec![
            Command {
                name: "/connect".into(),
                description: "Switch provider (uses stored key, or prompts if missing)".into(),
                needs_arg: false,
            },
            Command {
                name: "/clear".into(),
                description: "Reset conversation history".into(),
                needs_arg: false,
            },
            Command {
                name: "/save".into(),
                description: "Save transcript now".into(),
                needs_arg: false,
            },
            Command {
                name: "/view".into(),
                description: "View a file".into(),
                needs_arg: true,
            },
            Command {
                name: "/edit".into(),
                description: "Edit a file".into(),
                needs_arg: true,
            },
            Command {
                name: "/tools".into(),
                description: "List registered tools and their default permission verdict".into(),
                needs_arg: false,
            },
            Command {
                name: "/model".into(),
                description: "Show the current model (or `/model <id>` to switch)".into(),
                needs_arg: false,
            },
            Command {
                name: "/resume".into(),
                description: "Resume a saved transcript (opens picker, or /resume <path>)".into(),
                needs_arg: false,
            },
            Command {
                name: "/sandbox".into(),
                description: "Show sandbox status (`/sandbox on` or `/sandbox off` to toggle)"
                    .into(),
                needs_arg: false,
            },
            Command {
                name: "/theme".into(),
                description: "Pick a theme (opens picker, or /theme <name>)".into(),
                needs_arg: false,
            },
            Command {
                name: "/bash".into(),
                description: "Run a shell command (use `--net` / `--no-net` to override network)"
                    .into(),
                needs_arg: true,
            },
            Command {
                name: "/quit".into(),
                description: "Quit".into(),
                needs_arg: false,
            },
        ];
    }

    /// Indices into `self.commands` that match the current filter. If the
    /// filter is empty, returns every index.
    #[allow(dead_code)]
    pub fn filtered_command_indices(&self) -> Vec<usize> {
        if self.palette_filter.is_empty() {
            return (0..self.commands.len()).collect();
        }
        let needle = self.palette_filter.to_lowercase();
        self.commands
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                let name = c.name.strip_prefix('/').unwrap_or(&c.name).to_lowercase();
                name.starts_with(&needle)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Append a char to the palette filter and reset the cursor.
    #[allow(dead_code)]
    pub fn palette_push_char(&mut self, c: char) {
        self.palette_filter.push(c);
        self.command_index = 0;
    }

    /// Pop one char from the palette filter. Returns `false` if it was already
    /// empty (the caller can use this to close the palette on Backspace past
    /// the leading `/`).
    #[allow(dead_code)]
    pub fn palette_pop_char(&mut self) -> bool {
        let popped = self.palette_filter.pop().is_some();
        self.command_index = 0;
        popped
    }

    /// Close the legacy command palette state (filter + cursor). With the
    /// screen-stack redesign, actual screen closure is via `Effect::CloseScreen`.
    #[allow(dead_code)]
    pub fn close_command_palette(&mut self) {
        self.palette_filter.clear();
        self.command_index = 0;
    }

    /// Resolve the highlighted palette item. Operates on the filtered view —
    /// `command_index` is the position within the visible list, not within
    /// `self.commands`. Closes the palette either way; returns whether the
    /// caller should execute the command now or just leave the prefilled
    /// prompt for the user to finish typing arguments.
    #[allow(dead_code)]
    pub fn select_command(&mut self) -> Option<CommandSelection> {
        let filtered = self.filtered_command_indices();
        let real_idx = match filtered.get(self.command_index).copied() {
            Some(i) => i,
            None => {
                self.close_command_palette();
                return None;
            }
        };
        let cmd = &self.commands[real_idx];
        let name = cmd.name.clone();
        let needs_arg = cmd.needs_arg;

        self.input_mode = InputMode::Editing;
        self.palette_filter.clear();
        self.command_index = 0;

        if needs_arg {
            self.input_textarea = make_input_textarea(vec![format!("{name} ")]);
            Some(CommandSelection::Prefill(name))
        } else {
            self.input_textarea = make_input_textarea(Vec::<String>::new());
            Some(CommandSelection::Execute(name))
        }
    }

    /// Insert the currently-highlighted file as `@path` in the textarea.
    pub fn file_picker_select(&mut self) {
        let file = self.file_explorer.current();
        if file.is_dir {
            return;
        }
        let path = file.path.clone();

        let mut current = self.input_textarea.lines().join("\n");
        if let Some(last_at) = current.rfind('@') {
            current.truncate(last_at + 1);
            current.push_str(&path.to_string_lossy());
        } else {
            if !current.is_empty() && !current.ends_with(' ') {
                current.push(' ');
            }
            current.push('@');
            current.push_str(&path.to_string_lossy());
        }
        self.input_textarea = make_input_textarea(current.lines().map(|s| s.to_string()));
        let row = self.input_textarea.lines().len().saturating_sub(1) as u16;
        let col = self
            .input_textarea
            .lines()
            .last()
            .map(|l| l.len())
            .unwrap_or(0) as u16;
        self.input_textarea
            .move_cursor(tui_textarea::CursorMove::Jump(row, col));
        self.close_file_picker();
    }

    /// Show the file-picker popup.
    pub fn open_file_picker(&mut self) {
        self.is_file_picker_active = true;
    }

    /// Hide the file-picker popup.
    pub fn close_file_picker(&mut self) {
        self.is_file_picker_active = false;
    }

    /// Build a syntax-highlighted [`Editor`] for `path` and install it as
    /// the active editor. Used by the plugin-driven view/edit flow:
    /// `apply_effects::open_screen` calls this when a `view-file` or
    /// `edit-file` screen is pushed so `ui.rs` can render the file via
    /// ratatui-code-editor. Does **not** mutate `input_mode` — the
    /// screen stack tracks visibility instead. Returns `true` on
    /// success; on failure (missing file, I/O error, editor-construct
    /// error) a styled note is pushed and `false` is returned so the
    /// caller can skip pushing the marker screen.
    pub fn load_file_into_editor(&mut self, path: PathBuf) -> bool {
        if !path.exists() {
            self.push_note(
                rust_i18n::t!("notes.file-not-found", path = path.display().to_string())
                    .to_string(),
            );
            return false;
        }
        let lang = language_for_path(&path);
        let owned_theme = editor_theme_for_active(self);
        match std::fs::read_to_string(&path) {
            Ok(content) => match Editor::new(lang, &content, borrow_editor_theme(&owned_theme)) {
                Ok(editor) => {
                    self.editor = Some(editor);
                    self.active_file_path = Some(path);
                    true
                }
                Err(e) => {
                    self.push_note(
                        rust_i18n::t!("notes.file-editor-error", err = format!("{e:#}"))
                            .to_string(),
                    );
                    false
                }
            },
            Err(e) => {
                self.push_note(
                    rust_i18n::t!("notes.file-read-error", err = format!("{e:#}")).to_string(),
                );
                false
            }
        }
    }

    /// Clear the active editor + file path. Called by `apply_effects` when a
    /// `view-file` or `edit-file` screen is popped from the stack.
    pub fn clear_active_editor(&mut self) {
        self.editor = None;
        self.active_file_path = None;
    }

    /// Open `path` in the legacy popup editor (read-only or read-write per
    /// `edit`). Retained for the legacy `InputMode::ViewingFile`/
    /// `EditingFile` path; new code goes through
    /// [`Self::load_file_into_editor`] + the screen-stack abstraction.
    #[allow(dead_code)]
    pub fn open_file(&mut self, path: PathBuf, edit: bool) {
        if !path.exists() {
            self.push_note(
                rust_i18n::t!("notes.file-not-found", path = path.display().to_string())
                    .to_string(),
            );
            return;
        }
        let lang = language_for_path(&path);
        let owned_theme = editor_theme_for_active(self);
        match std::fs::read_to_string(&path) {
            Ok(content) => match Editor::new(lang, &content, borrow_editor_theme(&owned_theme)) {
                Ok(editor) => {
                    self.editor = Some(editor);
                    self.active_file_path = Some(path);
                    self.input_mode = if edit {
                        InputMode::EditingFile
                    } else {
                        InputMode::ViewingFile
                    };
                }
                Err(e) => self.push_note(
                    rust_i18n::t!("notes.file-editor-error", err = format!("{e:#}")).to_string(),
                ),
            },
            Err(e) => self.push_note(
                rust_i18n::t!("notes.file-read-error", err = format!("{e:#}")).to_string(),
            ),
        }
    }

    /// Persist the open editor's buffer to disk.
    pub fn save_file(&mut self) {
        let Some(path) = self.active_file_path.clone() else {
            return;
        };
        let Some(editor) = &self.editor else { return };
        let content = editor.get_content();
        match std::fs::write(&path, content) {
            Ok(_) => self.push_note(
                rust_i18n::t!("notes.file-saved", path = path.display().to_string()).to_string(),
            ),
            Err(e) => self.push_note(
                rust_i18n::t!("notes.file-write-error", err = format!("{e:#}")).to_string(),
            ),
        }
    }

    /// Populate `transcript_entries` from `dir` and enter the picker mode.
    ///
    /// Entries are sorted newest-first by the Unix timestamp embedded in the
    /// filename (`<unix>.json`). Files that cannot be parsed as JSON are
    /// silently skipped so a single corrupt file doesn't break the whole
    /// picker.
    pub fn open_transcript_picker(&mut self, dir: &std::path::Path) {
        self.transcript_entries = collect_transcript_entries(dir);
        self.transcript_index = 0;
        self.input_mode = InputMode::SelectingTranscript;
    }

    /// Close the transcript picker without selecting anything.
    pub fn close_transcript_picker(&mut self) {
        self.transcript_entries.clear();
        self.transcript_index = 0;
        self.input_mode = InputMode::Editing;
    }

    /// Return the path of the currently-highlighted transcript entry, if any.
    pub fn selected_transcript_path(&self) -> Option<&std::path::Path> {
        self.transcript_entries
            .get(self.transcript_index)
            .map(|e| e.path.as_path())
    }

    /// Replay a loaded transcript into the visible conversation log as
    /// "history" entries. Tool-use blocks are rendered with `[history]` status
    /// so they look distinct from live calls. Called after `load_transcript`
    /// succeeds so the user can see prior context.
    pub fn replay_transcript(&mut self, record: &TranscriptFile) {
        use savvagent_protocol::{ContentBlock, Role};

        self.entries.clear();
        self.live_text.clear();
        // Reset the canvas registry so a re-resume doesn't leak old
        // renderers and so replayed ids start at 0 — matching the ordinals
        // Task 26 saved each canvas's `state` blob under.
        self.canvas_registry.clear();
        self.html_block_index_to_id.clear();

        for msg in &record.messages {
            match msg.role {
                Role::User => {
                    // Collect text blocks; skip tool_result blocks (they're
                    // the host's synthetic responses — not user prose).
                    let text: String = msg
                        .content
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty() {
                        self.entries.push(Entry::User(text));
                    }
                }
                Role::Assistant => {
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text } if !text.is_empty() => {
                                self.entries.push(Entry::Assistant(text.clone()));
                            }
                            ContentBlock::ToolUse { name, input, .. } => {
                                self.entries.push(Entry::Tool {
                                    name: name.clone(),
                                    args: input.clone(),
                                    status: Some(ToolCallStatus::Ok),
                                    result_text: Some("[history]".into()),
                                });
                            }
                            ContentBlock::Thinking { .. } => {
                                // Signal a thinking block occurred without
                                // dumping the raw chain-of-thought into the
                                // visible log. Rendered dimmed via Note.
                                self.entries.push(Entry::Note("[thinking]".into()));
                            }
                            ContentBlock::Html { source, state } => {
                                // Recreate the canvas renderer (mirrors the
                                // streaming path's plugin `create_renderer`,
                                // which is exactly `HtmlCanvas::new(id, source)`).
                                //
                                // The id is allocated in stream order as we
                                // iterate top-level `Html` blocks, so the n-th
                                // canvas gets `ContentBlockId(n)` — matching the
                                // ordinal Task 26 saved its `state` blob under.
                                // Nested tool-emitted Html lives in
                                // `ToolResult.content`, never as a top-level
                                // assistant block, so it's naturally excluded.
                                let id = self.canvas_registry.allocate_id();
                                let mut renderer: Box<dyn ContentRenderer> =
                                    Box::new(savvagent_canvas::HtmlCanvas::new(id, source));
                                // Restore interactive state if present
                                // (base64 STANDARD → bytes). Decode/restore
                                // failures are soft: log and fall back to
                                // rendering from defaults — never abort resume.
                                if let Some(b64) = state {
                                    use base64::Engine as _;
                                    match base64::engine::general_purpose::STANDARD.decode(b64) {
                                        Ok(bytes) => {
                                            if let Err(e) = renderer.restore_state(&bytes) {
                                                tracing::warn!(
                                                    canvas_id = id.0,
                                                    error = ?e,
                                                    "resume: canvas state restore failed; rendering from defaults"
                                                );
                                            }
                                        }
                                        Err(e) => tracing::warn!(
                                            canvas_id = id.0,
                                            error = ?e,
                                            "resume: canvas state base64 decode failed; rendering from defaults"
                                        ),
                                    }
                                }
                                self.canvas_registry.insert(id, renderer);
                                self.entries.push(Entry::Canvas {
                                    id,
                                    source: source.clone(),
                                    source_preview: None,
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        self.update_metrics();
    }

    /// Legacy slash-command fallback for slashes the plugin router didn't
    /// claim. All commands here either still rely on App-side state
    /// machines that haven't been ported to plugins (e.g. the
    /// `SelectingProvider` InputMode for `/connect`) or are genuinely
    /// unknown.
    ///
    /// The legacy arms for `/clear`, `/save`, `/view`, `/edit`, `/quit`
    /// were removed once their plugin counterparts shipped (PR 5, PR 4,
    /// PR 8 hotfix): leaving the legacy arms intact meant disabling the
    /// owning plugin in `/plugins` had no effect — the slash was still
    /// silently serviced here. Now, when those plugins are disabled,
    /// their slashes fall through to the unknown-command arm.
    pub fn handle_command(&mut self, command: &str) -> bool {
        let parts: Vec<&str> = command.split_whitespace().collect();
        let Some(head) = parts.first() else {
            return false;
        };
        match *head {
            "/connect" => {
                // `/connect` is still partially routed through the legacy
                // `SelectingProvider` InputMode flow when no plugin owns
                // the slash. The `internal:connect` plugin (PR 5) is Core
                // and always present, so this arm only fires if a future
                // build removes that plugin.
                self.open_provider_selector();
                true
            }
            _ if head.starts_with('/') => {
                self.push_note(rust_i18n::t!("notes.unknown-command", cmd = head).to_string());
                true
            }
            _ => false,
        }
    }

    /// Open the `/connect` provider selector.
    pub fn open_provider_selector(&mut self) {
        self.provider_index = self
            .active_provider_id
            .and_then(|id| PROVIDERS.iter().position(|p| p.id == id))
            .unwrap_or(0);
        self.input_mode = InputMode::SelectingProvider;
    }

    /// Advance from provider selection to API-key entry, or cancel if `idx` is OOB.
    ///
    /// The placeholder text reflects whether a credential is already
    /// stored in the keyring: when present, the user can press Enter on
    /// an empty input to reuse it; otherwise the placeholder just hints
    /// at the env-var name.
    pub fn enter_api_key_for(&mut self, idx: usize) {
        let Some(spec) = PROVIDERS.get(idx) else {
            self.input_mode = InputMode::Editing;
            return;
        };
        let has_stored = matches!(crate::creds::load(spec.id), Ok(Some(_)));
        self.pending_provider = Some(spec);
        let mut ta = TextArea::default();
        ta.set_mask_char('●');
        let placeholder = if has_stored {
            rust_i18n::t!(
                "prompt.api-key.use-stored-or-paste-new",
                env = spec.api_key_env
            )
            .to_string()
        } else {
            rust_i18n::t!("prompt.api-key.paste-new", env = spec.api_key_env).to_string()
        };
        ta.set_placeholder_text(placeholder);
        self.api_key_textarea = ta;
        self.input_mode = InputMode::EnteringApiKey;
    }

    /// Read the masked input. Three outcomes:
    ///
    /// * `Some((spec, Some(key)))` — user typed a key and submitted;
    ///   internal state is reset.
    /// * `Some((spec, None))` — modal was open but the input is empty;
    ///   pending state is **preserved** so callers can fall back to a
    ///   stored credential (and, if no stored key exists, restore the
    ///   modal so the user can retry without losing their place).
    /// * `None` — no modal was open; callers should ignore.
    pub fn take_pending_api_key(&mut self) -> Option<(&'static ProviderSpec, Option<String>)> {
        let spec = *self.pending_provider.as_ref()?;
        let key = self.api_key_textarea.lines().join("");
        if key.is_empty() {
            // Keep pending_provider + textarea so the caller can either
            // reuse a stored credential or report the error and let the
            // user keep typing.
            return Some((spec, None));
        }
        self.pending_provider = None;
        self.api_key_textarea = TextArea::default();
        Some((spec, Some(key)))
    }

    /// Abort the `/connect` flow and return to the prompt.
    pub fn cancel_connect(&mut self) {
        self.pending_provider = None;
        self.api_key_textarea = TextArea::default();
        self.input_mode = InputMode::Editing;
    }

    // ---- Effect mutation surface (called by `plugin::effects::apply_effects`) ----

    /// Append a styled-line note to the conversation log. Flattens the
    /// `StyledLine`'s spans into plain text; styling is dropped for now
    /// (preserved in the effect payload for future log-styling work).
    pub fn push_styled_note(&mut self, line: savvagent_plugin::StyledLine) {
        let text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
        self.push_note(text);
    }

    /// Scan the entries backwards for the most recent `Entry::RouteBadge`
    /// and parse it into `(provider, model, reason)`. The badge format is
    /// `"provider/model — Reason"`, written by `apply_turn_event`'s
    /// `RouteSelected` arm. Returns `None` when no badge is present in
    /// this session yet, or when the format can't be parsed; the latter
    /// case logs a `tracing::warn!` so a divergence between the writer
    /// (in `apply_turn_event`) and this reader doesn't fail silently.
    pub fn most_recent_routing_decision(&self) -> Option<(String, String, String)> {
        let badge = self.entries.iter().rev().find_map(|e| match e {
            Entry::RouteBadge(s) => Some(s.as_str()),
            _ => None,
        })?;
        let Some((left, reason)) = badge.split_once(" — ") else {
            tracing::warn!(
                badge = %badge,
                "route badge missing ' — ' separator; format may have changed"
            );
            return None;
        };
        let Some((provider, model)) = left.split_once('/') else {
            tracing::warn!(
                badge = %badge,
                "route badge left half missing '/'; format may have changed"
            );
            return None;
        };
        Some((provider.to_string(), model.to_string(), reason.to_string()))
    }

    /// Owning vec of provider ids that the TUI knows are connected.
    /// Source: the `registered_providers` field populated by the
    /// `RegisterProvider` arm of `apply_effects` (see
    /// `crates/savvagent/src/plugin/effects.rs`). This is **not** a
    /// direct view of the host pool — a provider plugin must emit
    /// `Effect::RegisterProvider` for the id to appear here. In normal
    /// TUI operation that effect is fired by each provider plugin's
    /// `on_event(HostStarting)` callback once a keyring credential is
    /// found, so this list aligns with the host pool the user sees.
    /// Code paths that build a `Host` directly (tests, headless
    /// examples) bypass `apply_effects` and will see this list empty
    /// even when the pool has connected providers — that's the
    /// expected behavior, since the TUI is the source of truth for the
    /// view layer.
    ///
    /// Used by `render_routing_show` (in `main.rs`) to label routing rules
    /// whose target provider isn't connected.
    pub fn connected_provider_ids(&self) -> Vec<savvagent_protocol::ProviderId> {
        self.registered_providers
            .keys()
            .filter_map(|s| match savvagent_protocol::ProviderId::new(s) {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::warn!(
                        provider_id = %s,
                        error = %e,
                        "registered_providers key failed ProviderId round-trip; \
                         /route show may mis-label rules targeting this provider"
                    );
                    None
                }
            })
            .collect()
    }

    /// Clear the conversation log.
    pub fn clear_log(&mut self) {
        self.entries.clear();
        self.live_text.clear();
        self.update_metrics();
    }

    /// Request that the event loop exit on the next tick.
    pub fn request_quit(&mut self) {
        self.should_quit = true;
    }

    /// Replace the prompt textarea contents with `text` and put the cursor at
    /// the very end. Called by `apply_effects` in response to
    /// [`savvagent_plugin::Effect::PrefillInput`]. The command palette emits
    /// `PrefillInput { text: "/cmd " }` for slashes that need a path arg
    /// (e.g. `/view`, `/edit`) so the user can complete the line via the
    /// `@` file picker instead of executing the command with no args.
    pub fn prefill_input(&mut self, text: String) {
        self.input_textarea = make_input_textarea(vec![text]);
        let row = self.input_textarea.lines().len().saturating_sub(1) as u16;
        let col = self
            .input_textarea
            .lines()
            .last()
            .map(|l| l.len())
            .unwrap_or(0) as u16;
        self.input_textarea
            .move_cursor(tui_textarea::CursorMove::Jump(row, col));
    }

    /// Set the active theme by slug. Unknown slugs are surfaced as a
    /// styled note; the in-memory selection is left unchanged.
    ///
    /// Called from `apply_effects` on `Effect::SetActiveTheme`.
    pub fn set_active_theme_by_slug(&mut self, slug: String) {
        match crate::plugin::builtin::themes::catalog::Theme::from_name(&slug) {
            Some(theme) => {
                self.active_theme = theme;
            }
            None => {
                self.push_styled_note(savvagent_plugin::StyledLine::plain(
                    rust_i18n::t!("notes.theme-not-found", slug = slug).to_string(),
                ));
            }
        }
    }

    /// Set the active locale by code. Unknown codes are surfaced as a
    /// styled note; the in-memory selection (and the `rust_i18n` global)
    /// are left unchanged. Returns `true` if the locale was changed,
    /// `false` if the code was rejected.
    ///
    /// Called from `apply_effects` on `Effect::SetActiveLocale`.
    pub fn set_active_language(&mut self, code: String) -> bool {
        if crate::plugin::builtin::language::catalog::is_supported(&code) {
            rust_i18n::set_locale(&code);
            self.active_language = code;
            true
        } else {
            self.push_styled_note(savvagent_plugin::StyledLine::plain(
                rust_i18n::t!("notes.language-not-found", code = code).to_string(),
            ));
            false
        }
    }

    /// Persist the active locale to `~/.savvagent/language.toml`. Errors
    /// surface as a styled note; the in-memory selection is kept either
    /// way. Called from `apply_effects` on `Effect::SetActiveLocale { persist: true }`.
    pub fn persist_language(&mut self) {
        let code = self.active_language.clone();
        match crate::plugin::builtin::language::catalog::save(&code) {
            Ok(()) => {
                let native = crate::plugin::builtin::language::catalog::lookup(&code)
                    .map(|l| l.native_name)
                    .unwrap_or(code.as_str());
                self.push_styled_note(savvagent_plugin::StyledLine::plain(
                    rust_i18n::t!("notes.language-set", native = native).to_string(),
                ));
            }
            Err(e) => {
                self.push_styled_note(savvagent_plugin::StyledLine::plain(
                    rust_i18n::t!(
                        "notes.language-persistence-failed",
                        code = code,
                        err = format!("{e:#}")
                    )
                    .to_string(),
                ));
            }
        }
    }

    /// Persist the active theme to `~/.savvagent/theme.toml`. Errors
    /// surface as a styled note; the in-memory selection is kept either
    /// way so the session-scoped UX is consistent.
    ///
    /// Called from `apply_effects` on `Effect::SetActiveTheme { persist: true }`.
    pub fn persist_config(&mut self) {
        let theme = self.active_theme;
        match crate::plugin::builtin::themes::catalog::save(theme) {
            Ok(()) => {
                self.push_styled_note(savvagent_plugin::StyledLine::plain(
                    rust_i18n::t!("notes.theme-set", slug = theme.name()).to_string(),
                ));
            }
            Err(e) => {
                self.push_styled_note(savvagent_plugin::StyledLine::plain(
                    rust_i18n::t!(
                        "notes.theme-persistence-failed",
                        slug = theme.name(),
                        err = format!("{e:#}")
                    )
                    .to_string(),
                ));
            }
        }
    }

    /// Set the active LLM provider. Stub — full wiring in PR 5.
    #[allow(unused_variables)]
    pub fn set_active_provider(&mut self, id: savvagent_plugin::ProviderId) {
        tracing::debug!("set_active_provider effect ignored in PR 3");
    }

    /// Register a provider announced by a plugin. v0.9 stores the constructed
    /// [`savvagent_mcp::ProviderClient`] in a per-id map and surfaces a note;
    /// PR 7 wires this client into the [`savvagent_host::Host`] tool-loop.
    pub fn register_provider(
        &mut self,
        id: savvagent_plugin::ProviderId,
        display_name: String,
        client: Box<dyn savvagent_mcp::ProviderClient>,
    ) {
        tracing::info!(
            provider_id = %id.as_str(),
            display_name = %display_name,
            "provider registered"
        );
        self.registered_providers
            .insert(id.as_str().to_string(), client);
        self.push_styled_note(savvagent_plugin::StyledLine::plain(
            rust_i18n::t!("splash.connected-to", provider = display_name).to_string(),
        ));
    }

    /// Save transcript to the given path. Serializes `entries` to a JSON array
    /// of strings (one element per entry) and writes to `path`.
    pub fn save_transcript_to(&mut self, path: String) -> std::io::Result<()> {
        let lines: Vec<String> = self
            .entries
            .iter()
            .map(|e| match e {
                Entry::User(t) => format!("user: {t}"),
                Entry::Assistant(t) => format!("assistant: {t}"),
                Entry::Tool {
                    name, args, status, ..
                } => {
                    let status_label = match status {
                        Some(ToolCallStatus::Ok) => "ok",
                        Some(ToolCallStatus::Errored) => "error",
                        None => "in-flight",
                    };
                    let args_str = serde_json::to_string(args).unwrap_or_default();
                    format!("tool: {name}({args_str}) [{status_label}]")
                }
                Entry::RouteBadge(t) => format!("route: {t}"),
                Entry::Note(t) => format!("note: {t}"),
                Entry::Canvas { id, source, .. } => {
                    format!("canvas: id={} source_len={}", id.0, source.len())
                }
            })
            .collect();
        let json = serde_json::to_string_pretty(&lines).map_err(std::io::Error::other)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Submit a prompt to the active provider. Stub — full wiring in PR 5.
    #[allow(unused_variables)]
    pub fn submit_prompt(&mut self, text: String) {
        tracing::debug!("submit_prompt effect ignored in PR 3");
    }

    /// Take the one-turn model override out of `App`, leaving `None` behind.
    ///
    /// Called at the worker-spawn site before every user turn so the override
    /// is consumed exactly once (the turn it was set for). Returns `Some(id)`
    /// if a `SetNextTurnModelOverride` effect fired before this turn, `None`
    /// otherwise.
    pub(crate) fn consume_model_override(&mut self) -> Option<String> {
        self.next_turn_model_override.take()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Scan `dir` for `*.json` transcript files and return picker rows
/// sorted newest-first.
///
/// Uses two strategies for ordering:
/// 1. The `saved_at` timestamp inside the file (versioned format).
/// 2. The numeric stem of the filename (`<unix>.json`) for legacy files.
///
/// Files that cannot be read or parsed as JSON are silently skipped.
pub fn collect_transcript_entries(dir: &std::path::Path) -> Vec<TranscriptEntry> {
    use savvagent_protocol::ContentBlock;

    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut entries: Vec<(u64, TranscriptEntry)> = Vec::new();

    for item in read_dir.flatten() {
        let path = item.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        // Try to parse for metadata. On any failure, skip.
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(root) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };

        let (saved_at, messages) = match &root {
            serde_json::Value::Object(map) if map.contains_key("schema_version") => {
                // `Host::load_transcript` requires the full `TranscriptFile`
                // (with non-Option `messages`) to deserialize, so a row whose
                // `messages` field is missing or unparseable would always
                // produce a `Malformed` error on selection. Skip those —
                // consistent with the docstring contract above.
                let Some(msgs_val) = map.get("messages") else {
                    continue;
                };
                let Ok(msgs) =
                    serde_json::from_value::<Vec<savvagent_protocol::Message>>(msgs_val.clone())
                else {
                    continue;
                };
                let sa = map.get("saved_at").and_then(|v| v.as_u64()).unwrap_or(0);
                (sa, msgs)
            }
            serde_json::Value::Array(_) => {
                let Ok(msgs) =
                    serde_json::from_value::<Vec<savvagent_protocol::Message>>(root.clone())
                else {
                    continue;
                };
                // Fall back to stem-as-timestamp for legacy files.
                let sa = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                (sa, msgs)
            }
            _ => continue,
        };

        // Sort key: prefer saved_at, fall back to stem.
        let sort_key = if saved_at > 0 {
            saved_at
        } else {
            path.file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
        };

        let timestamp = if saved_at > 0 {
            format_unix_ts(saved_at)
        } else {
            // Legacy: stem is already the unix ts.
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
            stem.parse::<u64>()
                .map(format_unix_ts)
                .unwrap_or_else(|_| stem.to_owned())
        };

        // First user message text as preview.
        let preview = messages
            .iter()
            .find(|m| m.role == savvagent_protocol::Role::User)
            .and_then(|m| {
                m.content.iter().find_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(truncate(text, 60))
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_else(|| "(empty)".into());

        entries.push((
            sort_key,
            TranscriptEntry {
                path,
                timestamp,
                preview,
                message_count: messages.len(),
            },
        ));
    }

    // Newest first.
    entries.sort_by_key(|e| std::cmp::Reverse(e.0));
    entries.into_iter().map(|(_, e)| e).collect()
}

/// Format a Unix timestamp as a local-time-like string.
/// Uses naive UTC formatting since we don't pull in a chrono dep.
fn format_unix_ts(secs: u64) -> String {
    // Simple: express as YYYY-MM-DD HH:MM:SS UTC.
    let s = secs;
    let sec = s % 60;
    let min = (s / 60) % 60;
    let hour = (s / 3600) % 24;
    let days = s / 86400;
    // Days since Unix epoch → Gregorian calendar (Proleptic).
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}")
}

/// Minimal Gregorian calendar conversion for Unix-epoch day count.
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fresh_app() -> App {
        App::new("test-model".into(), PathBuf::from("/tmp"), "en".to_string())
    }

    /// Verify that `PluginRegistry::active_prompt_segments` — the value that
    /// `main` pushes into `Host::set_prompt_segments` at startup — includes
    /// the html-canvas segment when the full builtin set is registered.
    ///
    /// This is the unit-side of the startup wiring: the production code does
    /// `host.set_prompt_segments(registry.active_prompt_segments())` right
    /// after `app.install_plugin_runtime(registry, indexes)`. The host's
    /// `set_prompt_segments` / `active_prompt_segments` round-trip is covered
    /// in `savvagent-host`; this test pins the "what gets pushed" side.
    #[tokio::test]
    async fn startup_pushes_html_canvas_segment_to_host() {
        let _lock = crate::test_helpers::HOME_LOCK.lock().unwrap();
        let set = crate::plugin::register_builtins(
            std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::BTreeMap::new())),
            std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::plugin::builtin::user_hooks::discovery::HooksIndex::default(),
            )),
            "test-session".into(),
            std::path::PathBuf::from("/tmp"),
            std::sync::Arc::new(tokio::sync::RwLock::new(std::path::PathBuf::from(
                "/t.json",
            ))),
        );
        let registry = crate::plugin::registry::PluginRegistry::new(set);
        let segments = registry.active_prompt_segments();
        assert!(
            segments
                .iter()
                .any(|s| s.id == "internal:html-canvas:default"),
            "expected internal:html-canvas:default in active segments, got: {:?}",
            segments.iter().map(|s| &s.id).collect::<Vec<_>>()
        );
    }

    /// `App::new` seeds `transcript_path` to a real path inside the
    /// transcript directory (`<transcript_dir>/<session_id>.json`) so the
    /// user-hooks plugin sees a real file location in stdin payloads even
    /// before the first save fires.
    #[tokio::test]
    async fn new_app_transcript_path_is_session_scoped() {
        let app = App::new(
            "test-model".into(),
            PathBuf::from("/tmp/transcripts"),
            "en".to_string(),
        );
        let path = app.transcript_path.read().await.clone();
        // Path is inside the transcript dir...
        assert_eq!(
            path.parent(),
            Some(std::path::Path::new("/tmp/transcripts"))
        );
        // ...and is named after the session id with a .json extension.
        let expected_name = format!("{}.json", app.session_id);
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(expected_name.as_str())
        );
    }

    /// Auto-tail: when the user hasn't scrolled and content overflows the
    /// viewport, the bottom row of the viewport is the newest line.
    #[test]
    fn log_scroll_y_auto_tail_pins_newest_to_bottom() {
        // 50 wrapped lines, 10 visible → 40 lines hidden above (bottom of
        // line 50 sits on the last viewport row).
        assert_eq!(log_scroll_y(50, 10, None), 40);
    }

    /// `Some(n)` keeps the viewport `n` rows above the bottom, regardless
    /// of how much content has accumulated. Newly arriving content grows
    /// `max_scroll` and `scroll_y` in lockstep so the visible window
    /// doesn't jump.
    #[test]
    fn log_scroll_y_scrolled_back_holds_offset() {
        // Same 50/10 layout but scrolled 5 rows back: scroll_y = 40 - 5.
        assert_eq!(log_scroll_y(50, 10, Some(5)), 35);
        // Add 20 more lines (total 70). scroll_y must move by 20 too so
        // the same 5-rows-from-bottom window stays put.
        assert_eq!(log_scroll_y(70, 10, Some(5)), 55);
    }

    /// `Some(u16::MAX)` is the "scroll to top" sentinel emitted by the
    /// Home key. For conversations that fit in `u16::MAX` wrapped rows
    /// (effectively every realistic session) it clamps to `0` — the very
    /// top of history. For pathologically long histories the row index
    /// can't be represented in `u16` at all, so it clamps to the deepest
    /// row ratatui's scroll API can address (`u16::MAX`).
    #[test]
    fn log_scroll_y_top_sentinel_jumps_to_top() {
        assert_eq!(log_scroll_y(50, 10, Some(u16::MAX)), 0);
        assert_eq!(log_scroll_y(u16::MAX as usize + 10, 10, Some(u16::MAX)), 0);
        // Beyond u16::MAX rows, scroll API hits its own ceiling.
        assert_eq!(
            log_scroll_y(1_000_000, 10, Some(u16::MAX)),
            u16::MAX,
            "huge histories saturate at the deepest u16-addressable row",
        );
    }

    /// When the viewport is bigger than the content, there's nothing to
    /// scroll — `scroll_y` must be 0 in every mode (no underflow, no
    /// reverse scroll).
    #[test]
    fn log_scroll_y_short_content_is_top_anchored() {
        assert_eq!(log_scroll_y(0, 10, None), 0);
        assert_eq!(log_scroll_y(0, 10, Some(5)), 0);
        assert_eq!(log_scroll_y(10, 10, None), 0);
        assert_eq!(log_scroll_y(10, 10, Some(99)), 0);
    }

    /// Wheel up from auto-tail (`None`) enters scrollback at exactly one
    /// step worth of rows — the user starts seeing history without having
    /// to over-scroll past the current bottom.
    #[test]
    fn wheel_up_from_autotail_enters_scrollback() {
        assert_eq!(
            log_scroll_offset_after_wheel(None, WheelDirection::Up, 3),
            Some(3)
        );
    }

    /// Repeated wheel ups accumulate; saturation keeps a stuck-down wheel
    /// from panicking on overflow. (Realistic terminals can't emit 65k+
    /// notches per loop tick, but the wheel handler runs on every event so
    /// the math has to be infallible.)
    #[test]
    fn wheel_up_accumulates_and_saturates() {
        assert_eq!(
            log_scroll_offset_after_wheel(Some(10), WheelDirection::Up, 3),
            Some(13)
        );
        assert_eq!(
            log_scroll_offset_after_wheel(Some(u16::MAX - 1), WheelDirection::Up, 5),
            Some(u16::MAX)
        );
    }

    /// Wheel down decrements and snaps back to `None` (auto-tail) when the
    /// offset would reach zero — same contract `PageDown` honors, so a
    /// session that mixes wheel + keyboard never lands in the ambiguous
    /// "parked at bottom of scrollback" state.
    #[test]
    fn wheel_down_decrements_and_snaps_to_autotail() {
        assert_eq!(
            log_scroll_offset_after_wheel(Some(10), WheelDirection::Down, 3),
            Some(7)
        );
        // Step would take us to 0 → snap to auto-tail.
        assert_eq!(
            log_scroll_offset_after_wheel(Some(3), WheelDirection::Down, 3),
            None
        );
        // Step exceeds the offset → also snap to auto-tail (no underflow).
        assert_eq!(
            log_scroll_offset_after_wheel(Some(2), WheelDirection::Down, 3),
            None
        );
        // Already at auto-tail → stay at auto-tail.
        assert_eq!(
            log_scroll_offset_after_wheel(None, WheelDirection::Down, 3),
            None
        );
    }

    /// `make_input_textarea` must apply the wrap+grow + history-depth
    /// settings. Reset paths that bypass this helper would silently
    /// regress to a single-row scrolling input or a 50-entry undo
    /// stack, so this test pins the configuration.
    #[test]
    fn make_input_textarea_configures_wrap_and_row_bounds() {
        let ta = make_input_textarea(Vec::<String>::new());
        assert_eq!(ta.wrap_mode(), WrapMode::WordOrGlyph);
        assert_eq!(ta.min_rows(), INPUT_MIN_ROWS);
        assert_eq!(ta.max_rows(), INPUT_MAX_ROWS);
        assert_eq!(ta.max_histories(), INPUT_MAX_HISTORIES);

        let ta_seeded = make_input_textarea(vec!["seed".to_string()]);
        assert_eq!(ta_seeded.lines(), &["seed".to_string()]);
        assert_eq!(ta_seeded.wrap_mode(), WrapMode::WordOrGlyph);
        assert_eq!(ta_seeded.max_histories(), INPUT_MAX_HISTORIES);
    }

    /// A long single line must report a `preferred_rows` height larger
    /// than the minimum when wrapped at a narrow width — confirming the
    /// dynamic-height computation in `ui::render` actually grows the
    /// input box rather than horizontally scrolling out of view.
    #[test]
    fn long_line_measures_taller_than_min_rows() {
        let mut ta = make_input_textarea(vec!["x".repeat(200)]);
        // 20 cols outer width → ~18 cols inner content after borders +
        // 1-col horizontal padding on each side. 200/18 ≈ 12 visual rows
        // pre-clamp, but `set_max_rows(INPUT_MAX_ROWS=10)` caps the
        // preferred rows.
        let m = ta.measure(20);
        assert!(
            m.preferred_rows > INPUT_MIN_ROWS,
            "wrapped long line should grow the input above the minimum; got {}",
            m.preferred_rows
        );
        assert!(
            m.preferred_rows <= INPUT_MAX_ROWS,
            "input height must be clamped at INPUT_MAX_ROWS; got {}",
            m.preferred_rows
        );
    }

    #[test]
    fn empty_filter_lists_every_command() {
        let app = fresh_app();
        let filtered = app.filtered_command_indices();
        assert_eq!(filtered.len(), app.commands.len());
    }

    #[test]
    fn filter_narrows_by_prefix_case_insensitive() {
        let mut app = fresh_app();
        app.palette_filter = "co".into();
        let names: Vec<&str> = app
            .filtered_command_indices()
            .into_iter()
            .map(|i| app.commands[i].name.as_str())
            .collect();
        assert_eq!(names, vec!["/connect"]);

        app.palette_filter = "C".into();
        let names: Vec<&str> = app
            .filtered_command_indices()
            .into_iter()
            .map(|i| app.commands[i].name.as_str())
            .collect();
        assert!(names.contains(&"/connect"));
        assert!(names.contains(&"/clear"));
    }

    #[test]
    fn filter_with_no_matches_returns_empty_list() {
        let mut app = fresh_app();
        app.palette_filter = "xyz".into();
        assert!(app.filtered_command_indices().is_empty());
    }

    #[test]
    fn select_no_arg_command_returns_execute_with_empty_input() {
        let mut app = fresh_app();
        app.palette_filter = "c".into();
        // Two visible commands at this point: /connect (0) and /clear (1).
        app.command_index = 1;
        let outcome = app.select_command();
        assert_eq!(outcome, Some(CommandSelection::Execute("/clear".into())));
        assert!(matches!(app.input_mode, InputMode::Editing));
        assert_eq!(app.input_textarea.lines(), &[String::new()]);
        assert!(app.palette_filter.is_empty());
    }

    #[test]
    fn select_arg_command_returns_prefill_with_seeded_input() {
        let mut app = fresh_app();
        app.palette_filter = "vi".into();
        app.command_index = 0;
        let outcome = app.select_command();
        assert_eq!(outcome, Some(CommandSelection::Prefill("/view".into())));
        assert_eq!(app.input_textarea.lines(), &["/view ".to_string()]);
    }

    #[test]
    fn select_with_no_match_closes_palette() {
        let mut app = fresh_app();
        app.palette_filter = "zzz".into();
        let outcome = app.select_command();
        assert!(outcome.is_none());
        assert!(matches!(app.input_mode, InputMode::Editing));
    }

    #[test]
    fn pop_past_empty_signals_close() {
        let mut app = fresh_app();
        app.palette_push_char('c');
        assert!(app.palette_pop_char());
        assert!(!app.palette_pop_char());
    }

    #[test]
    fn permission_request_enters_prompt_mode() {
        let mut app = fresh_app();
        app.apply_turn_event(TurnEvent::PermissionRequested {
            id: 42,
            name: "run".into(),
            summary: "run: ls".into(),
            args: serde_json::json!({"command": "ls"}),
        });
        assert!(matches!(app.input_mode, InputMode::PermissionPrompt));
        let req = app.pending_permission.expect("pending should be set");
        assert_eq!(req.id, 42);
        assert_eq!(req.name, "run");
    }

    #[test]
    fn bash_command_parses_net_flag() {
        let p = parse_bash_command("--net curl https://example.com").unwrap();
        assert_eq!(p.net_override, NetOverride::ForceAllow);
        assert_eq!(p.command, "curl https://example.com");
    }

    #[test]
    fn bash_command_parses_no_net_flag() {
        let p = parse_bash_command("--no-net ls /tmp").unwrap();
        assert_eq!(p.net_override, NetOverride::ForceDeny);
        assert_eq!(p.command, "ls /tmp");
    }

    #[test]
    fn bash_command_without_flag_has_no_override() {
        let p = parse_bash_command("ls /tmp").unwrap();
        assert_eq!(p.net_override, NetOverride::Inherit);
        assert_eq!(p.command, "ls /tmp");
    }

    #[test]
    fn bash_command_flag_only_recognised_at_start() {
        // A --net mid-command is part of the command body.
        let p = parse_bash_command("echo --net hi").unwrap();
        assert_eq!(p.net_override, NetOverride::Inherit);
        assert_eq!(p.command, "echo --net hi");
    }

    #[test]
    fn bash_command_empty_after_flag_is_an_error() {
        assert!(matches!(
            parse_bash_command("--net   ").unwrap_err(),
            BashCommandError::EmptyCommand
        ));
        assert!(matches!(
            parse_bash_command("").unwrap_err(),
            BashCommandError::EmptyCommand
        ));
    }

    #[test]
    fn bash_command_leading_whitespace_trimmed() {
        let p = parse_bash_command("   --net  echo hi").unwrap();
        assert_eq!(p.net_override, NetOverride::ForceAllow);
        assert_eq!(p.command, "echo hi");
    }

    #[test]
    fn bash_command_rejects_single_dash_typo() {
        let err = parse_bash_command("-net curl foo").unwrap_err();
        assert!(matches!(err, BashCommandError::UnknownFlag { .. }));
    }

    #[test]
    fn bash_command_rejects_capitalised_flag() {
        assert!(matches!(
            parse_bash_command("--Net curl foo").unwrap_err(),
            BashCommandError::UnknownFlag { .. }
        ));
    }

    #[test]
    fn bash_command_rejects_net_with_equals() {
        assert!(matches!(
            parse_bash_command("--net=true curl foo").unwrap_err(),
            BashCommandError::UnknownFlag { .. }
        ));
    }

    #[test]
    fn bash_command_rejects_unknown_dash_token() {
        assert!(matches!(
            parse_bash_command("--quiet ls").unwrap_err(),
            BashCommandError::UnknownFlag { .. }
        ));
    }

    #[test]
    fn bash_command_net_alone_without_command_is_an_error() {
        // `--net` followed by only whitespace — must error EmptyCommand,
        // not UnknownFlag.
        assert!(matches!(
            parse_bash_command("--net").unwrap_err(),
            BashCommandError::EmptyCommand
        ));
    }

    #[test]
    fn bash_network_request_enters_modal_with_id_and_summary() {
        let mut app = fresh_app();
        app.apply_turn_event(TurnEvent::BashNetworkRequested {
            id: 7,
            summary: savvagent_host::BASH_NETWORK_PROMPT_SUMMARY.into(),
        });
        match &app.input_mode {
            InputMode::BashNetworkPrompt { id, summary } => {
                assert_eq!(*id, 7);
                assert!(summary.contains("tool-bash"), "summary: {summary}");
            }
            other => panic!(
                "expected BashNetworkPrompt, got {:?}",
                input_mode_label(other)
            ),
        }
    }

    fn input_mode_label(m: &InputMode) -> &'static str {
        match m {
            InputMode::Editing => "Editing",
            InputMode::ViewingFile => "ViewingFile",
            InputMode::EditingFile => "EditingFile",
            InputMode::SelectingProvider => "SelectingProvider",
            InputMode::EnteringApiKey => "EnteringApiKey",
            InputMode::PermissionPrompt => "PermissionPrompt",
            InputMode::BashNetworkPrompt { .. } => "BashNetworkPrompt",
            InputMode::SelectingTranscript => "SelectingTranscript",
            InputMode::Canvas { .. } => "Canvas",
        }
    }

    fn collect_app_notes(app: &App) -> Vec<String> {
        app.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Note(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn set_active_language_known_code_updates_rust_i18n() {
        use crate::test_helpers::HOME_LOCK;
        let _lock = HOME_LOCK.lock().unwrap();

        let mut app = fresh_app();
        let changed = app.set_active_language("es".to_string());
        assert!(changed, "known code must return true");
        assert_eq!(app.active_language, "es");
        assert_eq!(&*rust_i18n::locale(), "es");

        rust_i18n::set_locale("en");
    }

    #[test]
    fn set_active_language_unknown_code_pushes_note_and_does_not_mutate() {
        let mut app = fresh_app();
        let before = app.active_language.clone();
        let changed = app.set_active_language("xx".to_string());
        assert!(!changed, "unknown code must return false");
        assert_eq!(
            app.active_language, before,
            "unknown code must not mutate active_language"
        );
        let notes = collect_app_notes(&app);
        assert!(
            notes.last().map(|n| n.contains("xx")).unwrap_or(false),
            "notes: {:?}",
            notes
        );
    }

    #[test]
    fn persist_language_writes_file_and_pushes_note() {
        use crate::test_helpers::{HOME_LOCK, HomeGuard};
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();

        let mut app = fresh_app();
        let _ = app.set_active_language("pt".to_string());
        app.persist_language();

        let path = crate::plugin::builtin::language::catalog::config_path()
            .expect("HOME set in HomeGuard");
        let text = std::fs::read_to_string(&path).expect("file should be written");
        assert!(text.contains(r#"language = "pt""#), "file content: {text}");

        let notes = collect_app_notes(&app);
        let last = notes.last().cloned().unwrap_or_default();
        assert!(
            last.contains("Português"),
            "expected native name in note, got: {last}"
        );

        rust_i18n::set_locale("en");
    }

    #[test]
    fn most_recent_routing_decision_parses_badge() {
        let mut app = fresh_app();
        app.entries.push(Entry::RouteBadge(
            "anthropic/claude-opus-4-7 — Override".into(),
        ));
        app.entries.push(Entry::Assistant("hi".into()));
        let got = app.most_recent_routing_decision().expect("parses");
        assert_eq!(got.0, "anthropic");
        assert_eq!(got.1, "claude-opus-4-7");
        assert_eq!(got.2, "Override");
    }

    #[test]
    fn most_recent_routing_decision_none_when_no_badge() {
        let app = fresh_app();
        assert!(app.most_recent_routing_decision().is_none());
    }

    #[test]
    fn most_recent_routing_decision_parses_rule_badge() {
        let mut app = fresh_app();
        app.entries.push(Entry::RouteBadge(
            "anthropic/claude-opus-4-7 — Rule(deep-reasoning)".into(),
        ));
        let got = app.most_recent_routing_decision().expect("parses");
        assert_eq!(got.0, "anthropic");
        assert_eq!(got.1, "claude-opus-4-7");
        assert_eq!(got.2, "Rule(deep-reasoning)");
    }

    #[test]
    fn most_recent_routing_decision_parses_heuristic_badge() {
        // Round-trip pin against `savvagent_host::router::heuristics::HeuristicKind`'s
        // `Display`: a rename of "short"/"coding" would silently break the badge
        // parser without this test. The Display output is a cross-crate contract.
        let mut app = fresh_app();
        app.entries.push(Entry::RouteBadge(
            "anthropic/claude-haiku-4-5 — Heuristic(short)".into(),
        ));
        let got = app.most_recent_routing_decision().expect("parses");
        assert_eq!(got.0, "anthropic");
        assert_eq!(got.1, "claude-haiku-4-5");
        assert_eq!(got.2, "Heuristic(short)");

        let mut app = fresh_app();
        app.entries.push(Entry::RouteBadge(
            "anthropic/claude-opus-4-7 — Heuristic(coding)".into(),
        ));
        let got = app.most_recent_routing_decision().expect("parses");
        assert_eq!(got.0, "anthropic");
        assert_eq!(got.1, "claude-opus-4-7");
        assert_eq!(got.2, "Heuristic(coding)");
    }

    #[test]
    fn most_recent_routing_decision_warns_on_unparseable_badge() {
        // Badge that lacks the " — " separator. The contract under
        // test is "returns None on parse failure"; the parser also
        // fires a `tracing::warn!` but that side-effect is not
        // observable here without a tracing harness.
        let mut app = fresh_app();
        app.entries
            .push(Entry::RouteBadge("malformed-no-separator".into()));
        assert!(app.most_recent_routing_decision().is_none());
    }

    #[test]
    fn entry_carries_canvas_variant() {
        let e = Entry::Canvas {
            id: savvagent_plugin::ContentBlockId(7),
            source: "<p>hi</p>".into(),
            source_preview: None,
        };
        match e {
            Entry::Canvas {
                id,
                source,
                source_preview,
            } => {
                assert_eq!(id, savvagent_plugin::ContentBlockId(7));
                assert_eq!(source, "<p>hi</p>");
                assert!(source_preview.is_none());
            }
            _ => panic!("expected Canvas"),
        }
    }

    /// Verify that `save_transcript_to` round-trips a Canvas entry into
    /// the plain-text JSON transcript. The serialized form is a string
    /// starting with `"canvas: id=3"`.
    #[test]
    fn canvas_entry_persists_to_transcript() {
        use std::path::PathBuf;
        use tempfile::NamedTempFile;

        let mut app = App::new("model".into(), PathBuf::from("/tmp"), "en".to_string());
        app.entries.push(Entry::Canvas {
            id: savvagent_plugin::ContentBlockId(3),
            source: "<p>x</p>".into(),
            source_preview: None,
        });

        let tmp = NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_str().unwrap().to_string();
        app.save_transcript_to(path.clone())
            .expect("save should succeed");

        let written = std::fs::read_to_string(&path).expect("read back");
        assert!(
            written.contains("canvas: id=3"),
            "expected canvas entry in transcript, got: {written}"
        );
        assert!(
            written.contains("source_len=8"),
            "expected source_len in transcript, got: {written}"
        );
    }

    #[test]
    fn focus_canvas_sets_input_mode_and_is_focused() {
        let mut app = fresh_app();
        let id = savvagent_plugin::ContentBlockId(7);
        // No renderer registered for `id`; freeze/thaw must no-op gracefully.
        app.focus_canvas(id, Some(2));
        assert!(app.is_canvas_focused(id));
        if let InputMode::Canvas { id: x, element_idx } = app.input_mode {
            assert_eq!(x, id);
            assert_eq!(element_idx, Some(2));
        } else {
            panic!("expected Canvas input mode");
        }
    }

    #[test]
    fn unfocus_canvas_returns_to_editing() {
        let mut app = fresh_app();
        let id = savvagent_plugin::ContentBlockId(8);
        app.focus_canvas(id, None);
        app.unfocus_canvas();
        assert!(matches!(app.input_mode, InputMode::Editing));
        assert!(!app.is_canvas_focused(id));
    }

    #[test]
    fn canvas_hit_maps_cell_to_pixel_offset_in_matching_canvas() {
        let id = savvagent_plugin::ContentBlockId(3);
        let rect = ratatui::layout::Rect {
            x: 10,
            y: 5,
            width: 40,
            height: 12,
        };
        let cell = savvagent_canvas::CellPixelSize {
            width: 8,
            height: 16,
        };
        let targets = vec![(id, rect)];
        // Top-left cell → (0, 0).
        assert_eq!(canvas_hit(&targets, 10, 5, cell), Some((id, 0, 0)));
        // Two cells right, one cell down → (16, 16).
        assert_eq!(canvas_hit(&targets, 12, 6, cell), Some((id, 16, 16)));
    }

    #[test]
    fn canvas_hit_returns_none_outside_every_rect() {
        let id = savvagent_plugin::ContentBlockId(3);
        let rect = ratatui::layout::Rect {
            x: 10,
            y: 5,
            width: 40,
            height: 12,
        };
        let cell = savvagent_canvas::CellPixelSize {
            width: 8,
            height: 16,
        };
        let targets = vec![(id, rect)];
        assert_eq!(canvas_hit(&targets, 9, 5, cell), None); // left of
        assert_eq!(canvas_hit(&targets, 50, 5, cell), None); // right of (col 10+40)
        assert_eq!(canvas_hit(&targets, 10, 4, cell), None); // above
        assert_eq!(canvas_hit(&targets, 10, 17, cell), None); // below (row 5+12)
        assert_eq!(canvas_hit(&[], 10, 5, cell), None); // no targets
    }

    #[test]
    fn canvas_hit_picks_first_matching_canvas() {
        let cell = savvagent_canvas::CellPixelSize {
            width: 10,
            height: 20,
        };
        let a = savvagent_plugin::ContentBlockId(1);
        let b = savvagent_plugin::ContentBlockId(2);
        let targets = vec![
            (
                a,
                ratatui::layout::Rect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 5,
                },
            ),
            (
                b,
                ratatui::layout::Rect {
                    x: 20,
                    y: 0,
                    width: 10,
                    height: 5,
                },
            ),
        ];
        assert_eq!(canvas_hit(&targets, 5, 2, cell), Some((a, 50, 40)));
        assert_eq!(canvas_hit(&targets, 22, 1, cell), Some((b, 20, 20)));
    }

    #[test]
    fn canvas_registry_allocates_unique_ids() {
        let mut reg = CanvasRegistry::new();
        let id0 = reg.allocate_id();
        let id1 = reg.allocate_id();
        assert_eq!(id0, ContentBlockId(0));
        assert_eq!(id1, ContentBlockId(1));
        assert_ne!(id0, id1);
    }

    #[test]
    fn app_has_canvas_registry_field() {
        let app = fresh_app();
        // Just confirm the field is accessible; the bool value depends on the
        // host terminal and is not meaningful in a unit test.
        let _ = app.canvas_registry.image_protocol_available();
    }

    #[test]
    fn entry_tool_carries_raw_value_and_result_text() {
        // Construct an Entry::Tool with the new field shape and assert the
        // fields are accessible at their new names and types.
        let entry = Entry::Tool {
            name: "read_file".to_string(),
            args: serde_json::json!({"path": "src/main.rs"}),
            status: Some(ToolCallStatus::Ok),
            result_text: Some(r#"{"bytes": 1234}"#.to_string()),
        };
        let Entry::Tool {
            name,
            args,
            status,
            result_text,
        } = entry
        else {
            panic!("expected Entry::Tool");
        };
        assert_eq!(name, "read_file");
        assert_eq!(args.get("path").unwrap(), &serde_json::json!("src/main.rs"));
        assert_eq!(status, Some(ToolCallStatus::Ok));
        assert_eq!(result_text.as_deref(), Some(r#"{"bytes": 1234}"#));
    }

    // Frame -> DynamicImage conversion ----------------------------------

    #[test]
    fn frame_to_dynamic_image_accepts_well_formed_rgba8() {
        // 2x2 image, 4 bytes/pixel = 16 bytes total.
        let frame = savvagent_plugin::Frame {
            width: 2,
            height: 2,
            format: PixelFormat::Rgba8,
            bytes: vec![
                255, 0, 0, 255, // red
                0, 255, 0, 255, // green
                0, 0, 255, 255, // blue
                255, 255, 255, 255, // white
            ],
        };
        let img = frame_to_dynamic_image(&frame).expect("conversion should succeed");
        assert_eq!(img.width(), 2);
        assert_eq!(img.height(), 2);
    }

    #[test]
    fn frame_to_dynamic_image_swaps_bgra_channels() {
        // BGRA input: (0,0,255,255) is red in BGRA but must read as red
        // (255,0,0,255) in the resulting RGBA image.
        let frame = savvagent_plugin::Frame {
            width: 1,
            height: 1,
            format: PixelFormat::Bgra8,
            bytes: vec![0, 0, 255, 255], // BGRA = pure red
        };
        let img = frame_to_dynamic_image(&frame).expect("conversion should succeed");
        let rgba = img.to_rgba8();
        let px = rgba.get_pixel(0, 0);
        assert_eq!(
            px.0,
            [255, 0, 0, 255],
            "BGRA byte order must be swapped to RGBA"
        );
    }

    #[test]
    fn frame_to_dynamic_image_rejects_zero_dimensions() {
        let frame = savvagent_plugin::Frame {
            width: 0,
            height: 10,
            format: PixelFormat::Rgba8,
            bytes: vec![],
        };
        assert!(frame_to_dynamic_image(&frame).is_none());
    }

    #[test]
    fn frame_to_dynamic_image_rejects_mismatched_byte_length() {
        // 2x2 should be 16 bytes; pass 8 and confirm we don't panic.
        let frame = savvagent_plugin::Frame {
            width: 2,
            height: 2,
            format: PixelFormat::Rgba8,
            bytes: vec![0; 8],
        };
        assert!(frame_to_dynamic_image(&frame).is_none());
    }

    /// Streaming HTML block: source_preview accumulates during streaming,
    /// then moves to source on ContentBlockStop.
    ///
    /// Renderer creation (the async half) is NOT tested here because it
    /// requires a live plugin registry with `HtmlCanvasPlugin` installed.
    /// That integration path is exercised in `main.rs`'s `create_canvas_renderer`
    /// call after `TurnEvent::HtmlBlockStop` is processed.
    #[test]
    fn streaming_html_block_transitions_to_canvas_on_stop() {
        let mut app = fresh_app();

        // Start: a fresh canvas entry with empty source_preview is pushed.
        let block_id = app.handle_html_block_start();

        // Delta: fragments are appended to source_preview.
        app.handle_html_block_delta(block_id, "<!doctype");
        app.handle_html_block_delta(block_id, " html><body>hi</body>");

        // While streaming, the entry has source_preview = Some(...).
        let entry = app.last_entry().expect("entry pushed");
        match entry {
            Entry::Canvas {
                source_preview,
                source,
                ..
            } => {
                assert_eq!(
                    source_preview.as_deref(),
                    Some("<!doctype html><body>hi</body>"),
                    "source_preview accumulates fragments during streaming"
                );
                assert!(source.is_empty(), "source must stay empty while streaming");
            }
            _ => panic!("expected Canvas entry, got {entry:?}"),
        }

        // Stop: preview is swapped into source and set to None.
        app.handle_html_block_stop(block_id);

        let entry = app.last_entry().expect("entry");
        match entry {
            Entry::Canvas {
                id,
                source,
                source_preview,
            } => {
                assert!(
                    source_preview.is_none(),
                    "source_preview must be None after block stop"
                );
                assert_eq!(
                    source, "<!doctype html><body>hi</body>",
                    "source must hold the assembled HTML after block stop"
                );
                // Renderer creation happens asynchronously in main.rs after
                // TurnEvent::HtmlBlockStop; not testable in sync unit tests.
                // We only verify the id is stable.
                assert_eq!(*id, block_id);
            }
            _ => panic!("expected Canvas entry, got {entry:?}"),
        }
    }

    /// Auto-export writes a canvas file to `~/.savvagent/canvases/` after
    /// `handle_html_block_stop` finalizes the source, simulating the call
    /// sequence used by `main.rs::auto_export_canvas`.
    #[test]
    fn auto_export_writes_file_on_block_stop() {
        use crate::plugin::builtin::html_canvas::auto_export::{
            auto_export_path, canvases_dir, write_canvas,
        };
        use crate::test_helpers::{HOME_LOCK, HomeGuard};

        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();

        let mut app = fresh_app();
        let id = app.handle_html_block_start();
        app.handle_html_block_delta(id, "<p>hi</p>");
        app.handle_html_block_stop(id);

        // Retrieve the finalized source (mirrors auto_export_canvas in main.rs).
        let source = match app.last_entry().expect("canvas entry") {
            Entry::Canvas { source, .. } => source.clone(),
            other => panic!("expected Canvas, got {other:?}"),
        };

        let base = canvases_dir().expect("HOME set by HomeGuard");
        let path = auto_export_path(&base, 1_716_300_000, 1, id);
        write_canvas(&path, &source).expect("write_canvas");

        assert!(path.exists(), "canvas file must be created");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "<p>hi</p>");
    }

    /// `apply_turn_event` routes `HtmlBlockStart/Delta/Stop` through the
    /// handler methods and keeps the `html_block_index_to_id` map in sync.
    #[test]
    fn apply_turn_event_html_block_roundtrip() {
        use savvagent_host::TurnEvent;

        let mut app = fresh_app();

        app.apply_turn_event(TurnEvent::HtmlBlockStart { index: 2 });
        // After start: one canvas entry with empty preview; index mapped.
        assert_eq!(app.html_block_index_to_id.len(), 1);
        let &id = app.html_block_index_to_id.get(&2).expect("index 2 mapped");

        app.apply_turn_event(TurnEvent::HtmlBlockDelta {
            index: 2,
            source: "<p>hello</p>".to_string(),
        });
        // After delta: source_preview has the fragment.
        if let Some(Entry::Canvas { source_preview, .. }) = app.last_entry() {
            assert_eq!(source_preview.as_deref(), Some("<p>hello</p>"));
        } else {
            panic!("expected Canvas entry");
        }

        app.apply_turn_event(TurnEvent::HtmlBlockStop { index: 2 });
        // After stop: preview is None, source is set, index removed from map.
        assert!(
            app.html_block_index_to_id.is_empty(),
            "index must be removed on stop"
        );
        if let Some(Entry::Canvas {
            id: entry_id,
            source,
            source_preview,
        }) = app.last_entry()
        {
            assert_eq!(*entry_id, id);
            assert_eq!(source, "<p>hello</p>");
            assert!(source_preview.is_none());
        } else {
            panic!("expected Canvas entry");
        }
    }

    /// `consume_model_override` returns the stored id and clears the field.
    #[test]
    fn consume_model_override_takes_the_value() {
        let mut app = fresh_app();
        assert!(
            app.next_turn_model_override.is_none(),
            "fresh App has no override"
        );

        app.next_turn_model_override = Some("claude-opus-5".to_string());
        let taken = app.consume_model_override();
        assert_eq!(
            taken.as_deref(),
            Some("claude-opus-5"),
            "should return the stored id"
        );
        assert!(
            app.next_turn_model_override.is_none(),
            "field must be None after consume"
        );

        // Second call is idempotent — no panic, returns None.
        assert!(app.consume_model_override().is_none());
    }

    /// Build a `TranscriptFile` with a single assistant message whose
    /// `content` is the given blocks.
    fn transcript_with_assistant_blocks(
        blocks: Vec<savvagent_protocol::ContentBlock>,
    ) -> TranscriptFile {
        use savvagent_protocol::{Message, Role};
        TranscriptFile {
            schema_version: 1,
            model: "test-model".into(),
            saved_at: 1_716_300_000,
            messages: vec![Message {
                role: Role::Assistant,
                content: blocks,
            }],
            subagent_transcripts: Default::default(),
        }
    }

    /// Phase 1 bug fix: `/resume` must recreate canvases from `Html`
    /// blocks (previously they fell into `_ => {}` and were dropped).
    /// Even a `state: None` canvas must reappear as an `Entry::Canvas`
    /// with a working renderer in the registry.
    #[test]
    fn replay_recreates_canvas_from_html_block() {
        use savvagent_protocol::ContentBlock;

        let source = "<!doctype html><body><p>hello</p></body>".to_string();
        let record = transcript_with_assistant_blocks(vec![ContentBlock::Html {
            source: source.clone(),
            state: None,
        }]);

        let mut app = fresh_app();
        app.replay_transcript(&record);

        // An Entry::Canvas with the source must exist.
        let canvas = app
            .entries
            .iter()
            .find_map(|e| match e {
                Entry::Canvas { id, source: s, .. } => Some((*id, s.clone())),
                _ => None,
            })
            .expect("replay must push an Entry::Canvas for an Html block");
        assert_eq!(canvas.1, source, "canvas source must round-trip");

        // The registry must hold a renderer keyed by the same id.
        assert!(
            app.canvas_registry.get_mut(canvas.0).is_some(),
            "registry must have a renderer for the replayed canvas id"
        );
    }

    /// Interactive state embedded in an `Html` block (base64 STANDARD of a
    /// `CanvasState`) must be restored onto the recreated renderer.
    #[test]
    fn replay_restores_canvas_state_from_html_block() {
        use base64::Engine as _;
        use savvagent_protocol::ContentBlock;

        // Deterministic state: a CanvasState with one open <details>.
        let mut state = savvagent_canvas::CanvasState {
            schema_version: 1,
            ..Default::default()
        };
        state.open_details.insert("88".into());
        let b64 = base64::engine::general_purpose::STANDARD.encode(state.to_bytes());

        let source = "<!doctype html><body><details><summary>s</summary><p>y</p></details></body>"
            .to_string();
        let record = transcript_with_assistant_blocks(vec![ContentBlock::Html {
            source,
            state: Some(b64),
        }]);

        let mut app = fresh_app();
        app.replay_transcript(&record);

        // Snapshot the recreated renderer; restored open_details must survive.
        let renderer = app
            .canvas_registry
            .get_mut(savvagent_plugin::ContentBlockId(0))
            .expect("renderer for id 0");
        let snap = renderer
            .snapshot_state()
            .expect("snapshot non-empty after restore");
        let restored = savvagent_canvas::CanvasState::from_bytes(&snap).unwrap();
        assert!(
            !restored.open_details.is_empty(),
            "open_details must be restored from the Html block's state"
        );
    }

    /// Two top-level `Html` blocks must get `ContentBlockId(0)` and `(1)`,
    /// proving the replayed ids align with the ordinals Task 26 saved
    /// each canvas's state blob under.
    #[test]
    fn replay_two_canvases_get_ids_zero_and_one_matching_save_ordinals() {
        use savvagent_protocol::ContentBlock;

        let record = transcript_with_assistant_blocks(vec![
            ContentBlock::Html {
                source: "<!doctype html><body><p>a</p></body>".into(),
                state: None,
            },
            ContentBlock::Html {
                source: "<!doctype html><body><p>b</p></body>".into(),
                state: None,
            },
        ]);

        let mut app = fresh_app();
        app.replay_transcript(&record);

        let ids: Vec<u32> = app
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::Canvas { id, .. } => Some(id.0),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![0, 1], "ids must be allocated in stream order");
        assert!(app.canvas_registry.get_mut(ContentBlockId(0)).is_some());
        assert!(app.canvas_registry.get_mut(ContentBlockId(1)).is_some());
    }
}
