//! egui paint pass for the native front-end.
//!
//! Synchronous and side-effect-light: it reads the already-built `App` and the
//! cached [`RenderModel`] snapshot (produced in `SavvagentApp::update` before
//! this runs) and lays them out into egui panels. The only state mutation is
//! the prompt buffer and the turn-submit it triggers on Enter. No `.await`, no
//! plugin-mutex access, no host calls happen here.
//!
//! Slot output (banner/tips/footer) and tool summaries come from the same
//! plugin render boundary the ratatui TUI uses — `build_model` reuses
//! `ui::compute_home_frame_data` — so the GUI and TUI agree on slot content.

use savvagent_plugin::manifest::ScreenLayout;
use savvagent_plugin::{StyledLine, StyledSpan, TextMods};

use super::{FONT_SIZE, SavvagentApp};
use crate::app::Entry;
use crate::egui_app::convert::styled_line_to_job;
use crate::egui_app::screen::modal_geometry;
use crate::palette::Palette;

/// Paint the whole window: header, footer/tips, prompt, and the conversation
/// log. Bottom panels stack in add order (last-added sits highest above the
/// central panel), so we add the prompt first (lowest) then the footer above
/// it, which reads top-to-bottom as `log → footer → prompt`.
pub fn paint(state: &mut SavvagentApp, ctx: &egui::Context) {
    let palette = Palette::for_theme(state.app.active_theme);
    let screen_open = !state.app.screen_stack.is_empty();

    paint_header(state, ctx);
    if !screen_open {
        paint_prompt(state, ctx); // prompt hidden while a modal owns input
    }
    paint_footer(state, ctx, &palette);
    paint_log(state, ctx, &palette);

    if screen_open {
        paint_screen_overlay(state, ctx, &palette);
    }

    // Plan 3: drive the file-dialog each frame. It paints itself; we only
    // consume the result. The picked path becomes an `@<path>` reference
    // in the prompt buffer.
    state.file_picker.update(ctx);
    if let Some(picked) = state.file_picker.take_picked() {
        crate::egui_app::widgets::file_picker::splice_at_reference(&mut state.prompt, &picked);
    }
}

/// Paint the top screen of the stack (if any) as an overlay above the home
/// panels. The screen's `render(region)` is SYNC, so no async here. The caller
/// (`paint`) checks `screen_stack` itself to suppress the home prompt; this
/// function silently does nothing when the stack is empty.
fn paint_screen_overlay(state: &mut SavvagentApp, ctx: &egui::Context, palette: &Palette) {
    // Glyph metrics for points <-> cols/rows.
    let font = egui::FontId::monospace(FONT_SIZE);
    let (glyph_w, glyph_h) = ctx.fonts(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let avail = ctx.available_rect(); // central area below/above panels already reserved

    // Extract everything we need from the screen up-front so the immutable
    // borrow on `state.app` ends before we mutably borrow `state.editor_buffer`
    // inside the area closure. The screen's `render(region)` is the only call
    // that touches per-frame screen state, and screens are owned by the stack,
    // so the cloned outputs (lines/tips/id/layout) are stable for this frame.
    let (layout, id, lines, tips, geom) = {
        let Some((screen, layout)) = state.app.screen_stack.top() else {
            return;
        };
        let geom = modal_geometry(avail, layout, glyph_w, glyph_h);
        let id = screen.id();
        let lines = screen.render(geom.region);
        let tips = screen.tips();
        (layout.clone(), id, lines, tips, geom)
    };

    // Dim the background behind a modal/bottom-sheet so focus is obvious.
    if !matches!(layout, ScreenLayout::Fullscreen { .. }) {
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new("screen_dim"),
        ));
        painter.rect_filled(avail, 0.0, egui::Color32::from_black_alpha(128));
    }

    let title = match &layout {
        ScreenLayout::CenteredModal { title: Some(t), .. } => Some(t.clone()),
        _ => None,
    };

    let area = egui::Area::new(egui::Id::new(("screen_overlay", id.clone())))
        .order(egui::Order::Foreground)
        .fixed_pos(geom.outer.min);
    area.show(ctx, |ui| {
        ui.set_clip_rect(geom.outer);
        let frame = egui::Frame::popup(ui.style())
            .fill(palette_bg(palette))
            .stroke(egui::Stroke::new(1.0, palette_border(palette)));
        frame.show(ui, |ui| {
            ui.set_width(geom.outer.width());
            ui.set_height(geom.outer.height());
            if let Some(t) = &title {
                ui.label(egui::RichText::new(t).strong());
                ui.separator();
            }
            // Plan 3: view-file/edit-file marker screens get the egui
            // code editor. The buffer lives on SavvagentApp; if it
            // hasn't loaded yet (file missing, IO error), the screen
            // paints empty and the screen's `tips()` still shows.
            if id == "view-file" || id == "edit-file" {
                if let Some(buf) = state.editor_buffer.as_mut() {
                    let editable = id == "edit-file";
                    crate::egui_app::widgets::editor::paint_editor(ui, buf, palette, editable);
                }
            }
            for line in &lines {
                ui.label(styled_line_to_job(line, palette, FONT_SIZE));
            }
            if !tips.is_empty() {
                ui.separator();
                for line in &tips {
                    ui.label(styled_line_to_job(line, palette, FONT_SIZE));
                }
            }
        });
    });
}

