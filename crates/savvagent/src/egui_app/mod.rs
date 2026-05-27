//! Native egui front-end (v0.19.0 migration, Plan 1). Built alongside the
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
pub mod view;

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
    /// and held for later GUI tasks (slash-command dispatch, `/connect`, tool
    /// registration), which need them exactly as `run_app` does. Unread in the
    /// foundation; the allow is scoped to these two reserved fields rather than
    /// the whole struct so any *other* dead field still surfaces as a warning.
    #[allow(dead_code)]
    project_root: PathBuf,
    #[allow(dead_code)]
    tool_bins: ToolBins,
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

    /// Spawn a streaming turn for `text`. A faithful port of `run_app`'s
    /// Enter-key turn-spawn path: push the user entry, set `is_loading`,
    /// consume any one-turn model override, then `spawn` the worker that runs
    /// `Host::run_turn_streaming` and forwards `TurnEvent`s back over the
    /// channel.
    ///
    /// Slash-command dispatch and the `PromptSubmitted` hook / pending-prompt
    /// machinery are intentionally out of scope for the foundation (they're
    /// owned by later GUI tasks); this is the plain "send a prompt to the
    /// active host" path.
    fn submit_prompt(&mut self, text: String) {
        if text.is_empty() || self.app.is_loading {
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
        // 1. Global input routing. Only Ctrl-C (quit) is handled globally in
        //    the foundation; the prompt `TextEdit` consumes its own keys. We
        //    convert through the same portable-key sink the plugin boundary
        //    uses so a later task can route arbitrary global accelerators
        //    through plugins without re-deriving modifiers.
        let events = ctx.input(|i| i.events.clone());
        for ev in &events {
            if let Some(k) = convert::egui_event_to_portable(ev) {
                use savvagent_plugin::KeyCodePortable;
                if k.modifiers.ctrl && matches!(k.code, KeyCodePortable::Char('c')) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        // 2. Drain the worker channel and rebuild the render-model snapshot in
        //    one async pass on the UI thread. `handle_worker_msg` borrows
        //    `&mut self`, while draining borrows `self.worker_rx`; we resolve
        //    the aliasing by first collecting messages into an owned `Vec`
        //    (borrows only `self.worker_rx`), then handling them (borrows the
        //    rest of `self`).
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

        // 3. Paint.
        view::paint(self, ctx);

        // 4. Keep repainting while a turn streams so newly-arrived deltas show
        //    up without requiring a user input event to wake the event loop.
        if self.app.is_loading {
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
