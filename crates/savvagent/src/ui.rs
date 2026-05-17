//! Render pass: paint the current [`App`] state into the frame.

use crate::app::{App, Entry, InputMode, TranscriptEntry};
use crate::palette::Palette;
use crate::providers::PROVIDERS;
use crate::splash;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, FrameExt, List, ListItem, Padding, Paragraph, Wrap},
};
use savvagent_host::ToolCallStatus;

/// Pre-computed plugin slot output for one render frame. Built async from
/// `compute_home_frame_data` before `terminal.draw` runs so the draw closure
/// stays synchronous and never touches plugin mutexes.
pub struct HomeFrameData {
    pub banner: Vec<savvagent_plugin::StyledLine>,
    pub tips: Vec<savvagent_plugin::StyledLine>,
    pub footer_left: Vec<savvagent_plugin::StyledLine>,
    pub footer_center: Vec<savvagent_plugin::StyledLine>,
    pub footer_right: Vec<savvagent_plugin::StyledLine>,
}

impl HomeFrameData {
    /// Empty fallback used when plugins are not installed yet.
    pub fn empty() -> Self {
        Self {
            banner: vec![],
            tips: vec![],
            footer_left: vec![],
            footer_center: vec![],
            footer_right: vec![],
        }
    }
}

/// Resolve every slot's lines for the current frame. Locks plugin mutexes
/// briefly per contributor.
pub async fn compute_home_frame_data(app: &crate::app::App, area: Rect) -> HomeFrameData {
    use std::sync::Once;

    use crate::plugin::convert::rect_to_region;
    use crate::plugin::slots::SlotRouter;

    static WARNED_NO_RUNTIME: Once = Once::new();

    let (Some(reg), Some(idx)) = (
        app.plugin_registry.as_ref().cloned(),
        app.plugin_indexes.as_ref().cloned(),
    ) else {
        WARNED_NO_RUNTIME.call_once(|| {
            tracing::warn!(
                "compute_home_frame_data called before install_plugin_runtime — TUI is rendering with no plugin output"
            );
        });
        return HomeFrameData::empty();
    };
    let reg_guard = reg.read().await;
    let idx_guard = idx.read().await;
    let router = SlotRouter::new(&idx_guard, &reg_guard);

    let full_row = rect_to_region(Rect::new(area.x, area.y, area.width, 1));
    let banner = router.render("home.banner", full_row).await;
    let tips = router.render("home.tips", full_row).await;
    let footer_left = router.render("home.footer.left", full_row).await;
    let footer_center = router.render("home.footer.center", full_row).await;
    let footer_right = router.render("home.footer.right", full_row).await;

    HomeFrameData {
        banner,
        tips,
        footer_left,
        footer_center,
        footer_right,
    }
}