// Small helpers to pull two palette slots as Color32 for chrome.
fn palette_bg(p: &Palette) -> egui::Color32 {
    crate::egui_app::convert::theme_color_to_color32(savvagent_plugin::ThemeColor::Bg, p)
}
fn palette_border(p: &Palette) -> egui::Color32 {
    crate::egui_app::convert::theme_color_to_color32(savvagent_plugin::ThemeColor::Border, p)
}

/// Top panel: product name + active provider/model label.
fn paint_header(state: &SavvagentApp, ctx: &egui::Context) {
    egui::TopBottomPanel::top("header").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.heading("savvagent");
            ui.separator();
            // The active provider id is a static slug when connected; the model
            // string is whatever the host last reported. A richer
            // provider/model widget is a later task — `app.model` is the
            // honest "what will the next turn use" value for the foundation.
            let provider = state.app.active_provider_id.unwrap_or("(no provider)");
            ui.label(provider);
            if !state.app.model.is_empty() {
                ui.label("·");
                ui.label(&state.app.model);
            }
        });
    });
}

/// Bottom panel: the multi-line prompt editor. Submits on Enter (without
/// Shift). Shift+Enter inserts a newline (egui's default multiline behavior),
/// matching the ratatui prompt's contract.
fn paint_prompt(state: &mut SavvagentApp, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("prompt").show(ctx, |ui| {
        let resp = ui.add(
            egui::TextEdit::multiline(&mut state.prompt)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("Ask savvagent…"),
        );
        // Enter (no Shift) submits. A *multiline* `TextEdit` consumes Enter to
        // insert a newline and KEEPS focus, so `lost_focus()` never fires on
        // Enter — gating submit on it (the single-line idiom) meant the prompt
        // never submitted. Gate on `has_focus()` instead: the widget still
        // inserts a trailing newline for this Enter, but `mem::take` + `trim`
        // below discard it. Shift+Enter falls through to the default newline.
        let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        let submit = resp.has_focus() && enter_pressed;
        if submit {
            let text = std::mem::take(&mut state.prompt);
            let trimmed = text.trim().to_string();
            if !trimmed.is_empty() && !state.app.is_loading {
                state.submit_prompt(trimmed);
            }
            // Keep typing without re-clicking the field.
            resp.request_focus();
        }

        // `/` on an empty prompt opens the command palette — the egui
        // equivalent of the TUI's `OnHome` `/` keybinding (which the egui
        // shell doesn't route through the plugin `KeybindingRouter`). The
        // typed `/` is cleared and `pending_open_palette` tells the next
        // frame's async pass to emit `Effect::OpenScreen { id: "palette" }`.
        // `paint_prompt` only runs when no screen is open, so no extra guard.
        if palette_trigger(&state.prompt) {
            state.prompt.clear();
            state.pending_open_palette = true;
            ctx.request_repaint();
        }

        // `@` opens the file picker to browse for a path — the egui
        // equivalent of the ratatui prompt's `@`-opens-explorer affordance
        // (main.rs's `KeyCode::Char('@')` arm). Unlike `/`, `@` is valid
        // mid-prompt (e.g. `explain @foo`), so we trigger on the keystroke,
        // not on buffer contents. The typed `@` stays in the buffer as the
        // splice marker; `paint`'s `take_picked` → `splice_at_reference`
        // replaces it with the chosen `@<path>`. Cancelling leaves the bare
        // `@` for the user to edit or delete.
        if resp.has_focus() && typed_at_marker(ui) {
            state.file_picker.open();
        }
    });
}

