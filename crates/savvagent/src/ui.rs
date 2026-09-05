//! Render pass: paint the current [`App`] state into the frame.

use crate::app::{App, Entry, InputMode, TranscriptEntry, log_scroll_y};
use crate::palette::Palette;
use crate::providers::effective_providers;
use crate::splash;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, FrameExt, List, ListItem, Padding, Paragraph, Wrap,
    },
};
use savvagent_host::ToolCallStatus;
use savvagent_plugin::ContentBlockId;

/// Rows reserved in the conversation paragraph for each `Entry::Canvas`
/// placeholder. The image (or source-code fallback) is overlaid on top of
/// these blank rows after the paragraph renders. Phase 1 uses a fixed
/// height; Phase 2 will measure document height from the renderer.
const CANVAS_RESERVED_ROWS: u16 = 12;

/// Pre-rendered styled spans for a single `Entry::Tool` row, produced during
/// `compute_home_frame_data`. Stored on `HomeFrameData` and consumed by the
/// sync `render_log` so the render path never locks plugin mutexes.
#[derive(Debug, Clone)]
pub struct ToolEntryRender {
    /// Spans for the arguments line.
    pub arg_spans: Vec<savvagent_plugin::StyledSpan>,
    /// Spans for the result line; `None` while the tool call is in flight.
    pub result_spans: Option<Vec<savvagent_plugin::StyledSpan>>,
}

/// Pre-computed plugin slot output for one render frame. Built async from
/// `compute_home_frame_data` before `terminal.draw` runs so the draw closure
/// stays synchronous and never touches plugin mutexes.
pub struct HomeFrameData {
    pub banner: Vec<savvagent_plugin::StyledLine>,
    pub tips: Vec<savvagent_plugin::StyledLine>,
    pub footer_left: Vec<savvagent_plugin::StyledLine>,
    pub footer_center: Vec<savvagent_plugin::StyledLine>,
    pub footer_right: Vec<savvagent_plugin::StyledLine>,
    /// One entry per `Entry::Tool` in `app.entries`, in order. The Nth
    /// `Entry::Tool` encountered while iterating `app.entries` maps to the
    /// Nth element of this vec.
    pub tool_entries: Vec<ToolEntryRender>,
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
            tool_entries: vec![],
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

    // The registry and index read-locks (`reg_guard`/`idx_guard`) are
    // dropped here so write-lock waiters (e.g. `/connect` while a screen
    // is open) are not blocked by the tool-summary loop.
    drop(reg_guard);
    drop(idx_guard);

    let tool_entries = compute_tool_entries(
        &app.entries,
        app.plugin_indexes.as_ref().cloned(),
        app.plugin_registry.as_ref().cloned(),
    )
    .await;

    HomeFrameData {
        banner,
        tips,
        footer_left,
        footer_center,
        footer_right,
        tool_entries,
    }
}

