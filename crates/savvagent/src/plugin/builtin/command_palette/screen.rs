//! Filterable list-of-commands modal.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};

/// One row in the palette: a slash command name, its summary, and whether
/// it requires an argument (selecting a `needs_arg == true` row prefills
/// the textarea with `"/cmd "` instead of dispatching the slash immediately).
#[derive(Debug, Clone)]
pub struct PaletteCommand {
    /// Slash command name without the leading `/`.
    pub name: String,
    /// One-line summary from the plugin's `SlashSpec.summary`.
    pub description: String,
    /// `true` if the command's `SlashSpec.requires_arg` is set — the
    /// palette then prefills the textarea with `"/cmd "` on selection
    /// instead of dispatching the slash with empty args.
    pub needs_arg: bool,
}

/// Modal screen that lets the user filter and run slash commands by name.
///
/// The command list is populated by `apply_effects::open_screen` from the
/// runtime's [`crate::plugin::manifests::Indexes::slash`] map and each
/// owning plugin's manifest — so disabled plugins' slashes don't appear,
/// and new plugins are picked up without touching this file.
///
/// On `Enter`:
/// - `needs_arg == true` rows emit `Stack([CloseScreen, PrefillInput])`
///   so the user can complete the slash (typically via the `@` file picker).
/// - Other rows emit `Stack([CloseScreen, RunSlash])`.
pub struct PaletteScreen {
    filter: String,
    cursor: usize,
    commands: Vec<PaletteCommand>,
}

impl PaletteScreen {
    /// Empty palette with no rows; only useful before
    /// `apply_effects::open_screen` replaces it with a populated screen
    /// via [`Self::with_commands`].
    pub fn empty() -> Self {
        Self {
            filter: String::new(),
            cursor: 0,
            commands: Vec::new(),
        }
    }

    /// Populate the palette with `commands` (already sorted by the caller).
    pub fn with_commands(commands: Vec<PaletteCommand>) -> Self {
        Self {
            filter: String::new(),
            cursor: 0,
            commands,
        }
    }

    fn filtered(&self) -> Vec<(usize, &PaletteCommand)> {
        let f = self.filter.to_ascii_lowercase();
        self.commands
            .iter()
            .enumerate()
            .filter(|(_, c)| c.name.contains(&f))
            .collect()
    }
}

impl Default for PaletteScreen {
    fn default() -> Self {
        Self::empty()
    }
}

#[async_trait]
impl Screen for PaletteScreen {
    fn id(&self) -> String {
        "palette".to_string()
    }