/// Whether the current prompt buffer should open the command palette.
/// Fires only when the user has typed exactly `/` into an otherwise-empty
/// prompt — a `/` after other text is a literal slash, not a trigger.
fn palette_trigger(prompt: &str) -> bool {
    prompt == "/"
}

/// Whether the user typed `@` this frame (a text-input event for `@`).
/// Used to open the file picker from the prompt, mirroring the ratatui
/// `@`-opens-explorer keybinding.
fn typed_at_marker(ui: &egui::Ui) -> bool {
    ui.input(|i| {
        i.events
            .iter()
            .any(|e| matches!(e, egui::Event::Text(t) if t == "@"))
    })
}

/// Bottom panel (above the prompt): plugin tips followed by the three footer
/// segments. All lines render through the shared styled-line → `LayoutJob`
/// sink so colors resolve against the active palette exactly as in the TUI.
fn paint_footer(state: &SavvagentApp, ctx: &egui::Context, palette: &Palette) {
    let model = state.render_cache().lock().unwrap().clone();
    egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
        for line in &model.tips {
            ui.label(styled_line_to_job(line, palette, FONT_SIZE));
        }
        for line in model
            .footer_left
            .iter()
            .chain(&model.footer_center)
            .chain(&model.footer_right)
        {
            ui.label(styled_line_to_job(line, palette, FONT_SIZE));
        }
    });
}

/// Central panel: the conversation log, bottom-anchored and scrollable.
fn paint_log(state: &mut SavvagentApp, ctx: &egui::Context, palette: &Palette) {
    enum EntrySnap {
        User(String),
        Assistant(String),
        Tool(String),
        Note(String),
    }

    let screen_open = !state.app.screen_stack.is_empty();
    let model = state.render_cache().lock().unwrap().clone();
    egui::CentralPanel::default().show(ctx, |ui| {
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Banner (welcome art / splash) sits above the log, mirroring
                // the TUI home screen. The plugin-driven banner slot is part
                // of the same render model the footer/tips come from.
                for line in &model.banner {
                    ui.label(styled_line_to_job(line, palette, FONT_SIZE));
                }
                if !model.banner.is_empty() && !state.app.entries.is_empty() {
                    ui.add_space(8.0);
                }

                // The Nth `Entry::Tool` maps to the Nth `tool_entries` element
                // — the render model is built in entry order. We walk a tool
                // cursor alongside the entry loop to keep them aligned.
                let mut tool_cursor = 0usize;
                let mut any_canvas_clicked = false;
                let entry_count = state.app.entries.len();
                for idx in 0..entry_count {
                    // Snapshot the entry shape for the immutable branches so
                    // we don't hold a borrow across the canvas branch.
                    let entry_snapshot = match &state.app.entries[idx] {
                        Entry::User(t) => Some(EntrySnap::User(t.clone())),
                        Entry::Assistant(t) => Some(EntrySnap::Assistant(t.clone())),
                        Entry::Tool { name, .. } => Some(EntrySnap::Tool(name.clone())),
                        Entry::RouteBadge(t) | Entry::Note(t) => Some(EntrySnap::Note(t.clone())),
                        Entry::Canvas { .. } => None, // handled below
                    };
                    if let Some(snap) = entry_snapshot {
                        match snap {
                            EntrySnap::User(t) => paint_role_block(ui, palette, "you", &t),
                            EntrySnap::Assistant(t) => {
                                paint_role_block(ui, palette, "savvagent", &t)
                            }
                            EntrySnap::Tool(name) => {
                                let render = model.tool_entries.get(tool_cursor);
                                tool_cursor += 1;
                                paint_tool(ui, palette, &name, render);
                            }
                            EntrySnap::Note(t) => {
                                ui.weak(t);
                            }
                        }
                    } else {
                        // Canvas branch: pull the source/preview out by
                        // cloning so `state.app` can be borrowed mutably
                        // for the renderer (the indexed entry aliases the
                        // same borrow).
                        let (cid, source, preview) = match &state.app.entries[idx] {
                            Entry::Canvas {
                                id,
                                source,
                                source_preview,
                            } => (*id, source.clone(), source_preview.clone()),
                            Entry::User(..)
                            | Entry::Assistant(..)
                            | Entry::Tool { .. }
                            | Entry::RouteBadge(..)
                            | Entry::Note(..) => {
                                unreachable!("entry_snapshot was None only for Canvas")
                            }
                        };
                        let clicked = crate::egui_app::widgets::canvas::paint(
                            ui,
                            ctx,
                            &mut state.app,
                            &mut state.gui_canvas_cache,
                            &state.host_slot,
                            &state.rt,
                            cid,
                            &source,
                            preview.as_deref(),
                            screen_open,
                            palette,
                        );
                        if clicked {
                            any_canvas_clicked = true;
                        }
                    }
                    ui.add_space(4.0);
                }

                // In-flight streaming assistant text: `App::live_text`
                // accumulates deltas until `flush_live_text` finalizes them
                // into an `Entry::Assistant`. Paint it live so the response
                // appears as it streams.
                if !state.app.live_text.is_empty() {
                    paint_role_block(ui, palette, "savvagent", &state.app.live_text);
                }

                // Click-outside-to-unfocus: if the user clicked anywhere
                // this frame but no canvas claimed it, drop any active
                // canvas focus. Runs after the entry loop so
                // `any_canvas_clicked` is final. Skipped while a screen
                // overlay is up — the user can't be clicking on a canvas,
                // and `InputMode::Canvas` should survive the overlay.
                let global_click = ctx.input(|i| i.pointer.any_click());
                if !screen_open
                    && global_click
                    && !any_canvas_clicked
                    && matches!(state.app.input_mode, crate::app::InputMode::Canvas { .. })
                {
                    state.app.unfocus_canvas();
                }
            });
    });
}

