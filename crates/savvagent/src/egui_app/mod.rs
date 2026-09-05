//! Native egui front-end (v0.19.0 migration, Plans 1–2). Built alongside the
//! ratatui TUI and launched via the `savvagent gui` subcommand — see
//! `docs/superpowers/specs/2026-05-26-v0.19.0-egui-frontend-design.md`.
//!
//! Pinned eframe/egui: 0.32. Trait method confirmed: `fn update(&mut self,
//! ctx: &egui::Context, frame: &mut eframe::Frame)`. eframe default features
//! (glow + x11 + wayland + default_fonts) are kept so the window opens on
//! Linux; submodules are added by later foundation tasks.
//!
//! This front-end reuses the exact host/turn machinery the ratatui TUI drives
//! in `run_app`: the same `WorkerMsg` channel, the same turn-id counters, and
//! the same pub(crate) leaf helpers (`translate_turn_event_to_host_event`,
//! `create_canvas_renderer`, `auto_export_canvas`, `save_transcript_now`,
//! `current_host`). Bootstrap is split for the GUI — `bootstrap_host_only`
//! runs the network half off the UI thread and `build_app_with_host` builds
//! the `App` on it — whereas the TUI calls `bootstrap_app_and_host` straight
//! through. The other difference is the shell: egui paints synchronously each
//! frame instead of ratatui's blocking `run_app` event loop.
//!
//! ## Async inside a synchronous paint pass
//!
//! `eframe::App::update` is synchronous, and `main()` is `#[tokio::main]`, so
//! the paint pass runs *inside* a Tokio runtime. We drive the async
//! worker-drain / render-model rebuild on the UI thread via [`block_on_ui`],
//! which wraps `futures::executor::block_on` (NOT `Handle::block_on`, which
//! panics inside a runtime) and — critically — `tokio::task::unconstrained`,
//! so the polled futures opt out of Tokio's cooperative-scheduling budget.
//! See [`block_on_ui`] for why that wrapper is mandatory. The Tokio context is
//! still entered, so `tokio::spawn` inside those futures works — and the turn
//! workers spawned by `submit_prompt` use the stored `Handle`.

pub mod convert;
pub mod fonts;
pub mod render_model;
pub mod screen;
pub mod view;
pub mod widgets;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::runtime::Handle;
use tokio::sync::mpsc;

use crate::app::{App, Entry};
use crate::{
    HostSlot, ToolBins, WorkerMsg, auto_export_canvas, create_canvas_renderer, current_host,
    save_transcript_now, translate_turn_event_to_host_event,
};
use render_model::{RenderModel, RenderModelCache, build_model};

/// What `build_app_with_host` returns — the seed for a [`SavvagentApp`]. The
/// network half (`bootstrap_host_only`) runs off the UI thread so a hanging
/// provider probe (e.g. Ollama on a dropped `localhost:11434`) can't freeze
/// the window; this `App` half is then built on the UI thread.
type BootOutput = (App, HostSlot, std::path::PathBuf, ToolBins);

/// Logical monospace columns the slot/render-model builder lays out against.
/// The ratatui path sizes this to the live terminal width; the egui paint pass
/// has no fixed column grid, so the foundation uses a constant. A later
/// fidelity task can derive this from the central-panel width and the
/// monospace glyph advance once a font is bundled (Task 11).
const RENDER_COLS: u16 = 120;

/// Font size (px) used for every styled-text section in the GUI log/footer.
/// Matches the value the `convert` unit tests exercise.
const FONT_SIZE: f32 = 14.0;

/// Drive a future to completion on the UI (winit) thread.
///
/// The egui paint pass is synchronous but needs to run async host/plugin
/// code, so it drives futures with the foreign `futures` executor rather than
/// `Handle::block_on` (which panics inside a runtime). The crucial detail is
/// [`tokio::task::unconstrained`]: it opts the future out of Tokio's
/// cooperative-scheduling budget.
///
/// Without it, a Tokio future polled by `futures::executor::block_on` can wedge
/// the entire window. Tokio's `coop` budget is per-*task* and refreshed by the
/// Tokio scheduler at the start of each poll. The `futures` executor is not a
/// Tokio task, so once a poll exhausts the budget, `tokio::task::coop::poll_proceed`
/// returns `Pending` and there is no scheduler to refresh it and re-poll — the
/// future (e.g. a perfectly *uncontended* `RwLock::read`/`Mutex::lock`) never
/// completes, and `block_on` never returns control to the winit event loop.
/// That presented as the GUI freezing (no input, no resize) on a lock acquire
/// with no actual lock holder. `unconstrained` makes `poll_proceed` always
/// proceed, so these futures complete normally.
pub(crate) fn block_on_ui<F: std::future::Future>(fut: F) -> F::Output {
    futures::executor::block_on(tokio::task::unconstrained(fut))
}