    fn render(&self, region: Region) -> Vec<StyledLine> {
        let mut lines = vec![StyledLine::plain(format!("> {}", self.filter))];
        if self.commands.is_empty() {
            lines.push(StyledLine::plain(""));
            lines.push(StyledLine {
                spans: vec![StyledSpan {
                    text: rust_i18n::t!("picker.command-palette.no-commands").to_string(),
                    fg: Some(ThemeColor::Muted),
                    bg: None,
                    modifiers: TextMods::default(),
                }],
            });
            return lines;
        }
        // Align description column across rows by padding the slash-name
        // span to the widest name in the filtered list (with a 12-char
        // floor + 2 cols of breathing room). Without this, names longer
        // than the old fixed `{:<12}` width — `/connect anthropic`,
        // `/connect gemini`, … — collide with their descriptions.
        let filtered = self.filtered();
        let name_col_width = filtered
            .iter()
            .map(|(_, c)| c.name.chars().count())
            .max()
            .unwrap_or(0)
            .max(12)
            + 2;
        // The list lives in a fixed-height `BottomSheet`, so a long,
        // unfiltered command set (30+ builtins) won't fit. Window the
        // rows around the cursor, budgeting *three* of the region's rows
        // for non-command chrome: the `> filter` line (already pushed),
        // the spacer/scroll-hint line, and the sheet's last row — which
        // the host overpaints with our `tips()` after the paragraph
        // (`ui.rs::paint_screen`). Reserving only two would put a row we
        // still drew underneath the tips line, and since the window is
        // anchored so the cursor sits on its *last* row that hidden row
        // is the selected one: the `▶` highlight would vanish for every
        // scrolled list. Then show a scroll hint in place of the usual
        // blank spacer row whenever the window doesn't reach an edge.
        let capacity = (region.height as usize).saturating_sub(3).max(1);
        let window_start = if filtered.len() <= capacity {
            0
        } else {
            (self.cursor + 1).saturating_sub(capacity)
        };
        let window_end = (window_start + capacity).min(filtered.len());

        let hidden_above = window_start;
        let hidden_below = filtered.len() - window_end;
        // Only name the side that actually has hidden rows: at either end
        // of a long list the other count is zero, and "↑0 more above" is
        // noise rather than information.
        let hint = match (hidden_above, hidden_below) {
            (0, 0) => String::new(),
            (0, below) => format!("  ↓{below} more below"),
            (above, 0) => format!("  ↑{above} more above"),
            (above, below) => format!("  ↑{above} more above · ↓{below} more below"),
        };
        if hint.is_empty() {
            lines.push(StyledLine::plain(""));
        } else {
            lines.push(StyledLine {
                spans: vec![StyledSpan {
                    text: hint,
                    fg: Some(ThemeColor::Muted),
                    bg: None,
                    modifiers: TextMods::default(),
                }],
            });
        }
        for (visual_idx, (_, cmd)) in filtered[window_start..window_end]
            .iter()
            .enumerate()
            .map(|(i, c)| (window_start + i, c))
        {
            let marker = if visual_idx == self.cursor {
                "▶ "
            } else {
                "  "
            };
            let name_with_slash = format!("/{}", cmd.name);
            let pad_count = name_col_width.saturating_sub(name_with_slash.chars().count());
            let padding = " ".repeat(pad_count);
            lines.push(StyledLine {
                spans: vec![
                    StyledSpan {
                        text: format!("{marker}{name_with_slash}{padding}"),
                        fg: Some(if visual_idx == self.cursor {
                            ThemeColor::Accent
                        } else {
                            ThemeColor::Fg
                        }),
                        bg: None,
                        modifiers: TextMods {
                            bold: visual_idx == self.cursor,
                            ..Default::default()
                        },
                    },
                    StyledSpan {
                        text: cmd.description.clone(),
                        fg: Some(ThemeColor::Muted),
                        bg: None,
                        modifiers: TextMods::default(),
                    },
                ],
            });
        }
        lines
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            KeyCodePortable::Esc => Ok(vec![Effect::CloseScreen]),
            KeyCodePortable::Up => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                Ok(vec![])
            }
            KeyCodePortable::Down => {
                let max = self.filtered().len().saturating_sub(1);
                if self.cursor < max {
                    self.cursor += 1;
                }
                Ok(vec![])
            }
            KeyCodePortable::Backspace => {
                self.filter.pop();
                self.cursor = 0;
                Ok(vec![])
            }
            KeyCodePortable::Char(c) => {
                self.filter.push(c);
                self.cursor = 0;
                Ok(vec![])
            }
            KeyCodePortable::Enter => {
                let filtered = self.filtered();
                let Some((_, cmd)) = filtered.get(self.cursor).cloned() else {
                    return Ok(vec![Effect::CloseScreen]);
                };
                let name = cmd.name.clone();
                if cmd.needs_arg {
                    // Don't fire the slash with empty args (which would
                    // error with "usage: /<cmd> <path>"). Instead, close
                    // the palette and seed the textarea so the user can
                    // complete the line — typically via the `@` file
                    // picker — before pressing Enter.
                    Ok(vec![Effect::Stack(vec![
                        Effect::CloseScreen,
                        Effect::PrefillInput {
                            text: format!("/{name} "),
                        },
                    ])])
                } else {
                    Ok(vec![Effect::Stack(vec![
                        Effect::CloseScreen,
                        Effect::RunSlash { name, args: vec![] },
                    ])])
                }
            }
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            rust_i18n::t!("picker.command-palette.tips").to_string(),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::KeyMods;