pub fn render(app: &mut App, frame: &mut Frame, frame_data: &HomeFrameData) {
    let area = frame.area();

    if app.show_splash {
        splash::render(frame, area, &app.splash_sandbox);
        return;
    }

    let palette = Palette::for_theme(app.active_theme);

    // Paint the active theme's base style across the whole frame so any
    // widget that doesn't set its own bg picks up the theme background.
    frame.buffer_mut().set_style(area, palette.base_style());

    // Build the prompt textarea up-front so we can ask tui-textarea how
    // tall it wants to be (driven by wrap mode + min/max rows configured
    // on `app.input_textarea`). The measured height drives the input
    // constraint below so the box grows with multi-line / wrapped input
    // and shrinks back to its 3-row minimum when cleared.
    let mut textarea = app.input_textarea.clone();
    textarea.set_block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette.border).bg(palette.bg))
            .padding(Padding::horizontal(1)),
    );
    textarea.set_style(palette.base_style());
    // tui-textarea defaults the cursor-line style to UNDERLINED, which
    // ends up underlining the whole one-line prompt. Override to the
    // base style so the input renders flat like the rest of the UI.
    textarea.set_cursor_line_style(palette.base_style());
    let input_rows = textarea.measure(area.width).preferred_rows;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),          // header
            Constraint::Min(1),             // log
            Constraint::Length(1),          // banner (plugin slot: home.banner)
            Constraint::Length(1),          // tips (plugin slot: home.tips)
            Constraint::Length(input_rows), // input (dynamic, clamped by textarea min/max rows)
            Constraint::Length(1),          // footer (plugin slots: home.footer.*)
        ])
        .split(area);

    let resumed_label = app
        .resumed_at
        .as_deref()
        .map(|ts| format!(" · resumed: {ts}"))
        .unwrap_or_default();
    let header_text = if app.connected {
        format!(
            "Savvagent — {} · {}{}",
            app.active_provider_id.unwrap_or("?"),
            app.model,
            resumed_label,
        )
    } else {
        "Savvagent — disconnected · type /connect".to_string()
    };
    let header_color = if app.connected {
        palette.accent
    } else {
        palette.warning
    };
    let header = Paragraph::new(header_text)
        .style(
            palette
                .base_style()
                .fg(header_color)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette.border).bg(palette.bg))
                .padding(Padding::horizontal(2)),
        );
    frame.render_widget(header, chunks[0]);

    render_log(app, frame, chunks[1], palette);

    // Banner row — one-line update banner, rendered from plugin slot.
    // The slot returns nothing when there is no update available, so the
    // row paints as theme background only.
    let banner_lines: Vec<Line<'static>> = frame_data
        .banner
        .iter()
        .cloned()
        .map(|l| crate::plugin::convert::styled_line_to_ratatui(l, &palette))
        .collect();
    let banner_para = Paragraph::new(banner_lines).style(palette.base_style());
    frame.render_widget(banner_para, chunks[2]);

    // Tips row — one-line hints above the prompt, rendered from plugin slot.
    // Inset horizontally so the row aligns with the content inside the
    // bordered blocks above and below (border + interior padding = 3 cols).
    let tips_lines: Vec<Line<'static>> = frame_data
        .tips
        .iter()
        .cloned()
        .map(|l| crate::plugin::convert::styled_line_to_ratatui(l, &palette))
        .collect();
    let tips_para = Paragraph::new(tips_lines).style(palette.base_style());
    frame.render_widget(tips_para, chunks[3]);

    frame.render_widget(&textarea, chunks[4]);

    // Footer row — see `compose_footer_line` for the join semantics.
    let separator = savvagent_plugin::StyledSpan {
        text: " · ".into(),
        fg: Some(savvagent_plugin::ThemeColor::Muted),
        bg: None,
        modifiers: savvagent_plugin::TextMods::default(),
    };
    let footer_line = crate::plugin::convert::styled_line_to_ratatui(
        compose_footer_line(
            [
                &frame_data.footer_left,
                &frame_data.footer_center,
                &frame_data.footer_right,
            ],
            &separator,
        ),
        &palette,
    );
    frame.render_widget(
        Paragraph::new(footer_line).style(palette.base_style()),
        chunks[5],
    );

    if app.is_file_picker_active {
        let popup = centered_rect(60, 40, area);
        frame.render_widget(Clear, popup);
        frame.render_widget_ref(app.file_explorer.widget(), popup);
    }

    // Screen-stack: if any screen is on top, paint it over the home chrome.
    if let Some((top_screen, layout)) = app.screen_stack.top() {
        // view-file / edit-file are marker screens whose actual content
        // lives in `App::editor` (ratatui-code-editor). Render the
        // editor widget directly in a bordered popup with a title that
        // matches the legacy `InputMode::ViewingFile`/`EditingFile`
        // chrome. Other screens go through the styled-line render path.
        let top_id = top_screen.id();
        let is_file_screen = top_id == "view-file" || top_id == "edit-file";
        if is_file_screen {
            paint_file_screen(frame, area, app, palette, top_id == "edit-file");
        } else {
            paint_screen(frame, area, top_screen, layout, palette);
        }
    }

    if matches!(
        app.input_mode,
        InputMode::ViewingFile | InputMode::EditingFile
    ) {
        if let Some(editor) = &app.editor {
            let popup = centered_rect(80, 80, area);
            frame.render_widget(Clear, popup);

            let path_str = app
                .active_file_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let (title, hint) = if matches!(app.input_mode, InputMode::EditingFile) {
                (
                    format!(" Editing: {path_str} "),
                    " [Esc] Save & Close | [Enter] New Line ",
                )
            } else {
                (
                    format!(" Viewing: {path_str} "),
                    " [Esc] Close | [j/k] Scroll ",
                )
            };

            let block = Block::default()
                .borders(Borders::ALL)
                .title(Line::styled(title, palette.base_style().fg(palette.fg)))
                .title_bottom(Line::from(hint).right_aligned());
            let inner = popup.inner(Margin {
                horizontal: 1,
                vertical: 1,
            });
            frame.render_widget(block, popup);
            frame.render_widget(editor, inner);
        }
    }

    if matches!(app.input_mode, InputMode::EditingFile) {
        if let Some(editor) = &app.editor {
            let popup = centered_rect(80, 80, area);
            let inner = popup.inner(Margin {
                horizontal: 1,
                vertical: 1,
            });
            if let Some((x, y)) = editor.get_visible_cursor(&inner) {
                frame.set_cursor_position((x, y));
            }
        }
    }

    if matches!(app.input_mode, InputMode::SelectingProvider) {
        let popup = centered_rect(60, 40, area);
        frame.render_widget(Clear, popup);
        let items: Vec<ListItem> = PROVIDERS
            .iter()
            .enumerate()
            .map(|(i, spec)| {
                let style = if i == app.provider_index {
                    palette
                        .base_style()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    palette.base_style()
                };
                let active_marker = if Some(spec.id) == app.active_provider_id {
                    " (active)"
                } else {
                    ""
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<22}", spec.display_name), style),
                    Span::styled(
                        format!(" {}{}", spec.id, active_marker),
                        palette.base_style().fg(palette.muted),
                    ),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette.border).bg(palette.bg))
                    .padding(Padding::new(2, 2, 1, 0))
                    .title(Line::styled(
                        " Connect to provider ",
                        palette.base_style().fg(palette.fg),
                    ))
                    .title_bottom(
                        Line::from(" [↑/↓] move  [Enter] select  [Esc] cancel ").right_aligned(),
                    ),
            )
            .style(palette.base_style())
            .highlight_symbol("> ");
        frame.render_widget(list, popup);
    }

    if matches!(app.input_mode, InputMode::PermissionPrompt) {
        if let Some(req) = &app.pending_permission {
            let popup = centered_rect(60, 40, area);
            frame.render_widget(Clear, popup);

            let args_pretty =
                serde_json::to_string_pretty(&req.args).unwrap_or_else(|_| req.args.to_string());
            let mut lines: Vec<Line<'static>> = Vec::new();
            lines.push(Line::from(Span::styled(
                format!("Tool: {}", req.name),
                palette
                    .base_style()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                req.summary.clone(),
                palette.base_style().fg(palette.fg),
            )));
            lines.push(Line::from(""));
            for line in args_pretty.lines() {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    palette.base_style().fg(palette.muted),
                )));
            }

            let body = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .style(palette.base_style())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(palette.border).bg(palette.bg))
                        .padding(Padding::new(2, 2, 1, 0))
                        .title(Line::styled(
                            " Permission requested ",
                            palette.base_style().fg(palette.fg),
                        ))
                        .title_bottom(
                            Line::from(" [y] allow  [n] deny  [a] always  [N] never  [Esc] deny ")
                                .right_aligned(),
                        ),
                );
            frame.render_widget(body, popup);
        }
    }

    if let InputMode::BashNetworkPrompt { summary, .. } = &app.input_mode {
        let popup = centered_rect(60, 35, area);
        frame.render_widget(Clear, popup);

        let lines: Vec<Line<'static>> = vec![
            Line::from(Span::styled(
                "Bash needs network access".to_string(),
                palette
                    .base_style()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                summary.clone(),
                palette.base_style().fg(palette.fg),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  [O]nce              allow this invocation only".to_string(),
                palette.base_style().fg(palette.success),
            )),
            Line::from(Span::styled(
                "  [A]lways            allow for the rest of this session".to_string(),
                palette.base_style().fg(palette.success),
            )),
            Line::from(Span::styled(
                "  [D]eny once         deny this invocation only".to_string(),
                palette.base_style().fg(palette.error),
            )),
            Line::from(Span::styled(
                "  [F]orever (Never)   deny for the rest of this session".to_string(),
                palette.base_style().fg(palette.error),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Per-call override: re-run with `/bash --net <cmd>` or `/bash --no-net <cmd>`"
                    .to_string(),
                palette.base_style().fg(palette.muted),
            )),
        ];

        let body = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(palette.base_style())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette.border).bg(palette.bg))
                    .padding(Padding::new(2, 2, 1, 0))
                    .title(Line::styled(
                        " Bash network access? ",
                        palette.base_style().fg(palette.fg),
                    ))
                    .title_bottom(
                        Line::from(" [O]nce  [A]lways  [D]eny  [F]orever  [Esc] deny ")
                            .right_aligned(),
                    ),
            );
        frame.render_widget(body, popup);
    }

    if matches!(app.input_mode, InputMode::EnteringApiKey) {
        let popup = centered_rect(60, 20, area);
        frame.render_widget(Clear, popup);
        let title = match app.pending_provider {
            Some(spec) => format!(" {} API key ", spec.display_name),
            None => " API key ".to_string(),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette.border).bg(palette.bg))
            .style(palette.base_style())
            .title(Line::styled(title, palette.base_style().fg(palette.fg)))
            .title_bottom(Line::from(" [Enter] connect  [Esc] cancel ").right_aligned());
        let inner = popup.inner(Margin {
            horizontal: 2,
            vertical: 1,
        });
        frame.render_widget(block, popup);
        let mut ta = app.api_key_textarea.clone();
        ta.set_block(Block::default());
        ta.set_style(palette.base_style());
        frame.render_widget(&ta, inner);
    }

    if matches!(app.input_mode, InputMode::SelectingTranscript) {
        render_transcript_picker(app, frame, area, palette);
    }
}