/// The eframe application. Mirrors the state `run_app` keeps on its stack —
/// the host slot, the worker channel, the four HostEvent turn-id counters —
/// plus the egui-only bits (render-model cache, prompt buffer).
pub struct SavvagentApp {
    /// Shared conversation/UI state, built once by `bootstrap_app_and_host`.
    pub app: App,
    /// Atomically-swappable active host (shared with future `/connect`).
    pub(crate) host_slot: HostSlot,
    /// Worker → UI messages, drained each frame in `update`.
    worker_rx: mpsc::Receiver<WorkerMsg>,
    /// Cloned into each spawned turn worker so it can report back.
    worker_tx: mpsc::Sender<WorkerMsg>,
    /// Tokio runtime handle captured from `main`'s `#[tokio::main]` runtime;
    /// turn workers are spawned onto it.
    pub(crate) rt: Handle,
    /// Latest slot snapshot (banner/tips/footer/tool_entries) rebuilt each
    /// frame off the live `App`.
    render_cache: RenderModelCache,

    // HostEvent emission state — identical semantics to `run_app`'s stack
    // locals. See `translate_turn_event_to_host_event` for the contract.
    next_turn_id: u32,
    current_turn_id: Option<u32>,
    next_tool_call_id: u64,
    last_tool_call_id: Option<u64>,

    /// Current prompt-editor buffer (bound to the bottom `TextEdit`).
    pub prompt: String,

    /// Project root + tool binaries — captured from `bootstrap_app_and_host`
    /// and threaded into the pending-action drain (`apply_pending_model_change`)
    /// and slash-command dispatch, exactly as `run_app` does.
    project_root: PathBuf,
    tool_bins: ToolBins,

    /// `Ctrl+O` file picker. The dialog is always allocated; `open()`
    /// puts it in pick-file mode and `update()` paints it each frame.
    pub file_picker: widgets::file_picker::FilePicker,

    /// Per-canvas egui `TextureHandle` cache. Renderer ownership stays on
    /// `App::canvas_registry`; this cache only stores the GPU-side handles.
    pub gui_canvas_cache: widgets::canvas::GuiCanvasCache,

    /// Set by `view::paint_prompt` when the user types `/` into an empty
    /// prompt; consumed at the top of the next frame's async pass to open the
    /// `palette` command picker. Mirrors the TUI's `/`-opens-palette
    /// keybinding, which the egui shell doesn't route through the plugin
    /// `KeybindingRouter`.
    pub(crate) pending_open_palette: bool,
}

impl SavvagentApp {
    /// Build the running front-end from a completed `build_app_with_host`
    /// result. Synchronous and non-blocking: the network half of bootstrap
    /// (provider health probes / `list_models`, via `bootstrap_host_only`) has
    /// already happened off the UI thread inside [`GuiApp`], so this only wires
    /// up the worker channel and the empty render cache. The UI thread never
    /// blocks here.
    fn from_boot(boot: BootOutput, rt: Handle) -> Self {
        let (app, host_slot, project_root, tool_bins) = boot;
        let (worker_tx, worker_rx) = mpsc::channel::<WorkerMsg>(128);
        let render_cache: RenderModelCache = Arc::new(Mutex::new(RenderModel::default()));
        Self {
            app,
            host_slot,
            worker_rx,
            worker_tx,
            rt,
            render_cache,
            next_turn_id: 0,
            current_turn_id: None,
            next_tool_call_id: 0,
            last_tool_call_id: None,
            prompt: String::new(),
            project_root,
            tool_bins,
            file_picker: widgets::file_picker::FilePicker::default(),
            gui_canvas_cache: widgets::canvas::GuiCanvasCache::new(),
            pending_open_palette: false,
        }
    }

