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

use savvagent_plugin::{StyledLine, StyledSpan, TextMods};

use super::{FONT_SIZE, SavvagentApp};
use crate::app::Entry;
use crate::egui_app::convert::styled_line_to_job;
use crate::palette::Palette;

/// Paint the whole window: header, footer/tips, prompt, and the conversation
/// log. Bottom panels stack in add order (last-added sits highest above the
/// central panel), so we add the prompt first (lowest) then the footer above
/// it, which reads top-to-bottom as `log → footer → prompt`.
pub fn paint(state: &mut SavvagentApp, ctx: &egui::Context) {
    let palette = Palette::for_theme(state.app.active_theme);

    paint_header(state, ctx);
    paint_prompt(state, ctx);
    paint_footer(state, ctx, &palette);
    paint_log(state, ctx, &palette);
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
        // Enter (no Shift) submits. egui reports the Enter as a `key_pressed`
        // in the same frame the widget loses focus.
        let submit = resp.lost_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        if submit {
            let text = std::mem::take(&mut state.prompt);
            let trimmed = text.trim().to_string();
            if !trimmed.is_empty() && !state.app.is_loading {
                state.submit_prompt(trimmed);
            }
            // Keep typing without re-clicking the field.
            resp.request_focus();
        }
    });
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
fn paint_log(state: &SavvagentApp, ctx: &egui::Context, palette: &Palette) {
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
                for entry in &state.app.entries {
                    match entry {
                        Entry::User(text) => paint_role_block(ui, palette, "you", text),
                        Entry::Assistant(text) => paint_role_block(ui, palette, "savvagent", text),
                        Entry::Tool { name, .. } => {
                            let render = model.tool_entries.get(tool_cursor);
                            tool_cursor += 1;
                            paint_tool(ui, palette, name, render);
                        }
                        Entry::RouteBadge(text) => {
                            ui.weak(text);
                        }
                        Entry::Note(text) => {
                            ui.weak(text);
                        }
                        Entry::Canvas { .. } => {
                            ui.weak("[canvas — rendered in Plan 4]");
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
