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
//! in `run_app`: the same `bootstrap_app_and_host`, the same `WorkerMsg`
//! channel, the same turn-id counters, and the same pub(crate) leaf helpers
//! (`translate_turn_event_to_host_event`, `create_canvas_renderer`,
//! `auto_export_canvas`, `save_transcript_now`, `current_host`). The only
//! difference is the shell: egui paints synchronously each frame instead of
//! ratatui's blocking `run_app` event loop.
//!
//! ## Async inside a synchronous paint pass
//!
//! `eframe::App::update` is synchronous, and `main()` is `#[tokio::main]`, so
//! the paint pass runs *inside* a Tokio runtime. We therefore use
//! `futures::executor::block_on` (NOT `Handle::block_on`, which panics with
//! "Cannot block the current thread from within a runtime") to drive the async
//! worker-drain / render-model rebuild on the UI thread. The Tokio context is
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
    HostSlot, ToolBins, WorkerMsg, auto_export_canvas, bootstrap_app_and_host,
    create_canvas_renderer, current_host, save_transcript_now, translate_turn_event_to_host_event,
};
use render_model::{RenderModel, RenderModelCache, build_model};

/// Logical monospace columns the slot/render-model builder lays out against.
/// The ratatui path sizes this to the live terminal width; the egui paint pass
/// has no fixed column grid, so the foundation uses a constant. A later
/// fidelity task can derive this from the central-panel width and the
/// monospace glyph advance once a font is bundled (Task 11).
const RENDER_COLS: u16 = 120;

/// Font size (px) used for every styled-text section in the GUI log/footer.
/// Matches the value the `convert` unit tests exercise.
const FONT_SIZE: f32 = 14.0;

/// The eframe application. Mirrors the state `run_app` keeps on its stack —
/// the host slot, the worker channel, the four HostEvent turn-id counters —
/// plus the egui-only bits (render-model cache, prompt buffer).
pub struct SavvagentApp {
    /// Shared conversation/UI state, built once by `bootstrap_app_and_host`.
    pub app: App,
    /// Atomically-swappable active host (shared with future `/connect`).
    host_slot: HostSlot,
    /// Worker → UI messages, drained each frame in `update`.
    worker_rx: mpsc::Receiver<WorkerMsg>,
    /// Cloned into each spawned turn worker so it can report back.
    worker_tx: mpsc::Sender<WorkerMsg>,
    /// Tokio runtime handle captured from `main`'s `#[tokio::main]` runtime;
    /// turn workers are spawned onto it.
    rt: Handle,
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

    /// Per-open file state for the GUI editor (view-file/edit-file).
    /// `None` when no marker screen is open. Loaded lazy by
    /// `widgets::editor::ensure_buffer_for_active_screen` on the first
    /// frame after the screen pushes; cleared by the same helper when
    /// the stack no longer contains a marker screen.
    pub editor_buffer: Option<widgets::editor::EditorBuffer>,

    /// `Ctrl+O` file picker. The dialog is always allocated; `open()`
    /// puts it in pick-file mode and `update()` paints it each frame.
    pub file_picker: widgets::file_picker::FilePicker,
}