    /// Drop the GUI texture cache and the shared renderer state in
    /// `App::canvas_registry`. Call alongside any operation that resets
    /// the conversation (today only `App::replay_transcript`).
    #[allow(dead_code)] // Hook for upcoming GUI conversation-reset paths.
    pub(crate) fn clear_canvas_caches(&mut self) {
        self.app.canvas_registry.clear();
        self.gui_canvas_cache.clear();
    }

    /// Read-only access to the cached render model for the paint pass.
    pub(crate) fn render_cache(&self) -> &RenderModelCache {
        &self.render_cache
    }

    /// Handle one worker message. A faithful port of the six `WorkerMsg` arms
    /// in `run_app`'s `while let Ok(msg) = worker_rx.try_recv()` block —
    /// same order, same leaf-helper calls, same plugin dispatch. The only
    /// omission is `update_metrics`-adjacent terminal-specific state (none
    /// exists here); everything that mutates `App` or fires a `HostEvent` is
    /// replicated.
    async fn handle_worker_msg(&mut self, msg: WorkerMsg) {
        match msg {
            WorkerMsg::Event(e) => {
                use savvagent_host::TurnEvent;
                let was_complete = matches!(e, TurnEvent::TurnComplete { .. });
                // Capture the canvas id before apply_turn_event consumes the
                // event and clears the index from html_block_index_to_id.
                let html_block_stop_id = if let TurnEvent::HtmlBlockStop { index } = &e {
                    self.app.html_block_index_to_id.get(index).copied()
                } else {
                    None
                };
                let host_event = translate_turn_event_to_host_event(
                    &e,
                    &mut self.next_turn_id,
                    &mut self.current_turn_id,
                    &mut self.next_tool_call_id,
                    &mut self.last_tool_call_id,
                );
                self.app.apply_turn_event(e);
                self.app.update_metrics();
                if let Some(canvas_id) = html_block_stop_id {
                    create_canvas_renderer(&mut self.app, canvas_id).await;
                    auto_export_canvas(
                        &self.app,
                        canvas_id,
                        self.current_turn_id.unwrap_or(self.next_turn_id),
                    );
                }
                if let Some(he) = host_event {
                    if let Err(err) =
                        crate::plugin::effects::dispatch_host_event(&mut self.app, he, 0).await
                    {
                        tracing::warn!(error = %err, "host-event dispatch (from TurnEvent) failed");
                    }
                }
                if was_complete {
                    if let Some(host) = current_host(&self.host_slot).await {
                        match save_transcript_now(&self.app, &host).await {
                            Ok(path) => {
                                if !path.as_os_str().is_empty() {
                                    let saved_path = path.to_string_lossy().into_owned();
                                    self.app.last_transcript = Some(path);
                                    if let Err(err) = crate::plugin::effects::dispatch_host_event(
                                        &mut self.app,
                                        savvagent_plugin::HostEvent::TranscriptSaved {
                                            path: saved_path,
                                        },
                                        0,
                                    )
                                    .await
                                    {
                                        tracing::warn!(error = %err, "TranscriptSaved dispatch failed");
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "save_transcript_now failed");
                                self.app.push_note(format!("Couldn't save transcript: {e}"));
                            }
                        }
                    }
                }
            }
            WorkerMsg::Error(msg) => {
                self.app.is_loading = false;
                self.app.entries.push(Entry::Note(format!("Error: {msg}")));
                self.app.update_metrics();
                // A runner error terminates the turn without a TurnComplete;
                // emit TurnEnd { success: false } so subscribers see symmetry
                // with successful turns. Synthesize a TurnStart first when the
                // provider errored before `IterationStarted { iteration: 1 }`.
                let turn_id = match self.current_turn_id.take() {
                    Some(id) => id,
                    None => {
                        self.next_turn_id = self.next_turn_id.saturating_add(1);
                        let synthetic = self.next_turn_id;
                        if let Err(err) = crate::plugin::effects::dispatch_host_event(
                            &mut self.app,
                            savvagent_plugin::HostEvent::TurnStart { turn_id: synthetic },
                            0,
                        )
                        .await
                        {
                            tracing::warn!(error = %err, "synthetic TurnStart dispatch failed");
                        }
                        synthetic
                    }
                };
                self.last_tool_call_id = None;
                if let Err(err) = crate::plugin::effects::dispatch_host_event(
                    &mut self.app,
                    savvagent_plugin::HostEvent::TurnEnd {
                        turn_id,
                        success: false,
                    },
                    0,
                )
                .await
                {
                    tracing::warn!(error = %err, "TurnEnd(failure) dispatch failed");
                }
            }
            WorkerMsg::BashDone => {
                self.app.is_loading = false;
                self.app.update_metrics();
            }
            WorkerMsg::DisconnectCompleted { provider, mode } => {
                self.app.push_note(
                    rust_i18n::t!("notes.disconnect-completed", name = provider, mode = mode)
                        .to_string(),
                );
            }
            WorkerMsg::DisconnectFailed { provider, err } => {
                self.app.push_note(
                    rust_i18n::t!("notes.disconnect-worker-failed", name = provider, err = err)
                        .to_string(),
                );
            }
            WorkerMsg::ModelRestored(original) => {
                self.app.model = original;
            }
        }
    }

    /// Drain the same pending-action queues `run_app` drains after a screen
    /// key, in the same order, so picker selections (model/provider/routing/
    /// etc.) take effect. Each `apply_pending_*` is a `pub(crate)` helper in
    /// `main.rs`.
    async fn drain_pending(&mut self) {
        crate::apply_pending_model_change(
            &mut self.app,
            &self.host_slot,
            &self.project_root,
            &self.tool_bins,
        )
        .await;
        crate::apply_pending_pool_add(&mut self.app, &self.host_slot).await;
        crate::apply_pending_gate(&mut self.app, &self.host_slot).await;
        crate::apply_pending_in_process_tools(&mut self.app, &self.host_slot).await;
        crate::apply_pending_routing_reload(&mut self.app, &self.host_slot).await;
        crate::apply_pending_routing_show(&mut self.app, &self.host_slot).await;
    }

    /// Spawn a streaming turn for `text`. A faithful port of `run_app`'s
    /// Enter-key turn-spawn path: push the user entry, set `is_loading`,
    /// consume any one-turn model override, then `spawn` the worker that runs
    /// `Host::run_turn_streaming` and forwards `TurnEvent`s back over the
    /// channel.
    ///
    /// `/`-prefixed input is routed to the shared `dispatch_slash_command`
    /// instead (opening pickers / running commands); the `PromptSubmitted` hook
    /// and pending-prompt-prefix machinery remain out of scope for now.
    fn submit_prompt(&mut self, text: String) {
        if text.is_empty() || self.app.is_loading {
            return;
        }
        // Slash command -> dispatch (opens pickers, runs commands) instead of a
        // turn. Mirrors the ratatui submit path: record the input in history,
        // run the shared `dispatch_slash_command` (which itself calls
        // `apply_effects` and may push a screen / queue pending actions), then
        // drain the pending queues to apply anything it staged.
        if text.starts_with('/') {
            self.app.prompt_history.append(text.clone());
            let tx = self.worker_tx.clone();
            block_on_ui(crate::dispatch_slash_command(
                &mut self.app,
                &text,
                &self.host_slot,
                &self.project_root,
                &self.tool_bins,
                &tx,
            ));
            block_on_ui(self.drain_pending());
            return;
        }
        let Some(host) = block_on_ui(current_host(&self.host_slot)) else {
            self.app
                .push_note(rust_i18n::t!("notes.not-connected-connect-first").to_string());
            return;
        };
        self.app.log_scroll_offset_from_bottom = None;
        self.app.prompt_history.append(text.clone());
        self.app.push_user(text.clone());
        self.app.is_loading = true;

        let model_override = self.app.consume_model_override();
        let original_model = self.app.model.clone();
        let prompt_text = text;

        let tx = self.worker_tx.clone();
        self.rt.spawn(async move {
            if let Some(ref override_id) = model_override {
                tracing::debug!(model = %override_id, "applying one-turn model override");
                host.set_model(override_id.clone()).await;
            }

            let (ev_tx, mut ev_rx) = mpsc::channel(64);
            let host_for_run = host.clone();
            let prompt = prompt_text;
            let runner =
                tokio::spawn(async move { host_for_run.run_turn_streaming(prompt, ev_tx).await });
            while let Some(ev) = ev_rx.recv().await {
                if tx.send(WorkerMsg::Event(ev)).await.is_err() {
                    break;
                }
            }
            let result = runner.await;

            if model_override.is_some() {
                tracing::debug!(model = %original_model, "restoring model after one-turn override");
                host.set_model(original_model.clone()).await;
                let _ = tx.send(WorkerMsg::ModelRestored(original_model)).await;
            }

            match result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    let _ = tx.send(WorkerMsg::Error(e.to_string())).await;
                }
                Err(join_err) => {
                    let _ = tx
                        .send(WorkerMsg::Error(format!("worker task failed: {join_err}")))
                        .await;
                }
            }
        });
    }
}