/// Paint a bold role label followed by the entry text rendered as plain
/// monospace lines (one `LayoutJob` per raw line, wrapped in an unstyled
/// `StyledLine` so it flows through the same palette-aware sink).
fn paint_role_block(ui: &mut egui::Ui, palette: &Palette, role: &str, text: &str) {
    ui.label(egui::RichText::new(role).strong());
    for raw in text.split('\n') {
        let line = StyledLine {
            spans: vec![StyledSpan {
                text: raw.to_string(),
                fg: None,
                bg: None,
                modifiers: TextMods::default(),
            }],
        };
        ui.label(styled_line_to_job(&line, palette, FONT_SIZE));
    }
}

/// Paint a tool entry from its pre-rendered summary spans (arg spans, then
/// result spans when the call has completed). Falls back to the bare tool name
/// when no matching render-model element exists (e.g. the model rebuilt a
/// frame behind the entry list).
fn paint_tool(
    ui: &mut egui::Ui,
    palette: &Palette,
    name: &str,
    render: Option<&crate::ui::ToolEntryRender>,
) {
    ui.label(egui::RichText::new(format!("⚙ {name}")).strong());
    let Some(render) = render else {
        return;
    };
    let arg_line = StyledLine {
        spans: render.arg_spans.clone(),
    };
    ui.label(styled_line_to_job(&arg_line, palette, FONT_SIZE));
    if let Some(result_spans) = &render.result_spans {
        let result_line = StyledLine {
            spans: result_spans.clone(),
        };
        ui.label(styled_line_to_job(&result_line, palette, FONT_SIZE));
    }
}

#[cfg(test)]
mod tests {
    use super::palette_trigger;

    #[test]
    fn lone_slash_triggers_palette() {
        assert!(palette_trigger("/"));
    }

    #[test]
    fn empty_prompt_does_not_trigger() {
        assert!(!palette_trigger(""));
    }

    #[test]
    fn slash_with_text_does_not_trigger() {
        // The user is typing/filtering — a `/cmd` partial or a `/` mid-text
        // is a literal slash, not a palette-open request.
        assert!(!palette_trigger("/co"));
        assert!(!palette_trigger("fix bug in a/b"));
        assert!(!palette_trigger(" /"));
    }
}