impl SavvagentApp {
    /// Build the app: bootstrap the host/plugin runtime synchronously (we are
    /// inside the Tokio runtime, so `futures::executor::block_on` drives the
    /// async bootstrap without `Handle::block_on`'s reentrancy panic), open the
    /// worker channel, and initialize an empty render cache.
    fn new(cc: &eframe::CreationContext<'_>, rt: Handle) -> anyhow::Result<Self> {
        fonts::install(&cc.egui_ctx);
        let (worker_tx, worker_rx) = mpsc::channel::<WorkerMsg>(128);
        let (app, host_slot, project_root, tool_bins) =
            futures::executor::block_on(bootstrap_app_and_host())?;
        let render_cache: RenderModelCache = Arc::new(Mutex::new(RenderModel::default()));
        Ok(Self {
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
            editor_buffer: None,
            file_picker: widgets::file_picker::FilePicker::default(),
        })
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
                        if let Ok(path) = save_transcript_now(&self.app, &host).await {
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

    /// Save the GUI editor buffer to disk, push a status note, and clear
    /// the dirty flag. Called from `update()` when `Ctrl-S` is observed
    /// while the `edit-file` marker screen is on top. Bypasses
    /// `Effect::SaveActiveFile` (which writes the ratatui editor, not
    /// the GUI buffer).
    fn save_editor_buffer(&mut self) {
        let Some(buf) = self.editor_buffer.as_mut() else {
            return;
        };
        let path_display = buf.path.display().to_string();
        match buf.save_to_disk() {
            Ok(()) => self
                .app
                .push_note(rust_i18n::t!("notes.file-saved", path = path_display).to_string()),
            Err(e) => self.app.push_note(
                rust_i18n::t!("notes.file-write-error", err = format!("{e:#}")).to_string(),
            ),
        }
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
            futures::executor::block_on(crate::dispatch_slash_command(
                &mut self.app,
                &text,
                &self.host_slot,
                &self.project_root,
                &self.tool_bins,
                &tx,
            ));
            futures::executor::block_on(self.drain_pending());
            return;
        }
        let Some(host) = futures::executor::block_on(current_host(&self.host_slot)) else {
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

impl eframe::App for SavvagentApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
                let open_picker = k.modifiers.ctrl && matches!(k.code, KC::Char('o'));
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
        futures::executor::block_on(async {
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

        // Plan 3: keep the GUI editor buffer in sync with the active
        // marker screen. Loads lazy from `App::active_file_path` on the
        // first frame after `view-file`/`edit-file` opens and drops when
        // the screen pops.
        widgets::editor::ensure_buffer_for_active_screen(&mut self.editor_buffer, &self.app);

        // 3. If a screen is open, route input to it and skip home handling —
        //    mirrors `run_app`'s precedence (quit → top screen `on_key` →
        //    home). Effects (push/pop/close) and the pending-action queues are
        //    applied per key, in the same order the TUI uses. The render model
        //    is only rebuilt again when we actually routed keys; block 2's
        //    rebuild is otherwise still current.
        if !self.app.screen_stack.is_empty() {
            // Plan 3: when the edit-file marker screen is on top, Ctrl-S
            // saves the GUI editor buffer directly and consumes the
            // key — the screen's on_key would otherwise emit
            // Effect::SaveActiveFile which writes the stale
            // ratatui-side App::editor.
            let edit_file_open = self
                .app
                .screen_stack
                .top()
                .map(|(s, _)| s.id() == "edit-file")
                .unwrap_or(false);
            let mut all_keys = screen::portable_keys_from_events(&events);
            if edit_file_open {
                let mut ctrl_s_count = 0usize;
                let mut esc_pending = false;
                all_keys.retain(|k| {
                    let is_ctrl_s = k.modifiers.ctrl
                        && matches!(k.code, savvagent_plugin::KeyCodePortable::Char('s'));
                    if is_ctrl_s {
                        ctrl_s_count += 1;
                    }
                    // Esc: observe but DO NOT consume — the screen still
                    // needs to emit Effect::CloseScreen to pop itself.
                    // We just pre-save here so that the apply_effects
                    // CloseScreen arm's `app.save_file()` call
                    // (effects.rs:61-62) becomes a no-op (path is None)
                    // and the user's GUI edits land on disk instead of
                    // being silently overwritten by the stale
                    // App::editor (ratatui) buffer.
                    let is_esc = matches!(k.code, savvagent_plugin::KeyCodePortable::Esc);
                    if is_esc {
                        esc_pending = true;
                    }
                    !is_ctrl_s
                });
                for _ in 0..ctrl_s_count {
                    self.save_editor_buffer();
                }
                if esc_pending {
                    // Save the GUI buffer (idempotent if not dirty; the
                    // legacy TUI semantic is "save always on Esc-close
                    // of edit-file"). Then tear down the ratatui-side
                    // editor + path so the upcoming Effect::CloseScreen's
                    // `app.save_file()` short-circuits.
                    self.save_editor_buffer();
                    self.app.clear_active_editor();
                }
            }
            let keys = all_keys;
            let had_keys = !keys.is_empty();
            futures::executor::block_on(async {
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

/// Launch the native window. Called from `main()` on the `gui` subcommand.
/// `main()` is `#[tokio::main]`, so a runtime is already running on this
/// thread; we capture its `Handle` and hand it to `SavvagentApp` for spawning
/// turn workers. `eframe::run_native` then owns the thread for the window's
/// lifetime.
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
        Box::new(move |cc| {
            let app = SavvagentApp::new(cc, rt).map_err(
                |e| -> Box<dyn std::error::Error + Send + Sync> {
                    Box::new(std::io::Error::other(e.to_string()))
                },
            )?;
            Ok(Box::new(app))
        }),
    )
}