/// Compute `ToolEntryRender`s for every `Entry::Tool` in `entries` by
/// asking the plugin registry for its tool-summary router. Passed cloned
/// `Arc<RwLock<...>>` handles instead of `&RwLockReadGuard` so the
/// caller (compute_home_frame_data) can drop its guards before this
/// long-running loop. No-op when either handle is `None` (e.g. plugin
/// runtime not installed).
async fn compute_tool_entries(
    entries: &[Entry],
    plugin_indexes: Option<std::sync::Arc<tokio::sync::RwLock<crate::plugin::manifests::Indexes>>>,
    plugin_registry: Option<
        std::sync::Arc<tokio::sync::RwLock<crate::plugin::registry::PluginRegistry>>,
    >,
) -> Vec<ToolEntryRender> {
    let (Some(reg_handle), Some(idx_handle)) = (plugin_registry, plugin_indexes) else {
        return Vec::new();
    };
    let tool_router = crate::plugin::tool_summaries::ToolSummaryRouter::new(
        idx_handle.clone(),
        reg_handle.clone(),
    );
    let mut tool_entries: Vec<ToolEntryRender> = Vec::new();
    for entry in entries {
        let crate::app::Entry::Tool {
            name,
            args,
            result_text,
            ..
        } = entry
        else {
            continue;
        };
        let arg_spans = match tool_router.summarize_call(name, args).await {
            Some(spans) => spans,
            None => savvagent_plugin::styled::json_spans(args),
        };
        let result_spans = match result_text {
            None => None,
            Some(text) => {
                let spans = match tool_router.summarize_result(name, text).await {
                    Some(spans) => spans,
                    None => match serde_json::from_str::<serde_json::Value>(text) {
                        Ok(v) => savvagent_plugin::styled::json_spans(&v),
                        Err(_) => vec![savvagent_plugin::StyledSpan {
                            text: text.clone(),
                            fg: Some(savvagent_plugin::ThemeColor::Muted),
                            bg: None,
                            modifiers: savvagent_plugin::TextMods::default(),
                        }],
                    },
                };
                Some(spans)
            }
        };
        tool_entries.push(ToolEntryRender {
            arg_spans,
            result_spans,
        });
    }
    tool_entries
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

    let canvas_overlays = render_log(app, frame, chunks[1], palette, frame_data);
    // Persist each canvas's on-screen cell rect for the mouse handler in
    // `main.rs::run_app`, which hit-tests clicks outside the render pass.
    // Refreshed every frame so stale rects (e.g. after scroll) never route
    // a click into the wrong block.
    app.canvas_click_targets = canvas_overlays.iter().map(|o| (o.id, o.area)).collect();
    // After the conversation log paints, overlay any Entry::Canvas blocks
    // (image protocol when supported, source-code fallback otherwise).
    // `canvas_overlays` carries each placeholder's on-screen rect (already
    // clipped to the conversation block's inner area).
    if !canvas_overlays.is_empty() {
        render_canvas_overlays(app, frame, palette, &canvas_overlays);
    }

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
            paint_screen(frame, area, chunks[4].y, top_screen, layout, palette);
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
        let items: Vec<ListItem> = effective_providers()
            .into_iter()
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

/// One overlay region computed during the conversation-log render pass.
/// `id` keys into [`crate::app::CanvasRegistry`] for the source/renderer;
/// `area` is the on-screen rect (already clipped to `inner_area`) where
/// the image or fallback should paint. `streaming` is `true` when the
/// canvas is still receiving `HtmlSourceDelta`s — Task 17 turns this into
/// the live-source preview path; Task 16 reuses the same branch as the
/// "no image protocol" fallback so the rectangle is never blank.
#[derive(Debug, Clone)]
struct CanvasOverlay {
    id: ContentBlockId,
    area: Rect,
    streaming: bool,
    source: String,
}

fn render_log(
    app: &App,
    frame: &mut Frame,
    area: Rect,
    palette: Palette,
    frame_data: &HomeFrameData,
) -> Vec<CanvasOverlay> {
    /// Tracks each canvas's first line index in `lines` plus the metadata
    /// the overlay pass needs to draw it. We resolve the on-screen rect
    /// at the bottom of this function once `inner_area.width` and the
    /// scroll offset are known.
    struct CanvasMark {
        id: ContentBlockId,
        line_idx: usize,
        streaming: bool,
        source: String,
    }
    let mut canvas_marks: Vec<CanvasMark> = Vec::new();
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(app.entries.len() * 2 + 1);
    let mut tool_entry_idx: usize = 0;
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
                name: _,
                args: _,
                status,
                result_text: _,
            } => {
                debug_assert!(
                    tool_entry_idx < frame_data.tool_entries.len(),
                    "tool_entries index out of bounds — stale frame data (idx={tool_entry_idx}, len={})",
                    frame_data.tool_entries.len()
                );
                let render = frame_data
                    .tool_entries
                    .get(tool_entry_idx)
                    .cloned()
                    .unwrap_or(ToolEntryRender {
                        arg_spans: vec![],
                        result_spans: None,
                    });
                tool_entry_idx += 1;

                let badge = match status {
                    None => "…",
                    Some(ToolCallStatus::Ok) => "✓",
                    Some(ToolCallStatus::Errored) => "✗",
                };
                let badge_color = match status {
                    None => palette.warning,
                    Some(ToolCallStatus::Ok) => palette.success,
                    Some(ToolCallStatus::Errored) => palette.error,
                };

                // Arguments line: badge prefix + pre-rendered styled spans.
                let mut arg_line_spans: Vec<Span<'static>> = vec![Span::styled(
                    format!("  {badge} "),
                    palette.base_style().fg(badge_color),
                )];
                for s in render.arg_spans {
                    arg_line_spans
                        .push(crate::plugin::convert::styled_span_to_ratatui(s, &palette));
                }
                lines.push(Line::from(arg_line_spans));

                // Result line (if any): pre-rendered styled spans, indented.
                if let Some(result_spans) = render.result_spans {
                    let mut result_line_spans: Vec<Span<'static>> = vec![Span::styled(
                        "    → ".to_string(),
                        palette.base_style().fg(palette.muted),
                    )];
                    for s in result_spans {
                        result_line_spans
                            .push(crate::plugin::convert::styled_span_to_ratatui(s, &palette));
                    }
                    lines.push(Line::from(result_line_spans));
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
            // Canvas entries reserve a fixed block of blank rows in the
            // paragraph so the dedicated overlay pass (see
            // `render_canvas_overlays`) has space to draw the image or
            // source-code fallback at the right vertical position. The
            // first reserved row carries a one-line caption that stays
            // visible even when overlay rendering is unavailable (e.g.,
            // mid-frame during a terminal resize).
            Entry::Canvas {
                id,
                source,
                source_preview,
            } => {
                let caption = if source_preview.is_some() {
                    format!("⬛ canvas:{} — streaming source…", id.0)
                } else {
                    format!("⬛ canvas:{}", id.0)
                };
                let start_idx = lines.len();
                lines.push(Line::from(Span::styled(
                    caption,
                    palette.base_style().fg(palette.muted),
                )));
                // CANVAS_RESERVED_ROWS - 1 blank rows below the caption.
                for _ in 1..CANVAS_RESERVED_ROWS {
                    lines.push(Line::from(""));
                }
                canvas_marks.push(CanvasMark {
                    id: *id,
                    line_idx: start_idx,
                    streaming: source_preview.is_some(),
                    source: source_preview.clone().unwrap_or_else(|| source.clone()),
                });
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

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.border).bg(palette.bg))
        .padding(Padding::new(2, 2, 1, 1))
        .title(Line::styled(
            " Conversation ",
            palette.base_style().fg(palette.fg),
        ));
    // `inner_area` excludes the border + padding, so `line_count(width)` and
    // `area.height` agree on the same coordinate space.
    let inner_area = block.inner(area);

    // Pre-compute the canvas overlay positions BEFORE moving `lines` into
    // `Paragraph::new`. We need each placeholder's wrapped-row offset from
    // the top of the paragraph, and the simple way to get that is to walk
    // the line vector once at the same `inner_area.width` the paragraph
    // will wrap at.
    let inner_width = inner_area.width as usize;
    let line_rows: Vec<usize> = lines
        .iter()
        .map(|line| wrapped_row_count(line.width(), inner_width))
        .collect();
    let mut cum: Vec<usize> = Vec::with_capacity(line_rows.len());
    let mut running = 0usize;
    for n in &line_rows {
        cum.push(running);
        running += *n;
    }
    let total_wrapped_rows = running;

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .style(palette.base_style());

    // Auto-tail by default: scroll so the LAST wrapped line lands on the
    // bottom row of `inner_area`. Ratatui's Paragraph renders top-down, so
    // without a scroll offset newly-streamed text falls off the bottom and
    // becomes invisible. See `log_scroll_y` for the cascade.
    //
    // We use our own `total_wrapped_rows` rather than
    // `para.line_count(inner_area.width)` so the auto-tail and the canvas
    // overlay placement below agree on the same row coordinate system.
    // Ratatui's `Wrap { trim: false }` word-wraps at whitespace; our
    // `wrapped_row_count` divides by cell width. The two can disagree by
    // a few rows when very long Assistant lines word-wrap. Keeping both
    // sides on the same approximation avoids a visual mismatch between
    // where the canvas placeholder paints and where the overlay draws —
    // at the cost of slightly imprecise auto-tail for pathologically
    // long unwrapped lines.
    let scroll_y = log_scroll_y(
        total_wrapped_rows,
        inner_area.height as usize,
        app.log_scroll_offset_from_bottom,
    );

    frame.render_widget(para.scroll((scroll_y, 0)).block(block), area);

    // Resolve each canvas placeholder's on-screen rect using the cumulative
    // wrapped-row offsets computed above. Emit a `CanvasOverlay` for every
    // placeholder whose top row lands in the visible band
    // `[scroll_y, scroll_y + inner_area.height)`. Rects are clipped to
    // `inner_area` so the overlay never bleeds into the bordered block's
    // chrome.
    if canvas_marks.is_empty() || inner_area.width == 0 || inner_area.height == 0 {
        return Vec::new();
    }

    let viewport_top = scroll_y as usize;
    let viewport_bottom = viewport_top.saturating_add(inner_area.height as usize);

    let mut overlays: Vec<CanvasOverlay> = Vec::with_capacity(canvas_marks.len());
    for mark in canvas_marks {
        let placeholder_top = cum.get(mark.line_idx).copied().unwrap_or(0);
        // Each placeholder occupies CANVAS_RESERVED_ROWS contiguous blank
        // rows (1-row caption + N-1 blank rows), each of which wraps to
        // exactly one screen row because `Line::from("")` has width 0.
        let placeholder_bottom = placeholder_top + CANVAS_RESERVED_ROWS as usize;

        // Skip canvases entirely outside the viewport.
        if placeholder_bottom <= viewport_top || placeholder_top >= viewport_bottom {
            continue;
        }

        // Clip the placeholder rect to the visible band.
        let visible_top = placeholder_top.max(viewport_top);
        let visible_bottom = placeholder_bottom.min(viewport_bottom);
        let on_screen_y = (visible_top - viewport_top) as u16;
        let height = (visible_bottom - visible_top) as u16;
        if height == 0 {
            continue;
        }

        let rect = Rect {
            x: inner_area.x,
            y: inner_area.y + on_screen_y,
            width: inner_area.width,
            height,
        };

        overlays.push(CanvasOverlay {
            id: mark.id,
            area: rect,
            streaming: mark.streaming,
            source: mark.source,
        });
    }
    overlays
}