fn render_transcript_picker(app: &App, frame: &mut Frame, area: Rect, palette: Palette) {
    let popup = centered_rect(70, 50, area);
    frame.render_widget(Clear, popup);

    if app.transcript_entries.is_empty() {
        let body = Paragraph::new("No transcripts found in ~/.savvagent/transcripts/")
            .style(palette.base_style())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette.border).bg(palette.bg))
                    .padding(Padding::new(2, 2, 1, 0))
                    .title(Line::styled(
                        " Resume transcript ",
                        palette.base_style().fg(palette.fg),
                    ))
                    .title_bottom(Line::from(" [Esc] cancel ").right_aligned()),
            );
        frame.render_widget(body, popup);
        return;
    }

    let items: Vec<ListItem> = app
        .transcript_entries
        .iter()
        .enumerate()
        .map(|(i, entry)| render_transcript_item(entry, i == app.transcript_index, palette))
        .collect();

    let list = List::new(items).style(palette.base_style()).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette.border).bg(palette.bg))
            .padding(Padding::new(2, 2, 1, 0))
            .title(" Resume transcript ")
            .title_bottom(Line::from(" [↑/↓] move  [Enter] resume  [Esc] cancel ").right_aligned()),
    );
    frame.render_widget(list, popup);
}

fn render_transcript_item(
    entry: &TranscriptEntry,
    selected: bool,
    palette: Palette,
) -> ListItem<'static> {
    let style = if selected {
        palette
            .base_style()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        palette.base_style()
    };
    let meta_style = palette.base_style().fg(palette.muted);
    let line = Line::from(vec![
        Span::styled(format!("{:<22}", entry.timestamp), style),
        Span::styled(format!(" {:>3} msgs  ", entry.message_count), meta_style),
        Span::styled(entry.preview.clone(), palette.base_style().fg(palette.fg)),
    ]);
    ListItem::new(line)
}