impl SavvagentApp {
    /// One paint pass for the *running* (post-bootstrap) front-end. Driven by
    /// [`GuiApp::update`] once the host half (`bootstrap_host_only`) has
    /// completed off the UI thread and `build_app_with_host` has built the
    /// `App` on it. Formerly the `eframe::App::update` body; the eframe trait
    /// now lives on [`GuiApp`] so the window can open and stay responsive while
    /// the (network-touching) host build runs in the background.
    fn frame(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 0. Drain any prompt prefill staged by `apply_effects` last frame
        //    (the command palette emits `Effect::PrefillInput { "/cmd " }` for
        //    arg-requiring slashes). `prefill_input` writes the ratatui
        //    textarea AND this bridge field; the egui prompt lives on
        //    `self.prompt`, so we move it across here.
        if let Some(text) = self.app.take_pending_prefill() {
            self.prompt = text;
        }

        // 1. Global quit chord. Observes Ctrl-C / Ctrl-D and requests a viewport
        //    close — the events themselves still propagate to block 3 (when a
        //    screen is open) or to the prompt `TextEdit`, but the close request
        //    races them and shuts the window. The GUI deliberately does NOT
        //    route home keybindings through the plugin `KeybindingRouter` —
        //    pickers are opened via slash commands (see `submit_prompt`); only
        //    the ratatui TUI drives plugin-bound home accelerators.
        let events = ctx.input(|i| i.events.clone());
        for ev in &events {
            if let Some(k) = convert::egui_event_to_portable(ev) {
                use savvagent_plugin::KeyCodePortable as KC;
                let quit = k.modifiers.ctrl && matches!(k.code, KC::Char('c') | KC::Char('d'));
                if quit {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                // Skip global Ctrl-O while a canvas holds focus — the
                // focused-canvas handler owns Ctrl-O (open-in-browser),
                // and the two would otherwise both fire.
                let open_picker = k.modifiers.ctrl
                    && matches!(k.code, KC::Char('o'))
                    && !matches!(self.app.input_mode, crate::app::InputMode::Canvas { .. });
                if open_picker {
                    self.file_picker.open();
                }
            }
        }

        // 2. Drain the worker channel and rebuild the render-model snapshot in
        //    one async pass. Runs every frame, screen open or not, so an
        //    in-flight streaming turn can never wedge on the 128-slot channel
        //    back-pressuring while a modal owns input.
        //
        //    `handle_worker_msg` borrows `&mut self`, while draining borrows
        //    `self.worker_rx`; we resolve the aliasing by first collecting
        //    messages into an owned `Vec` (borrows only `self.worker_rx`), then
        //    handling them (borrows the rest of `self`).
        block_on_ui(async {
            // `/` on an empty prompt opens the command palette (set by
            // `view::paint_prompt`). Reuses the `palette` screen, which
            // `apply_effects::open_screen` self-populates from the slash index.
            if std::mem::take(&mut self.pending_open_palette) {
                let effs = vec![savvagent_plugin::Effect::OpenScreen {
                    id: "palette".into(),
                    args: savvagent_plugin::ScreenArgs::None,
                }];
                if let Err(err) = crate::plugin::effects::apply_effects(&mut self.app, effs).await {
                    tracing::warn!(error = %err, "apply_effects (open palette) failed");
                }
                self.drain_pending().await;
            }
            let mut drained = Vec::new();
            while let Ok(msg) = self.worker_rx.try_recv() {
                drained.push(msg);
            }
            for msg in drained {
                self.handle_worker_msg(msg).await;
            }
            let model = build_model(&self.app, RENDER_COLS).await;
            *self.render_cache.lock().unwrap() = model;
        });

        // 3. If a screen is open, route input to it and skip home handling —
        //    mirrors `run_app`'s precedence (quit → top screen `on_key` →
        //    home). Effects (push/pop/close) and the pending-action queues are
        //    applied per key, in the same order the TUI uses. The render model
        //    is only rebuilt again when we actually routed keys; block 2's
        //    rebuild is otherwise still current.
        if !self.app.screen_stack.is_empty() {
            let keys = screen::portable_keys_from_events(&events);
            let had_keys = !keys.is_empty();
            block_on_ui(async {
                for key in keys {
                    let effs = match self.app.screen_stack.top_mut() {
                        Some((top, _layout)) => match top.on_key(key).await {
                            Ok(e) => e,
                            Err(err) => {
                                tracing::warn!(error = %err, "screen on_key failed");
                                continue;
                            }
                        },
                        None => break, // a prior key's effect closed the last screen
                    };
                    if let Err(err) =
                        crate::plugin::effects::apply_effects(&mut self.app, effs).await
                    {
                        tracing::warn!(error = %err, "apply_effects (screen) failed");
                    }
                    self.drain_pending().await;
                }
                if had_keys {
                    let model = build_model(&self.app, RENDER_COLS).await;
                    *self.render_cache.lock().unwrap() = model;
                }
            });
            view::paint(self, ctx);
            ctx.request_repaint(); // screens are interactive; keep ticking
            return;
        }

        // 4. Paint.
        view::paint(self, ctx);

        // 5. Keep repainting while a turn streams so newly-arrived deltas show
        //    up without requiring a user input event to wake the event loop.
        //    Also repaint if a slash command just opened a screen (via
        //    `submit_prompt` during paint), so the screen-routing block (3)
        //    takes over next frame and the overlay stays interactive without
        //    needing another input event.
        if self.app.is_loading || !self.app.screen_stack.is_empty() {
            ctx.request_repaint();
        }
    }
}

/// The eframe application. A thin lifecycle wrapper around [`SavvagentApp`]
/// whose sole job is to keep the window responsive while the network-touching
/// host build (`bootstrap_host_only`) runs **off the UI thread** (the `App`
/// half, `build_app_with_host`, is then built on the UI thread).
///
/// The previous design `block_on`'d bootstrap inside the eframe creation
/// closure, so a hanging provider probe (Ollama on a dropped
/// `localhost:11434`) wedged the UI thread before the first frame — the window
/// couldn't be moved, resized, or typed into. Here the closure returns
/// immediately; bootstrap runs on a Tokio worker and reports back over a
/// `oneshot`, and `update` paints a splash until it lands.
enum Boot {
    /// The network host-build is running on a Tokio worker (it returns only
    /// `Send` data — no `App`). `project_root`/`tool_bins` are kept on the UI
    /// thread so it can build the `!Send` `App` once the host arrives.
    Pending {
        rx: tokio::sync::oneshot::Receiver<Option<crate::HostBoot>>,
        project_root: std::path::PathBuf,
        tool_bins: ToolBins,
    },
    /// Bootstrap finished; normal painting takes over.
    Running(Box<SavvagentApp>),
    /// Bootstrap failed; the UI paints the error instead of crashing.
    Failed(String),
}

struct GuiApp {
    rt: Handle,
    boot: Boot,
}

impl GuiApp {
    /// Install fonts and kick off the **network** half of bootstrap on a Tokio
    /// worker. `App` is `!Send` (it holds a non-`Send` tree-sitter parser via
    /// the ratatui editor fields), so it cannot be built off-thread — only the
    /// host build, which returns `Send` data, runs on the worker. Returns
    /// immediately with [`Boot::Pending`] so the window opens at once.
    fn new(cc: &eframe::CreationContext<'_>, rt: Handle) -> Self {
        fonts::install(&cc.egui_ctx);
        // Local + fast (path resolution, config-file read): fine on the UI
        // thread.
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let tool_bins = crate::build_tool_bins();
        let config_file = crate::config_file::ConfigFile::load_or_default(
            &crate::config_file::ConfigFile::default_path(),
        );

        let (tx, rx) = tokio::sync::oneshot::channel();
        let ctx = cc.egui_ctx.clone();
        let pr = project_root.clone();
        let tb = tool_bins.clone();
        rt.spawn(async move {
            let host = crate::bootstrap_host_only(pr, tb, config_file).await;
            // Ignore send errors: the only receiver is `Boot::Pending`, which
            // outlives the window unless the app already shut down.
            let _ = tx.send(host);
            // Wake the UI loop so the next frame builds + swaps in the running
            // app instead of waiting for an unrelated input event.
            ctx.request_repaint();
        });
        Self {
            rt,
            boot: Boot::Pending {
                rx,
                project_root,
                tool_bins,
            },
        }
    }
}

impl GuiApp {
    /// Advance the boot state machine and hand back the running front-end once
    /// bootstrap has settled. While the host build is still in flight — or if
    /// it failed — this paints the splash and returns `None`, so the caller
    /// only ever touches a live `SavvagentApp`.
    ///
    /// The "`Boot::Pending` never escapes this function" invariant is expressed
    /// in control flow rather than a runtime `unreachable!`: the empty-channel
    /// case returns early, and every other `Pending` outcome transitions
    /// `self.boot` to `Running`/`Failed` before the final match. The residual
    /// `Pending` arm therefore needs no panic — it simply paints nothing and
    /// resolves on the next frame.
    fn poll_boot(&mut self, ctx: &egui::Context) -> Option<&mut SavvagentApp> {
        // While pending, poll the oneshot without blocking and keep repainting
        // so we notice completion.
        if let Boot::Pending {
            rx,
            project_root,
            tool_bins,
        } = &mut self.boot
        {
            use tokio::sync::oneshot::error::TryRecvError;
            match rx.try_recv() {
                Ok(host) => {
                    // Build the `!Send` `App` here on the UI thread. This is
                    // local-only work (manifests, plugin instantiation) — no
                    // network — so the brief `block_on` cannot freeze the
                    // window the way the old all-in-one bootstrap did.
                    let built = block_on_ui(crate::build_app_with_host(
                        host,
                        project_root.clone(),
                        tool_bins.clone(),
                    ));
                    self.boot = match built {
                        Ok(boot) => {
                            Boot::Running(Box::new(SavvagentApp::from_boot(boot, self.rt.clone())))
                        }
                        Err(e) => Boot::Failed(format!("{e:#}")),
                    };
                }
                Err(TryRecvError::Closed) => {
                    self.boot = Boot::Failed("bootstrap task was dropped".to_string())
                }
                Err(TryRecvError::Empty) => {
                    paint_boot_splash(ctx, None);
                    ctx.request_repaint(); // keep polling the oneshot
                    return None;
                }
            }
        }

        match &mut self.boot {
            Boot::Running(app) => Some(app),
            Boot::Failed(msg) => {
                paint_boot_splash(ctx, Some(msg));
                None
            }
            // Settled to Running/Failed by the block above (or returned early
            // while empty); a stray Pending just paints nothing this frame.
            Boot::Pending { .. } => None,
        }
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(app) = self.poll_boot(ctx) {
            app.frame(ctx, frame);
        }
    }
}

/// Centered "starting…" (or error) screen shown until bootstrap completes.
/// Deliberately trivial: no `App`, no plugins, no host — it must be paintable
/// before any of that exists.
fn paint_boot_splash(ctx: &egui::Context, error: Option<&str>) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.4);
            match error {
                None => {
                    ui.heading("savvagent");
                    ui.add_space(8.0);
                    ui.add(egui::Spinner::new());
                    ui.add_space(8.0);
                    ui.weak("Starting…");
                }
                Some(msg) => {
                    ui.heading("savvagent — startup failed");
                    ui.add_space(8.0);
                    ui.colored_label(egui::Color32::LIGHT_RED, msg);
                }
            }
        });
    });
}

/// Launch the native window. Called from `main()` on the `gui` subcommand.
/// `main()` is `#[tokio::main]`, so a runtime is already running on this
/// thread; we capture its `Handle` and hand it to [`GuiApp`] for spawning the
/// bootstrap task and (later) turn workers. `eframe::run_native` then owns the
/// thread for the window's lifetime.
pub fn run() -> eframe::Result {
    let rt = Handle::current();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 800.0])
            .with_title("savvagent"),
        ..Default::default()
    };
    eframe::run_native(
        "savvagent",
        native_options,
        Box::new(move |cc| Ok(Box::new(GuiApp::new(cc, rt)))),
    )
}