/// Wrapped-row count for a single logical line of visual width `w` at
/// `wrap_width`. Empty lines and 0-width wrap_widths collapse to 1 row.
fn wrapped_row_count(w: usize, wrap_width: usize) -> usize {
    if wrap_width == 0 {
        return 1;
    }
    if w == 0 {
        return 1;
    }
    w.div_ceil(wrap_width)
}

/// Paint each `Entry::Canvas` overlay over the conversation log. Three
/// branches:
///
/// 1. **Streaming.** `overlay.streaming` is `true` — the host is still
///    accumulating `HtmlSourceDelta`s. Render the partial source as a
///    code block with a "rendering…" hint. Phase 1 reuses the same path
///    as the no-image-protocol fallback so something always paints.
/// 2. **Image protocol available, complete source.** Drive the renderer,
///    convert the resulting `Frame` to a `StatefulProtocol`, and hand
///    `StatefulImage` to `render_stateful_widget`. The protocol is cached
///    on `CanvasRegistry::image_states`; the stateful widget re-encodes
///    internally when the area changes.
/// 3. **No image protocol.** Same code-block fallback as (1), with a
///    one-line yellow banner explaining the situation.
///
/// `overlay.area` is already clipped to the conversation block's inner
/// area, so any of the above paint inside the bordered "Conversation" box
/// without bleeding into the chrome.
fn render_canvas_overlays(
    app: &mut App,
    frame: &mut Frame,
    palette: Palette,
    overlays: &[CanvasOverlay],
) {
    for overlay in overlays {
        if overlay.area.width == 0 || overlay.area.height == 0 {
            continue;
        }
        // Focus chrome: when this canvas is the focused element, paint a
        // 1-cell accent border around the overlay and render the content
        // into the block's inner area. Unfocused canvases render as before.
        let focused = app.is_canvas_focused(overlay.id);
        let content_area = canvas_content_area(overlay.area, focused);
        if focused {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(palette.base_style().fg(palette.accent));
            frame.render_widget(block, overlay.area);
        }
        // A 1-cell shrink on a thin overlay can collapse the inner area to
        // zero — skip content rendering then (matches Phase 1's small-area
        // behavior), leaving just the border.
        if content_area.width == 0 || content_area.height == 0 {
            continue;
        }
        if overlay.streaming {
            render_source_preview(frame, content_area, &overlay.source, palette);
            continue;
        }
        if app.canvas_registry.image_protocol_available() {
            render_canvas_image(
                frame,
                content_area,
                app,
                overlay.id,
                palette,
                &overlay.source,
            );
        } else {
            render_canvas_source_fallback(frame, content_area, &overlay.source, palette);
        }
    }
}