fn render_log(app: &App, frame: &mut Frame, area: Rect, palette: Palette) {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(app.entries.len() * 2 + 1);
    for entry in &app.entries {
        match entry {
            Entry::User(text) => {
                lines.push(line_block(
                    rust_i18n::t!("conversation.you-prefix").as_ref(),
                    text,
                    palette.success,
                    palette,
                ));
            }
            Entry::Assistant(text) => {
                lines.push(line_block(
                    rust_i18n::t!("conversation.agent-prefix").as_ref(),
                    text,
                    palette.secondary,
                    palette,
                ));
            }
            Entry::Tool {
                name,
                arguments,
                status,
                result_preview,
            } => {
                let badge = match status {
                    None => "…",
                    Some(ToolCallStatus::Ok) => "✓",
                    Some(ToolCallStatus::Errored) => "✗",
                };
                let color = match status {
                    None => palette.warning,
                    Some(ToolCallStatus::Ok) => palette.success,
                    Some(ToolCallStatus::Errored) => palette.error,
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {badge} "), palette.base_style().fg(color)),
                    Span::styled(
                        format!("{name}({arguments})"),
                        palette
                            .base_style()
                            .fg(palette.warning)
                            .add_modifier(Modifier::DIM),
                    ),
                ]));
                if let Some(preview) = result_preview {
                    lines.push(Line::from(Span::styled(
                        format!("    → {preview}"),
                        palette.base_style().fg(palette.muted),
                    )));
                }
            }
            Entry::RouteBadge(text) => {
                // Muted single line. Style matches Entry::Note; the leading
                // glyph distinguishes routing decisions from generic notices.
                lines.push(Line::from(Span::styled(
                    format!("▸ {text}"),
                    palette
                        .base_style()
                        .fg(palette.muted)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
            Entry::Note(text) => {
                lines.push(Line::from(Span::styled(
                    format!("· {text}"),
                    palette
                        .base_style()
                        .fg(palette.muted)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        }
    }

    if !app.live_text.is_empty() {
        lines.push(line_block(
            rust_i18n::t!("conversation.agent-prefix").as_ref(),
            &app.live_text,
            palette.secondary,
            palette,
        ));
    }

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .style(palette.base_style())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette.border).bg(palette.bg))
                .padding(Padding::new(2, 2, 1, 1))
                .title(Line::styled(
                    " Conversation ",
                    palette.base_style().fg(palette.fg),
                )),
        );
    frame.render_widget(para, area);
}

fn line_block(prefix: &str, text: &str, color: Color, palette: Palette) -> Line<'static> {
    let style = palette.base_style().fg(color);
    Line::from(vec![
        Span::styled(prefix.to_string(), style.add_modifier(Modifier::BOLD)),
        Span::styled(text.to_string(), style),
    ])
}

/// Paint a plugin-provided screen over `area`, using the screen's declared
/// [`savvagent_plugin::ScreenLayout`] to position it.
///
/// For `CenteredModal`, the host draws the border and title so the
/// screen's `render` output fills the inner content area.
/// For `Fullscreen` and `BottomSheet`, content fills the computed area
/// directly.
///
/// Every layout punches a hole with [`Clear`] and then fills its region
/// with `palette.base_style()` so the modal sits on a uniform theme
/// background. Without that step the conversation log behind the modal
/// would bleed through under any plugin span that only sets `fg` — which
/// makes upstream themes (Solarized Light, Catppuccin Latte, Tokyo Night
/// Day, …) look like floating text rather than a popup.
/// Render the marker `view-file` / `edit-file` screen by drawing the
/// ratatui-code-editor widget held in `App::editor` inside a bordered
/// modal. Mirrors the legacy `InputMode::ViewingFile`/`EditingFile`
/// chrome but is driven by the screen stack instead of the deprecated
/// input-mode state machine.
fn paint_file_screen(
    f: &mut Frame,
    area: Rect,
    app: &crate::app::App,
    palette: Palette,
    edit: bool,
) {
    let popup = centered_rect(80, 80, area);
    f.render_widget(Clear, popup);
    f.buffer_mut().set_style(popup, palette.base_style());

    let path_str = app
        .active_file_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let (title_key, hint_key) = if edit {
        ("picker.edit-file.modal-title", "picker.edit-file.tips")
    } else {
        ("picker.view-file.modal-title", "picker.view-file.tips")
    };
    let title = format!(" {}: {} ", rust_i18n::t!(title_key), path_str);
    let hint = rust_i18n::t!(hint_key).to_string();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.border).bg(palette.bg))
        .title(Line::styled(title, palette.base_style().fg(palette.fg)))
        .title_bottom(Line::from(format!(" {hint} ")).right_aligned())
        .style(palette.base_style());

    let inner = popup.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    f.render_widget(block, popup);

    if let Some(editor) = &app.editor {
        f.render_widget(editor, inner);
        if edit {
            if let Some((x, y)) = editor.get_visible_cursor(&inner) {
                f.set_cursor_position((x, y));
            }
        }
    }
}

