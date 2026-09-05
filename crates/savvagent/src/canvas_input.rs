//! Shell-agnostic key + mouse dispatch for focused canvases.
//!
//! Both the ratatui TUI (`main.rs`) and the egui GUI
//! (`egui_app/widgets/canvas.rs`) translate their native events to
//! `KeyEventPortable` / `MouseEventPortable` and call the helpers here.
//! The bodies are the same logic the TUI used to keep inline; the move
//! lets the GUI reuse them without duplicating built-in shortcuts
//! (Esc / Tab / BackTab / Ctrl-J / Ctrl-K / Ctrl-O) or plugin
//! `OnFocusedCanvas` keybinding dispatch.

use savvagent_plugin::{
    ContentBlockId, InputEvent, KeyCodePortable, KeyEventPortable, MouseEventPortable,
};

use crate::HostSlot;
use crate::app::{App, Entry, InputMode, make_input_textarea};

/// Direction of canvas-to-canvas traversal (`Ctrl-J` / `Ctrl-K`).
pub(crate) const CANVAS_NEXT: i32 = 1;
pub(crate) const CANVAS_PREV: i32 = -1;

/// Return the id of the canvas adjacent to `current` in `entries` order,
/// stepping by `delta` (`+1` next, `-1` previous) with wrap-around.
/// Returns `None` when there are no canvases, and `Some(current)` when it
/// is the only canvas. Non-canvas entries are skipped.
pub(crate) fn adjacent_canvas(
    entries: &[Entry],
    current: ContentBlockId,
    delta: i32,
) -> Option<ContentBlockId> {
    let ids: Vec<ContentBlockId> = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Canvas { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    if ids.is_empty() {
        return None;
    }
    let pos = ids.iter().position(|x| *x == current)?;
    let len = ids.len() as i32;
    let next = (pos as i32 + delta).rem_euclid(len) as usize;
    Some(ids[next])
}

/// Compute the next focusable-element index after stepping `current` by
/// `delta` over `len` elements, wrapping. `None` (nothing focused) steps
/// to the first (`delta >= 0`) or last (`delta < 0`) element. Returns
/// `None` when there are no focusable elements.
pub(crate) fn cycle_index(current: Option<u32>, len: usize, delta: i32) -> Option<u32> {
    if len == 0 {
        return None;
    }
    let len_i = len as i32;
    let next = match current {
        Some(c) => (c as i32 + delta).rem_euclid(len_i),
        None if delta >= 0 => 0,
        None => len_i - 1,
    };
    Some(next as u32)
}

/// Apply the effects a canvas renderer emitted in response to an input
/// event:
///
/// * `OpenUrl { SystemBrowser }` shells out to [`App::url_opener`] (the OS
///   opener — `xdg-open` / `open` / `start`); failures are warn-only so a
///   missing opener never crashes the TUI.
/// * `OpenUrl { ContinueConversation }` stages the URL into the prompt
///   editor and notes it, leaving the user to review and submit.
///
/// `Effect::Stack` is flattened recursively. Every other effect is logged
/// and ignored — canvases don't emit them today.
pub(crate) async fn apply_canvas_effects(
    app: &mut App,
    _host_slot: &HostSlot,
    effects: Vec<savvagent_plugin::Effect>,
) {
    for effect in effects {
        match effect {
            savvagent_plugin::Effect::OpenUrl { url, target } => match target {
                savvagent_plugin::UrlTarget::SystemBrowser => {
                    match tokio::process::Command::new(&app.url_opener)
                        .arg(&url)
                        .spawn()
                    {
                        Ok(_) => {
                            app.push_note(format!("Opening {url} in browser"));
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, %url, "failed to open URL in browser");
                            app.push_note(format!("Failed to open {url}: {err}"));
                        }
                    }
                }
                savvagent_plugin::UrlTarget::ContinueConversation => {
                    app.input_textarea = make_input_textarea(std::iter::once(url.clone()));
                    app.input_mode = InputMode::Editing;
                    app.push_note(format!(
                        "Staged \"{url}\" in the prompt — press Enter to send"
                    ));
                }
            },
            savvagent_plugin::Effect::Stack(inner) => {
                Box::pin(apply_canvas_effects(app, _host_slot, inner)).await;
            }
            other => {
                tracing::warn!(effect = ?other, "ignoring unhandled canvas effect");
            }
        }
    }
}