/// Inner area available for canvas content given the overlay rect and focus
/// state. A focused canvas reserves 1 cell on every side for its accent
/// border (via `Block::inner`); an unfocused canvas uses the full rect. The
/// shrink is saturating, so a tiny focused overlay collapses to a zero-size
/// area rather than panicking — callers must guard against that.
fn canvas_content_area(area: Rect, focused: bool) -> Rect {
    if !focused {
        return area;
    }
    Block::default().borders(Borders::ALL).inner(area)
}

/// Drive the canvas renderer at `area`'s pixel width and overlay the
/// resulting image. Falls back to the source-code path on any failure
/// (no renderer registered, malformed frame, etc.) so the user still
/// sees the HTML they asked the model for.
fn render_canvas_image(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    id: ContentBlockId,
    palette: Palette,
    source: &str,
) {
    // Pixel width = cell width * picker-reported font width.
    let cell_w = match app.canvas_registry.image_cell_size() {
        Some((w, _h)) => w,
        None => {
            // Defensive: image_protocol_available() returned true above,
            // but threads-of-control change could in theory flip it.
            render_canvas_source_fallback(frame, area, source, palette);
            return;
        }
    };
    let pixel_width = (area.width as u32).saturating_mul(cell_w as u32);
    if pixel_width == 0 {
        render_canvas_source_fallback(frame, area, source, palette);
        return;
    }

    // Clear the overlay rect so any stale text (placeholder caption or
    // adjacent paragraph) doesn't bleed through the rendered image.
    frame.render_widget(Clear, area);
    frame.buffer_mut().set_style(area, palette.base_style());

    let protocol = app.canvas_registry.image_protocol_mut(id, pixel_width);
    match protocol {
        Some(state) => {
            let widget =
                ratatui_image::StatefulImage::<ratatui_image::protocol::StatefulProtocol>::default(
                );
            frame.render_stateful_widget(widget, area, state);
        }
        None => {
            // Renderer missing or frame empty/mis-sized — show the source
            // so the user still sees what the model emitted.
            render_canvas_source_fallback(frame, area, source, palette);
        }
    }
}