fn paint_screen(
    f: &mut Frame,
    area: Rect,
    screen: &dyn savvagent_plugin::Screen,
    layout: &savvagent_plugin::ScreenLayout,
    palette: Palette,
) {
    use savvagent_plugin::ScreenLayout;

    match layout {
        ScreenLayout::Fullscreen { .. } => {
            // Full-frame overlay: paint content directly.
            f.render_widget(Clear, area);
            f.buffer_mut().set_style(area, palette.base_style());
            let region = crate::plugin::convert::rect_to_region(area);
            let lines: Vec<Line<'static>> = screen
                .render(region)
                .into_iter()
                .map(|l| crate::plugin::convert::styled_line_to_ratatui(l, &palette))
                .collect();
            let para = Paragraph::new(lines).style(palette.base_style());
            f.render_widget(para, area);

            // Tips row at the very bottom of the frame.
            let tips = screen.tips();
            if !tips.is_empty() && area.height > 0 {
                let tips_row = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
                let tips_lines: Vec<Line<'static>> = tips
                    .into_iter()
                    .map(|l| crate::plugin::convert::styled_line_to_ratatui(l, &palette))
                    .collect();
                f.render_widget(
                    Paragraph::new(tips_lines).style(palette.base_style()),
                    tips_row,
                );
            }
        }
        ScreenLayout::CenteredModal {
            width_pct,
            height_pct,
            title,
        } => {
            // Compute the outer rect for the modal border.
            let w = ((area.width as u32 * (*width_pct as u32)) / 100)
                .max(20)
                .min(area.width as u32) as u16;
            let h = ((area.height as u32 * (*height_pct as u32)) / 100)
                .max(5)
                .min(area.height as u32) as u16;
            let x = area.x + area.width.saturating_sub(w) / 2;
            let y = area.y + area.height.saturating_sub(h) / 2;
            let outer = Rect::new(x, y, w, h);

            // Punch a hole over whatever's underneath, then fill the modal's
            // region with the theme's base style so spans that only set fg
            // sit on a uniform bg instead of the conversation log behind.
            f.render_widget(Clear, outer);
            f.buffer_mut().set_style(outer, palette.base_style());

            // Border + optional title.
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette.border).bg(palette.bg))
                .style(palette.base_style())
                .title(Line::styled(
                    title.as_deref().unwrap_or("").to_string(),
                    palette.base_style().fg(palette.fg),
                ));

            // Tips as a bottom title if present.
            let tips = screen.tips();
            let block = if let Some(tip_line) = tips.into_iter().next() {
                let tip_text: String = tip_line.spans.iter().map(|s| s.text.as_str()).collect();
                block.title_bottom(Line::from(tip_text).right_aligned())
            } else {
                block
            };

            // Interior padding: 2 cols horizontally and 1 row top/bottom
            // gives modal content breathing room inside the border.
            let inner = outer.inner(Margin {
                horizontal: 2,
                vertical: 1,
            });
            f.render_widget(block, outer);

            let region = crate::plugin::convert::rect_to_region(inner);
            let lines: Vec<Line<'static>> = screen
                .render(region)
                .into_iter()
                .map(|l| crate::plugin::convert::styled_line_to_ratatui(l, &palette))
                .collect();
            f.render_widget(Paragraph::new(lines).style(palette.base_style()), inner);
        }
        ScreenLayout::BottomSheet { height } => {
            let h = (*height).min(area.height);
            let sheet = Rect::new(area.x, area.y + area.height - h, area.width, h);
            f.render_widget(Clear, sheet);
            f.buffer_mut().set_style(sheet, palette.base_style());
            let region = crate::plugin::convert::rect_to_region(sheet);
            let lines: Vec<Line<'static>> = screen
                .render(region)
                .into_iter()
                .map(|l| crate::plugin::convert::styled_line_to_ratatui(l, &palette))
                .collect();
            f.render_widget(Paragraph::new(lines).style(palette.base_style()), sheet);

            let tips = screen.tips();
            if !tips.is_empty() && sheet.height > 0 {
                let tips_row = Rect::new(sheet.x, sheet.y + sheet.height - 1, sheet.width, 1);
                let tips_lines: Vec<Line<'static>> = tips
                    .into_iter()
                    .map(|l| crate::plugin::convert::styled_line_to_ratatui(l, &palette))
                    .collect();
                f.render_widget(
                    Paragraph::new(tips_lines).style(palette.base_style()),
                    tips_row,
                );
            }
        }
        // Future layout variants are silently treated as fullscreen.
        _ => {
            f.render_widget(Clear, area);
            f.buffer_mut().set_style(area, palette.base_style());
            let region = crate::plugin::convert::rect_to_region(area);
            let lines: Vec<Line<'static>> = screen
                .render(region)
                .into_iter()
                .map(|l| crate::plugin::convert::styled_line_to_ratatui(l, &palette))
                .collect();
            f.render_widget(Paragraph::new(lines).style(palette.base_style()), area);
        }
    }
}

pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Flatten the three home-footer slot groups into a single styled line
/// that flows left-to-right.
///
/// Output reads like:
///   `provider · turn-state · cwd · ~N ctx · $0.00 · vX.Y.Z`
///
/// `separator` is inserted between every non-empty `StyledLine` across
/// all groups, in slot order — including multiple contributors to the
/// same slot (the `SlotRouter` concatenates each plugin's output, so a
/// future second contributor to `home.footer.left` shares the slot
/// without its content being silently dropped). Lines with no spans are
/// skipped and never introduce a stray separator.
fn compose_footer_line(
    groups: [&[savvagent_plugin::StyledLine]; 3],
    separator: &savvagent_plugin::StyledSpan,
) -> savvagent_plugin::StyledLine {
    let mut spans: Vec<savvagent_plugin::StyledSpan> = Vec::new();
    for group in groups {
        for line in group {
            if line.spans.is_empty() {
                continue;
            }
            if !spans.is_empty() {
                spans.push(separator.clone());
            }
            spans.extend(line.spans.iter().cloned());
        }
    }
    savvagent_plugin::StyledLine { spans }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::{StyledLine, StyledSpan, TextMods, ThemeColor};

    fn span(text: &str) -> StyledSpan {
        StyledSpan {
            text: text.into(),
            fg: None,
            bg: None,
            modifiers: TextMods::default(),
        }
    }

    fn one_span_line(text: &str) -> StyledLine {
        StyledLine {
            spans: vec![span(text)],
        }
    }

    fn sep() -> StyledSpan {
        StyledSpan {
            text: " · ".into(),
            fg: Some(ThemeColor::Muted),
            bg: None,
            modifiers: TextMods::default(),
        }
    }

    fn joined(line: &StyledLine) -> String {
        line.spans.iter().map(|s| s.text.clone()).collect()
    }

    #[test]
    fn compose_footer_all_three_groups_populated() {
        let l = vec![one_span_line("Anthropic")];
        let c = vec![one_span_line("idle")];
        let r = vec![one_span_line("cwd")];
        let out = compose_footer_line([&l, &c, &r], &sep());
        assert_eq!(joined(&out), "Anthropic · idle · cwd");
    }

    #[test]
    fn compose_footer_only_right_has_no_leading_separator() {
        let empty: Vec<StyledLine> = vec![];
        let r = vec![one_span_line("cwd")];
        let out = compose_footer_line([&empty, &empty, &r], &sep());
        assert_eq!(joined(&out), "cwd");
    }

    #[test]
    fn compose_footer_left_and_right_only_single_separator() {
        let l = vec![one_span_line("Anthropic")];
        let empty: Vec<StyledLine> = vec![];
        let r = vec![one_span_line("cwd")];
        let out = compose_footer_line([&l, &empty, &r], &sep());
        assert_eq!(joined(&out), "Anthropic · cwd");
    }

    #[test]
    fn compose_footer_empty_spans_line_treated_as_no_content() {
        let l = vec![StyledLine { spans: vec![] }];
        let c = vec![one_span_line("idle")];
        let r = vec![one_span_line("cwd")];
        let out = compose_footer_line([&l, &c, &r], &sep());
        assert_eq!(joined(&out), "idle · cwd");
    }

    #[test]
    fn compose_footer_all_groups_empty_returns_empty_line() {
        let empty: Vec<StyledLine> = vec![];
        let out = compose_footer_line([&empty, &empty, &empty], &sep());
        assert!(out.spans.is_empty());
    }

    #[test]
    fn compose_footer_multiple_contributors_share_a_slot_with_separators() {
        // Two plugins both contributing to `home.footer.left` flow as
        // peers, separated like any other groups.
        let l = vec![one_span_line("Anthropic"), one_span_line("Local")];
        let c = vec![one_span_line("idle")];
        let empty: Vec<StyledLine> = vec![];
        let out = compose_footer_line([&l, &c, &empty], &sep());
        assert_eq!(joined(&out), "Anthropic · Local · idle");
    }

    #[test]
    fn compose_footer_skips_empty_lines_within_a_group() {
        let l = vec![StyledLine { spans: vec![] }, one_span_line("Anthropic")];
        let c = vec![one_span_line("idle")];
        let empty: Vec<StyledLine> = vec![];
        let out = compose_footer_line([&l, &c, &empty], &sep());
        assert_eq!(joined(&out), "Anthropic · idle");
    }

    #[test]
    fn compose_footer_preserves_intra_line_spans() {
        // A single contributor emitting multiple spans (e.g. the
        // home_footer right slot's `cwd · ~N ctx · $0.00 · vX.Y.Z`)
        // must not gain extra separators between its own spans.
        let r = vec![StyledLine {
            spans: vec![
                span("cwd"),
                span(" · "),
                span("~22 ctx"),
                span(" · "),
                span("$0.00"),
            ],
        }];
        let empty: Vec<StyledLine> = vec![];
        let out = compose_footer_line([&empty, &empty, &r], &sep());
        assert_eq!(joined(&out), "cwd · ~22 ctx · $0.00");
    }
}