/// Dispatch a frame-pixel mouse event to the renderer for `id` and apply any
/// returned effects. Returns `true` if the renderer reported `dirty=true`,
/// so the caller can invalidate any cached texture for that canvas.
pub async fn handle_canvas_mouse(
    app: &mut App,
    host_slot: &HostSlot,
    id: ContentBlockId,
    mouse: MouseEventPortable,
) -> bool {
    let outcome = match app.canvas_registry.get_mut(id) {
        Some(renderer) => match renderer.dispatch(InputEvent::Mouse(mouse)).await {
            Ok(outcome) => outcome,
            Err(err) => {
                tracing::warn!(error = %err, "canvas mouse dispatch failed");
                return false;
            }
        },
        None => return false,
    };
    let dirty = outcome.dirty;
    apply_canvas_effects(app, host_slot, outcome.effects).await;
    dirty
}

/// Handle a key event delivered while a canvas holds focus. Returns
/// `true` when the renderer's bitmap may have changed (so the caller
/// should invalidate any cached texture), `false` otherwise.
///
/// Precedence:
/// 1. Built-in keys (`Esc`, `Tab`, `BackTab`, `Ctrl-J`, `Ctrl-K`, `Ctrl-O`).
/// 2. Plugin `KeyScope::OnFocusedCanvas` bindings.
/// 3. Raw key dispatch to the focused renderer.
pub async fn handle_focused_canvas_key(
    app: &mut App,
    host_slot: &HostSlot,
    id: ContentBlockId,
    element_idx: Option<u32>,
    key: KeyEventPortable,
) -> bool {
    let ctrl = key.modifiers.ctrl;

    // --- 1. Built-in keys (always win) ---
    match key.code {
        KeyCodePortable::Esc => {
            app.unfocus_canvas();
            // Esc doesn't mutate the renderer's bitmap.
            return false;
        }
        KeyCodePortable::Tab => {
            let len = app
                .canvas_registry
                .get_mut(id)
                .map(|r| r.focusable_elements().len())
                .unwrap_or(0);
            let next = cycle_index(element_idx, len, 1);
            if let Some(r) = app.canvas_registry.get_mut(id) {
                r.set_focus(next);
            }
            app.set_canvas_element(next);
            // `set_focus` mutates renderer state — next paint should re-render.
            return true;
        }
        KeyCodePortable::BackTab => {
            let len = app
                .canvas_registry
                .get_mut(id)
                .map(|r| r.focusable_elements().len())
                .unwrap_or(0);
            let next = cycle_index(element_idx, len, -1);
            if let Some(r) = app.canvas_registry.get_mut(id) {
                r.set_focus(next);
            }
            app.set_canvas_element(next);
            return true;
        }
        KeyCodePortable::Char('j') if ctrl => {
            if let Some(next) = adjacent_canvas(&app.entries, id, CANVAS_NEXT) {
                app.focus_canvas(next, None);
            }
            return false;
        }
        KeyCodePortable::Char('k') if ctrl => {
            if let Some(prev) = adjacent_canvas(&app.entries, id, CANVAS_PREV) {
                app.focus_canvas(prev, None);
            }
            return false;
        }
        KeyCodePortable::Char('o') if ctrl => {
            // Open the focused canvas's final source in the system browser.
            let source = app.entries.iter().find_map(|e| match e {
                Entry::Canvas {
                    id: eid, source, ..
                } if *eid == id => Some(source.clone()),
                _ => None,
            });
            match source {
                Some(source) => {
                    use crate::plugin::builtin::html_canvas::open_in_browser;
                    match open_in_browser::write_temp_html(id, &source) {
                        Ok(path) => match open_in_browser::shell_open(&path) {
                            Ok(()) => app.push_note(format!(
                                "Opening canvas in browser ({})",
                                path.display()
                            )),
                            Err(err) => {
                                tracing::warn!(error = %err, "failed to open canvas in browser");
                                app.push_note(format!("Failed to open canvas: {err}"));
                            }
                        },
                        Err(err) => {
                            tracing::warn!(error = %err, "failed to write canvas temp file");
                            app.push_note(format!("Failed to write canvas file: {err}"));
                        }
                    }
                }
                None => app.push_note("No source available for this canvas yet".to_string()),
            }
            return false;
        }
        _ => {}
    }

    // --- 2. Plugin OnFocusedCanvas bindings (built-in keys already missed) ---
    if let (Some(_reg), Some(idx)) = (&app.plugin_registry, &app.plugin_indexes) {
        let action = {
            let idx_guard = idx.read().await;
            let router = crate::plugin::keybindings::KeybindingRouter::new(&idx_guard);
            router.route_canvas(&key)
        };
        if let Some(action) = action {
            crate::dispatch_bound_action(app, action).await;
            // Conservatively assume the plugin action may have changed
            // renderer state.
            return true;
        }
    }

    // --- 3. Raw key dispatch to the renderer ---
    // Borrow the renderer mutably only for the dispatch await; `effects`
    // is owned afterwards so no borrow of `app` is held across
    // `apply_canvas_effects` (mirrors the mouse handler).
    let dispatch_result = if let Some(renderer) = app.canvas_registry.get_mut(id) {
        match renderer
            .dispatch(savvagent_plugin::InputEvent::Key(key))
            .await
        {
            Ok(outcome) => Some(Ok((outcome.effects, outcome.dirty))),
            Err(err) => Some(Err(err)),
        }
    } else {
        None
    };
    match dispatch_result {
        Some(Ok((effects, dirty))) => {
            apply_canvas_effects(app, host_slot, effects).await;
            dirty
        }
        Some(Err(err)) => {
            tracing::warn!(error = %err, "canvas key dispatch failed");
            app.push_note(format!("Canvas didn't accept key input: {err}"));
            false
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Entry, InputMode};
    use savvagent_plugin::{
        ContentRenderer, Effect, Frame, InputOutcome, MouseEventKind, MouseEventPortable,
        PixelFormat, PixelSize, PluginError, UrlTarget,
    };
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use tokio::sync::RwLock;

    fn build_app() -> App {
        App::new("test-model".into(), PathBuf::from("/tmp"), "en".to_string())
    }

    fn empty_host_slot() -> crate::HostSlot {
        Arc::new(RwLock::new(None))
    }

    fn key(code: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code,
            modifiers: savvagent_plugin::KeyMods::default(),
        }
    }

    /// Synthetic mouse event used by the dispatch tests. Coordinates and
    /// kind are arbitrary — the tests assert on the renderer's recorded
    /// event and the function's return value, not the event payload.
    fn synthetic_mouse() -> MouseEventPortable {
        MouseEventPortable {
            kind: MouseEventKind::Move,
            button: None,
            x_pixel: 5,
            y_pixel: 5,
            modifiers: savvagent_plugin::KeyMods::default(),
        }
    }

    /// Renderer stub that records the last `dispatch` event and returns
    /// a pre-seeded `InputOutcome` (or `PluginError`). Scoped to the test
    /// module — kept distinct from `main.rs::canvas_key_tests::StubRenderer`
    /// because that one focuses on Tab/element-focus assertions instead.
    ///
    /// `last_event` is an `Arc<Mutex<...>>` so the test can keep a clone
    /// outside of the registry's `Box<dyn>` storage and inspect what the
    /// renderer recorded.
    struct DispatchStub {
        id: ContentBlockId,
        last_event: Arc<Mutex<Option<savvagent_plugin::InputEvent>>>,
        dispatch_result: Mutex<Option<Result<InputOutcome, PluginError>>>,
    }

    #[async_trait::async_trait]
    impl ContentRenderer for DispatchStub {
        fn id(&self) -> ContentBlockId {
            self.id
        }
        fn render(&mut self, _size: PixelSize) -> Frame {
            Frame {
                width: 1,
                height: 1,
                format: PixelFormat::Rgba8,
                bytes: vec![0, 0, 0, 255],
            }
        }
        async fn dispatch(
            &mut self,
            event: savvagent_plugin::InputEvent,
        ) -> Result<InputOutcome, PluginError> {
            *self.last_event.lock().unwrap() = Some(event);
            self.dispatch_result
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| {
                    Ok(InputOutcome {
                        effects: vec![],
                        dirty: false,
                    })
                })
        }
    }

    #[tokio::test]
    async fn esc_unfocuses_canvas() {
        let mut app = build_app();
        let id = ContentBlockId(0);
        // Manually seed focus state — no renderer needs to exist for the
        // built-in Esc branch.
        app.input_mode = InputMode::Canvas {
            id,
            element_idx: None,
        };
        let hs = empty_host_slot();
        handle_focused_canvas_key(&mut app, &hs, id, None, key(KeyCodePortable::Esc)).await;
        assert!(matches!(app.input_mode, InputMode::Editing));
    }

    // ---- I9: apply_canvas_effects coverage ----

    #[tokio::test]
    async fn continue_conversation_stages_prompt_and_flips_mode() {
        let mut app = build_app();
        let hs = empty_host_slot();
        let url = "https://x.example".to_string();
        apply_canvas_effects(
            &mut app,
            &hs,
            vec![Effect::OpenUrl {
                url: url.clone(),
                target: UrlTarget::ContinueConversation,
            }],
        )
        .await;
        assert!(matches!(app.input_mode, InputMode::Editing));
        let joined = app.input_textarea.lines().join("\n");
        assert!(
            joined.contains(&url),
            "expected staged URL in input textarea; got {joined:?}",
        );
        let staged_note = app
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::Note(s) => Some(s.as_str()),
                _ => None,
            })
            .any(|s| s.contains("Staged") && s.contains(&url));
        assert!(staged_note, "expected a 'Staged ...' Note entry");
    }

    #[tokio::test]
    async fn stack_flattens_recursively() {
        let mut app = build_app();
        let hs = empty_host_slot();
        let url = "https://y.example".to_string();
        let effects = vec![Effect::Stack(vec![Effect::Stack(vec![Effect::OpenUrl {
            url: url.clone(),
            target: UrlTarget::ContinueConversation,
        }])])];
        apply_canvas_effects(&mut app, &hs, effects).await;
        // Exactly one staged URL: the prompt's joined text contains the
        // URL once.
        let joined = app.input_textarea.lines().join("\n");
        assert_eq!(
            joined.matches(&url).count(),
            1,
            "recursion should apply the inner OpenUrl exactly once; got prompt {joined:?}",
        );
        // Exactly one "Staged" Note (the only inner effect produced one).
        let staged_notes: Vec<&str> = app
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::Note(s) if s.contains("Staged") => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            staged_notes.len(),
            1,
            "expected exactly one 'Staged' Note; got {staged_notes:?}",
        );
    }

    /// Drive the `SystemBrowser` arm with `opener` and return the notes it
    /// pushed.
    ///
    /// Every caller MUST pass an opener that is not a real browser launcher:
    /// `apply_canvas_effects` spawns it for real, so a default-constructed
    /// `App` here would open a window on the developer's desktop on every
    /// `cargo test` — and on every file save under `bacon test`.
    async fn notes_from_system_browser_open(opener: &str, url: &str) -> Vec<String> {
        let mut app = build_app();
        app.url_opener = opener.to_string();
        let hs = empty_host_slot();
        apply_canvas_effects(
            &mut app,
            &hs,
            vec![Effect::OpenUrl {
                url: url.to_string(),
                target: UrlTarget::SystemBrowser,
            }],
        )
        .await;
        app.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Note(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    /// A path that cannot exist, so `spawn` is guaranteed to fail and the
    /// warn-only branch runs without touching the real system.
    const MISSING_OPENER: &str = "/nonexistent/savvagent-test-url-opener";

    #[cfg(unix)]
    #[tokio::test]
    async fn open_url_system_browser_notes_success() {
        // `/bin/true` accepts the URL argument and exits 0 — a real spawn
        // with no side effect, which is what the success branch needs.
        let url = "https://z.example";
        let notes = notes_from_system_browser_open("/bin/true", url).await;
        assert_eq!(
            notes,
            vec![format!("Opening {url} in browser")],
            "success branch should push exactly the 'Opening' note",
        );
    }

    #[tokio::test]
    async fn open_url_system_browser_notes_failure() {
        let url = "https://z.example";
        let notes = notes_from_system_browser_open(MISSING_OPENER, url).await;
        assert_eq!(notes.len(), 1, "expected exactly one Note; got {notes:?}");
        assert!(
            notes[0].starts_with(&format!("Failed to open {url}: ")),
            "failure branch should push a 'Failed to open' note; got {notes:?}",
        );
    }

    // ---- I10: handle_canvas_mouse dirty propagation ----

    #[tokio::test]
    async fn mouse_dirty_true_propagates() {
        let mut app = build_app();
        let id = app.canvas_registry.allocate_id();
        app.canvas_registry.insert(
            id,
            Box::new(DispatchStub {
                id,
                last_event: Arc::new(Mutex::new(None)),
                dispatch_result: Mutex::new(Some(Ok(InputOutcome {
                    effects: vec![],
                    dirty: true,
                }))),
            }),
        );
        let dirty = handle_canvas_mouse(&mut app, &empty_host_slot(), id, synthetic_mouse()).await;
        assert!(dirty, "dirty=true from dispatch must propagate");
    }

    #[tokio::test]
    async fn mouse_dirty_false_propagates() {
        let mut app = build_app();
        let id = app.canvas_registry.allocate_id();
        app.canvas_registry.insert(
            id,
            Box::new(DispatchStub {
                id,
                last_event: Arc::new(Mutex::new(None)),
                dispatch_result: Mutex::new(Some(Ok(InputOutcome {
                    effects: vec![],
                    dirty: false,
                }))),
            }),
        );
        let dirty = handle_canvas_mouse(&mut app, &empty_host_slot(), id, synthetic_mouse()).await;
        assert!(!dirty, "dirty=false from dispatch must propagate");
    }

    #[tokio::test]
    async fn mouse_missing_renderer_returns_false() {
        let mut app = build_app();
        // Allocate an id but DO NOT insert a renderer — handle_canvas_mouse
        // must return false without panicking on the missing-renderer
        // branch.
        let id = app.canvas_registry.allocate_id();
        let dirty = handle_canvas_mouse(&mut app, &empty_host_slot(), id, synthetic_mouse()).await;
        assert!(!dirty);
    }

    #[tokio::test]
    async fn mouse_dispatch_err_returns_false() {
        let mut app = build_app();
        let id = app.canvas_registry.allocate_id();
        app.canvas_registry.insert(
            id,
            Box::new(DispatchStub {
                id,
                last_event: Arc::new(Mutex::new(None)),
                dispatch_result: Mutex::new(Some(Err(PluginError::Internal("boom".to_string())))),
            }),
        );
        let dirty = handle_canvas_mouse(&mut app, &empty_host_slot(), id, synthetic_mouse()).await;
        assert!(!dirty, "Err from dispatch must yield dirty=false");
    }

    // ---- I11: Raw key dispatch ----

    #[tokio::test]
    async fn raw_key_reaches_renderer_dispatch() {
        let mut app = build_app();
        let id = app.canvas_registry.allocate_id();
        // Keep a clone of `last_event` outside the box so the test can
        // inspect what `dispatch` recorded.
        let last_event = Arc::new(Mutex::new(None));
        let stub = DispatchStub {
            id,
            last_event: last_event.clone(),
            dispatch_result: Mutex::new(Some(Ok(InputOutcome {
                effects: vec![],
                dirty: false,
            }))),
        };
        app.canvas_registry.insert(id, Box::new(stub));
        app.entries.push(Entry::Canvas {
            id,
            source: "<p/>".into(),
            source_preview: None,
        });
        app.focus_canvas(id, None);
        let k = key(KeyCodePortable::Char('a'));
        handle_focused_canvas_key(&mut app, &empty_host_slot(), id, None, k.clone()).await;
        // The renderer's `dispatch` recorded the InputEvent it received.
        let recorded = last_event.lock().unwrap().clone();
        match recorded {
            Some(savvagent_plugin::InputEvent::Key(recorded_key)) => {
                assert_eq!(recorded_key.code, KeyCodePortable::Char('a'));
                assert!(!recorded_key.modifiers.ctrl);
                assert!(!recorded_key.modifiers.shift);
                assert!(!recorded_key.modifiers.alt);
            }
            other => panic!("expected InputEvent::Key('a') to reach renderer; got {other:?}",),
        }
    }

    #[tokio::test]
    async fn raw_key_propagates_effects() {
        let mut app = build_app();
        let id = app.canvas_registry.allocate_id();
        let url = "x.example".to_string();
        app.canvas_registry.insert(
            id,
            Box::new(DispatchStub {
                id,
                last_event: Arc::new(Mutex::new(None)),
                dispatch_result: Mutex::new(Some(Ok(InputOutcome {
                    effects: vec![Effect::OpenUrl {
                        url: url.clone(),
                        target: UrlTarget::ContinueConversation,
                    }],
                    dirty: true,
                }))),
            }),
        );
        app.entries.push(Entry::Canvas {
            id,
            source: "<p/>".into(),
            source_preview: None,
        });
        app.focus_canvas(id, None);
        let k = key(KeyCodePortable::Char('a'));
        let dirty = handle_focused_canvas_key(&mut app, &empty_host_slot(), id, None, k).await;
        assert!(dirty, "raw key must return outcome.dirty=true");
        // apply_canvas_effects ran: the ContinueConversation effect
        // flips InputMode back to Editing and stages the URL.
        assert!(matches!(app.input_mode, InputMode::Editing));
        let staged = app.input_textarea.lines().join("\n");
        assert!(
            staged.contains(&url),
            "expected URL staged in prompt; got {staged:?}",
        );
    }

    // NOTE: the plugin `OnFocusedCanvas` route is not exercised here —
    // it requires constructing `plugin_registry` + `plugin_indexes`
    // fixtures (>50 lines of setup) and is best tested in the plugin
    // crate's own routing module.
}