/// Source-code fallback used when the terminal lacks an image protocol.
/// Top row is a yellow banner naming the supported terminals; the rest of
/// the area renders the HTML source inside a left-bar "code block".
fn render_canvas_source_fallback(frame: &mut Frame, area: Rect, source: &str, palette: Palette) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    frame.render_widget(Clear, area);
    frame.buffer_mut().set_style(area, palette.base_style());

    let banner = Paragraph::new(
        "Inline HTML rendering requires kitty / WezTerm / Ghostty / iTerm2 / Sixel.",
    )
    .style(palette.base_style().fg(palette.warning))
    .wrap(Wrap { trim: false });
    let (banner_area, body_area) = split_top_one_line(area);
    frame.render_widget(banner, banner_area);
    if body_area.height > 0 {
        render_code_block(frame, body_area, source, palette);
    }
}

/// Streaming preview: same code-block helper plus a muted "rendering…"
/// header. Task 17 will swap this for a syntax-highlighted variant; the
/// header keeps the user-facing affordance stable.
fn render_source_preview(frame: &mut Frame, area: Rect, preview: &str, palette: Palette) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    frame.render_widget(Clear, area);
    frame.buffer_mut().set_style(area, palette.base_style());

    let header =
        Paragraph::new("Rendering HTML canvas…").style(palette.base_style().fg(palette.muted));
    let (header_area, body_area) = split_top_one_line(area);
    frame.render_widget(header, header_area);
    if body_area.height > 0 {
        render_code_block(frame, body_area, preview, palette);
    }
}