    fn key(c: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code: c,
            modifiers: KeyMods::default(),
        }
    }

    fn cmd(name: &str, needs_arg: bool) -> PaletteCommand {
        PaletteCommand {
            name: name.into(),
            description: format!("{name} description"),
            needs_arg,
        }
    }

    fn fixture() -> PaletteScreen {
        // Alphabetically sorted — matches apply_effects::open_screen ordering.
        PaletteScreen::with_commands(vec![
            cmd("clear", false),
            cmd("edit", true),
            cmd("quit", false),
            cmd("theme", false),
            cmd("view", true),
        ])
    }

    #[tokio::test]
    async fn enter_emits_close_then_runslash_for_first_match() {
        let mut p = fixture();
        let effs = p.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match effs.first() {
            Some(Effect::Stack(children)) => {
                assert!(matches!(children[0], Effect::CloseScreen));
                match &children[1] {
                    Effect::RunSlash { name, .. } => assert_eq!(name, "clear"),
                    other => panic!("expected RunSlash, got {other:?}"),
                }
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn typing_filters_and_resets_cursor() {
        let mut p = fixture();
        p.on_key(key(KeyCodePortable::Char('e'))).await.unwrap();
        let filtered = p.filtered();
        assert!(filtered.iter().all(|(_, c)| c.name.contains('e')));
        assert_eq!(p.cursor, 0);
    }

    /// Selecting a `needs_arg` command must seed the textarea with
    /// `"/<name> "` rather than firing the slash with empty args (which
    /// would error out with "usage: /<name> <arg>"). Regression test for
    /// hotfix bug #1.
    #[tokio::test]
    async fn enter_on_needs_arg_command_emits_prefill_not_runslash() {
        let mut p = fixture();
        for ch in "view".chars() {
            p.on_key(key(KeyCodePortable::Char(ch))).await.unwrap();
        }
        let effs = p.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match effs.first() {
            Some(Effect::Stack(children)) => {
                assert!(matches!(children[0], Effect::CloseScreen));
                match &children[1] {
                    Effect::PrefillInput { text } => assert_eq!(text, "/view "),
                    other => panic!("expected PrefillInput, got {other:?}"),
                }
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// `quit` is reachable from the palette (post-v0.9 regression).
    #[tokio::test]
    async fn quit_is_listed_and_runs_via_runslash() {
        let mut p = fixture();
        for ch in "quit".chars() {
            p.on_key(key(KeyCodePortable::Char(ch))).await.unwrap();
        }
        assert!(!p.filtered().is_empty(), "palette should list /quit");
        let effs = p.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match effs.first() {
            Some(Effect::Stack(children)) => {
                assert!(matches!(children[0], Effect::CloseScreen));
                match &children[1] {
                    Effect::RunSlash { name, args } => {
                        assert_eq!(name, "quit");
                        assert!(args.is_empty());
                    }
                    other => panic!("expected RunSlash, got {other:?}"),
                }
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Empty command set renders a placeholder and Enter is a no-op-ish close.
    #[tokio::test]
    async fn empty_palette_renders_placeholder_and_enter_closes() {
        let mut p = PaletteScreen::empty();
        let lines = p.render(Region {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect();
        assert!(
            joined.contains(rust_i18n::t!("picker.command-palette.no-commands").as_ref()),
            "empty render should show placeholder, got: {joined}"
        );
        let effs = p.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn esc_closes() {
        let mut p = fixture();
        let effs = p.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }

    /// The palette renders inside a fixed-height `BottomSheet`, so a
    /// command set that doesn't fit the region must be windowed around
    /// the cursor rather than silently truncated with no indication.
    #[tokio::test]
    async fn long_list_fits_shows_no_scroll_hint() {
        let commands: Vec<_> = (0..5).map(|i| cmd(&format!("cmd{i}"), false)).collect();
        let p = PaletteScreen::with_commands(commands);
        // capacity = height(12) - 3 = 9, which comfortably fits all 5 rows.
        let lines = p.render(Region {
            x: 0,
            y: 0,
            width: 80,
            height: 12,
        });
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect();
        assert!(!joined.contains("more above"));
        assert!(!joined.contains("more below"));
        for i in 0..5 {
            assert!(joined.contains(&format!("/cmd{i}")));
        }
    }

    /// When the filtered list overflows the sheet's capacity, only a
    /// window around the cursor renders, plus a hint showing how many
    /// rows are hidden above/below. Only the non-zero side of the hint is
    /// named — at either end of the list the other count is 0, and
    /// "↑0 more above" would be noise.
    #[tokio::test]
    async fn overflowing_list_windows_around_cursor_with_scroll_hint() {
        let commands: Vec<_> = (0..20).map(|i| cmd(&format!("cmd{i:02}"), false)).collect();
        let mut p = PaletteScreen::with_commands(commands);
        // capacity = height(6) - 3 = 3 visible rows out of 20 commands.
        let region = Region {
            x: 0,
            y: 0,
            width: 80,
            height: 6,
        };

        let lines = p.render(region);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect();
        // Cursor starts at 0: window is [0, 3), nothing hidden above but
        // 17 rows hidden below.
        assert!(joined.contains("/cmd00"));
        assert!(joined.contains("/cmd01"));
        assert!(joined.contains("/cmd02"));
        assert!(!joined.contains("/cmd03"));
        assert!(joined.contains("↓17 more below"));
        assert!(
            !joined.contains("more above"),
            "nothing is hidden above at the top of the list, got: {joined}"
        );

        // Move the cursor to the end; the window should follow it so the
        // selected row is always visible, and hidden-above must update.
        for _ in 0..19 {
            p.on_key(key(KeyCodePortable::Down)).await.unwrap();
        }
        let lines = p.render(region);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect();
        assert!(joined.contains("/cmd19"));
        assert!(joined.contains("↑17 more above"));
        assert!(
            !joined.contains("more below"),
            "nothing is hidden below at the end of the list, got: {joined}"
        );
    }

    /// Regression: the host overpaints the sheet's last row with `tips()`
    /// *after* the paragraph, so `render` must emit at most
    /// `region.height - 1` lines. The window is anchored so the cursor
    /// sits on its last row, which is precisely the row that would be
    /// swallowed — leaving the `▶` selection invisible for the rest of
    /// the list once it scrolls.
    #[tokio::test]
    async fn cursor_row_never_lands_on_the_tips_row() {
        let commands: Vec<_> = (0..30).map(|i| cmd(&format!("cmd{i:02}"), false)).collect();
        let mut p = PaletteScreen::with_commands(commands);
        let region = Region {
            x: 0,
            y: 0,
            width: 80,
            height: 12,
        };

        // Walk the whole list; at no point may the selected row fall on
        // (or past) the row the tips line will claim.
        for _ in 0..30 {
            let lines = p.render(region);
            let visible = region.height as usize - 1; // tips row is not ours
            assert!(
                lines.len() <= visible,
                "render emitted {} lines into {} usable rows",
                lines.len(),
                visible
            );
            let cursor_row = lines
                .iter()
                .position(|l| l.spans.iter().any(|s| s.text.starts_with("▶")))
                .expect("the selected row must always be rendered");
            assert!(
                cursor_row < visible,
                "cursor row {cursor_row} would be overpainted by the tips row"
            );
            p.on_key(key(KeyCodePortable::Down)).await.unwrap();
        }
    }

    /// A filtered list whose length is exactly the capacity must render in
    /// full — the off-by-one that hid the cursor also hid this last row
    /// while reporting `0 more below`.
    #[tokio::test]
    async fn list_exactly_filling_capacity_renders_every_row() {
        // capacity = height(12) - 3 = 9.
        let commands: Vec<_> = (0..9).map(|i| cmd(&format!("cmd{i}"), false)).collect();
        let p = PaletteScreen::with_commands(commands);
        let lines = p.render(Region {
            x: 0,
            y: 0,
            width: 80,
            height: 12,
        });
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect();
        for i in 0..9 {
            assert!(joined.contains(&format!("/cmd{i}")), "missing /cmd{i}");
        }
        assert!(!joined.contains("more below"));
    }
}