/// Minimal "code block" widget: muted text inside a left-border bar.
/// Phase 2 will plumb syntect-driven syntax highlighting through; Phase 1
/// keeps the surface flat to avoid dragging in another renderer dep.
fn render_code_block(frame: &mut Frame, area: Rect, source: &str, palette: Palette) {
    let widget = Paragraph::new(source.to_string())
        .wrap(Wrap { trim: false })
        .style(palette.base_style().fg(palette.fg))
        .block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(palette.base_style().fg(palette.border)),
        );
    frame.render_widget(widget, area);
}

/// Split `area` into a 1-row top strip and the remainder. When `area` is
/// 1 row tall the top strip takes the whole rect and the body has height 0.
fn split_top_one_line(area: Rect) -> (Rect, Rect) {
    if area.height == 0 {
        return (area, area);
    }
    let top = Rect {
        height: 1.min(area.height),
        ..area
    };
    let body = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    (top, body)
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
/// For `Fullscreen`, content fills the computed area directly.
/// For `BottomSheet`, content is anchored directly above the prompt
/// textarea (`input_top`) rather than the bottom of the whole terminal —
/// this is the inline `/`-command-palette-style overlay: the input row
/// stays visible immediately below the sheet instead of being covered.
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

/// Place a `BottomSheet` of `height` rows inside `area`, anchored so its
/// bottom edge meets the top of the prompt textarea (`input_top`) rather
/// than the bottom of the whole frame — the inline `/`-palette look, with
/// the input row left visible immediately below the sheet.
///
/// The height is clamped to the space that actually exists *above* the
/// prompt. Without that clamp a short terminal (the home layout's minimum
/// is header 3 + log 1 + banner 1 + tips 1 + input 3 + footer 1, so a
/// 15-row terminal puts `input_top` at 11) would pin the sheet's top at
/// `area.y` while keeping its full height, growing it back down over the
/// header and the textarea — exactly what the anchoring exists to avoid.
fn bottom_sheet_rect(area: Rect, input_top: u16, height: u16) -> Rect {
    let bottom = input_top.max(area.y);
    let h = height.min(bottom.saturating_sub(area.y)).min(area.height);
    Rect::new(area.x, bottom.saturating_sub(h), area.width, h)
}

fn paint_screen(
    f: &mut Frame,
    area: Rect,
    input_top: u16,
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
            let sheet = bottom_sheet_rect(area, input_top, *height);
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
    fn canvas_content_area_unfocused_is_unchanged() {
        let area = Rect {
            x: 3,
            y: 4,
            width: 20,
            height: 8,
        };
        assert_eq!(canvas_content_area(area, false), area);
    }

    #[test]
    fn canvas_content_area_focused_shrinks_by_one_cell_each_side() {
        let area = Rect {
            x: 3,
            y: 4,
            width: 20,
            height: 8,
        };
        let inner = canvas_content_area(area, true);
        assert_eq!(
            inner,
            Rect {
                x: 4,
                y: 5,
                width: 18,
                height: 6,
            }
        );
    }

    #[test]
    fn canvas_content_area_focused_tiny_area_collapses_without_panic() {
        // 1x1 overlay: the 1-cell border consumes the whole rect, leaving a
        // zero-size inner area (callers must guard against this).
        let area = Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        let inner = canvas_content_area(area, true);
        assert_eq!(inner.width, 0);
        assert_eq!(inner.height, 0);
    }

    #[test]
    fn focused_canvas_block_draws_a_border() {
        use ratatui::buffer::Buffer;
        use ratatui::widgets::Widget;

        let area = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 4,
        };
        let mut buf = Buffer::empty(area);
        // Mirror the focus-chrome block the overlay path renders.
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .render(area, &mut buf);
        let has_border = buf
            .content()
            .iter()
            .any(|c| matches!(c.symbol(), "┌" | "┐" | "└" | "┘" | "│" | "─"));
        assert!(has_border, "focused canvas should draw a border");
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

    // Canvas overlay row math --------------------------------------------

    #[test]
    fn wrapped_row_count_empty_line_is_one_row() {
        assert_eq!(wrapped_row_count(0, 80), 1);
    }

    #[test]
    fn wrapped_row_count_short_line_is_one_row() {
        assert_eq!(wrapped_row_count(10, 80), 1);
    }

    #[test]
    fn wrapped_row_count_exact_width_is_one_row() {
        assert_eq!(wrapped_row_count(80, 80), 1);
    }

    #[test]
    fn wrapped_row_count_overflow_by_one_yields_two_rows() {
        assert_eq!(wrapped_row_count(81, 80), 2);
    }

    #[test]
    fn wrapped_row_count_two_full_widths_plus_one_yields_three() {
        assert_eq!(wrapped_row_count(161, 80), 3);
    }

    #[test]
    fn wrapped_row_count_zero_wrap_width_collapses_to_one_row() {
        // Defensive: a 0-width inner area happens during terminal resize
        // edge cases. Returning 1 keeps the cumulative sum monotonic and
        // avoids divide-by-zero downstream.
        assert_eq!(wrapped_row_count(100, 0), 1);
    }

    #[test]
    fn split_top_one_line_splits_a_tall_rect() {
        let r = Rect::new(2, 3, 80, 10);
        let (top, body) = split_top_one_line(r);
        assert_eq!(top, Rect::new(2, 3, 80, 1));
        assert_eq!(body, Rect::new(2, 4, 80, 9));
    }

    #[test]
    fn split_top_one_line_with_one_row_leaves_no_body() {
        let r = Rect::new(0, 0, 40, 1);
        let (top, body) = split_top_one_line(r);
        assert_eq!(top.height, 1);
        assert_eq!(body.height, 0);
    }

    #[test]
    fn split_top_one_line_with_zero_rows_returns_unchanged() {
        let r = Rect::new(0, 0, 40, 0);
        let (top, body) = split_top_one_line(r);
        assert_eq!(top, r);
        assert_eq!(body, r);
    }

    /// A bottom sheet sits directly above the prompt, not at the bottom of
    /// the frame: on a roomy terminal the full requested height fits and the
    /// sheet's last row is the one immediately above `input_top`.
    #[test]
    fn bottom_sheet_is_anchored_above_the_prompt() {
        // 40-row frame, 3-row prompt + 1-row footer => input_top = 36.
        let area = Rect::new(0, 0, 100, 40);
        let sheet = bottom_sheet_rect(area, 36, 12);
        assert_eq!(sheet, Rect::new(0, 24, 100, 12));
        assert_eq!(sheet.y + sheet.height, 36, "must stop at the prompt");
    }

    /// Regression: on a short terminal there is less room above the prompt
    /// than the sheet asks for, and the sheet must shrink rather than grow
    /// back down over the header and the textarea.
    #[test]
    fn bottom_sheet_shrinks_instead_of_covering_the_prompt() {
        // 15-row frame: header 3 + log 1 + banner 1 + tips 1 + input 3 +
        // footer 1 leaves input_top = 11, well under the requested 12.
        let area = Rect::new(0, 0, 100, 15);
        let sheet = bottom_sheet_rect(area, 11, 12);
        assert_eq!(sheet.y, 0, "clamped sheet starts at the top of the area");
        assert_eq!(sheet.height, 11);
        assert_eq!(
            sheet.y + sheet.height,
            11,
            "sheet must never extend past input_top"
        );
    }

    /// The sheet is positioned relative to `area`, not the screen origin.
    #[test]
    fn bottom_sheet_respects_a_nonzero_area_origin() {
        let area = Rect::new(4, 5, 60, 30);
        let sheet = bottom_sheet_rect(area, 30, 8);
        assert_eq!(sheet, Rect::new(4, 22, 60, 8));
    }

    /// Degenerate case: the prompt is at the very top of the area, so there
    /// is no room at all. An empty sheet is fine; an overlapping one is not.
    #[test]
    fn bottom_sheet_with_no_room_above_the_prompt_is_empty() {
        let area = Rect::new(0, 7, 80, 20);
        let sheet = bottom_sheet_rect(area, 7, 12);
        assert_eq!(sheet.height, 0);
        assert_eq!(sheet.y, 7);
    }
}
