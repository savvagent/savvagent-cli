//! Conversion helpers between `savvagent-plugin` types and ratatui types.
//!
//! These are free functions rather than trait impls to keep the plugin crate
//! free of any ratatui dependency.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use savvagent_plugin::{
    KeyCodePortable, KeyEventPortable, KeyMods, Region, StyledLine, StyledSpan, TextMods,
    ThemeColor,
};

/// Convert crossterm [`KeyModifiers`] into the WIT-portable [`KeyMods`].
/// Shared by key-event and mouse-event translation.
pub fn modifiers_to_portable(m: KeyModifiers) -> KeyMods {
    KeyMods {
        ctrl: m.contains(KeyModifiers::CONTROL),
        alt: m.contains(KeyModifiers::ALT),
        shift: m.contains(KeyModifiers::SHIFT),
        meta: m.contains(KeyModifiers::SUPER),
    }
}

/// Convert a crossterm `KeyEvent` into the WIT-portable shape used by
/// the plugin runtime. Unknown key codes map to `KeyCodePortable::Unknown`.
pub fn key_event_to_portable(e: KeyEvent) -> KeyEventPortable {
    KeyEventPortable {
        code: match e.code {
            KeyCode::Char(c) => KeyCodePortable::Char(c),
            KeyCode::Backspace => KeyCodePortable::Backspace,
            KeyCode::Enter => KeyCodePortable::Enter,
            KeyCode::Esc => KeyCodePortable::Esc,
            KeyCode::Tab => KeyCodePortable::Tab,
            KeyCode::BackTab => KeyCodePortable::BackTab,
            KeyCode::Insert => KeyCodePortable::Insert,
            KeyCode::Delete => KeyCodePortable::Delete,
            KeyCode::Up => KeyCodePortable::Up,
            KeyCode::Down => KeyCodePortable::Down,
            KeyCode::Left => KeyCodePortable::Left,
            KeyCode::Right => KeyCodePortable::Right,
            KeyCode::Home => KeyCodePortable::Home,
            KeyCode::End => KeyCodePortable::End,
            KeyCode::PageUp => KeyCodePortable::PageUp,
            KeyCode::PageDown => KeyCodePortable::PageDown,
            KeyCode::F(n) => KeyCodePortable::F(n),
            KeyCode::Null => KeyCodePortable::Unknown,
            _ => KeyCodePortable::Unknown,
        },
        modifiers: modifiers_to_portable(e.modifiers),
    }
}

/// Convert a `savvagent_plugin::Region` into a `ratatui::layout::Rect`.
#[allow(dead_code)] // used by screen-stack dispatch in PR 3
pub fn region_to_rect(r: Region) -> ratatui::layout::Rect {
    ratatui::layout::Rect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

/// Convert a `ratatui::layout::Rect` into a `savvagent_plugin::Region`.
pub fn rect_to_region(r: ratatui::layout::Rect) -> Region {
    Region {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

/// Map a `ThemeColor` to the corresponding `ratatui::style::Color`.
///
/// Literal ANSI / indexed / RGB variants map 1:1. Semantic variants (`Fg`,
/// `Bg`, `Accent`, `Muted`, `Error`, `Warning`, `Success`, `Secondary`,
/// `Border`) resolve against the supplied [`crate::palette::Palette`] so
/// plugin output adapts to the active theme.
pub fn theme_color_to_ratatui(c: ThemeColor, palette: &crate::palette::Palette) -> Color {
    match c {
        ThemeColor::Default => Color::Reset,
        ThemeColor::Black => Color::Black,
        ThemeColor::Red => Color::Red,
        ThemeColor::Green => Color::Green,
        ThemeColor::Yellow => Color::Yellow,
        ThemeColor::Blue => Color::Blue,
        ThemeColor::Magenta => Color::Magenta,
        ThemeColor::Cyan => Color::Cyan,
        ThemeColor::White => Color::White,
        ThemeColor::DarkGray => Color::DarkGray,
        ThemeColor::LightRed => Color::LightRed,
        ThemeColor::LightGreen => Color::LightGreen,
        ThemeColor::LightYellow => Color::LightYellow,
        ThemeColor::LightBlue => Color::LightBlue,
        ThemeColor::LightMagenta => Color::LightMagenta,
        ThemeColor::LightCyan => Color::LightCyan,
        ThemeColor::Gray => Color::Gray,
        ThemeColor::Indexed(i) => Color::Indexed(i),
        ThemeColor::Rgb { r, g, b } => Color::Rgb(r, g, b),

        // Semantic slots — resolved against the active palette.
        ThemeColor::Fg => palette.fg,
        ThemeColor::Bg => palette.bg,
        ThemeColor::Accent => palette.accent,
        ThemeColor::Muted => palette.muted,
        ThemeColor::Error => palette.error,
        ThemeColor::Warning => palette.warning,
        ThemeColor::Success => palette.success,
        ThemeColor::Secondary => palette.secondary,
        ThemeColor::Border => palette.border,

        // `ThemeColor` is `#[non_exhaustive]`. Future variants added by
        // `savvagent-plugin` that this runtime has not been taught about
        // fall back to the theme's default fg so the text remains legible.
        _ => palette.fg,
    }
}

/// Map `TextMods` to a ratatui `Modifier` bitfield.
pub fn text_mods_to_modifier(m: TextMods) -> Modifier {
    let mut out = Modifier::empty();
    if m.bold {
        out |= Modifier::BOLD;
    }
    if m.italic {
        out |= Modifier::ITALIC;
    }
    if m.underline {
        out |= Modifier::UNDERLINED;
    }
    if m.reverse {
        out |= Modifier::REVERSED;
    }
    if m.dim {
        out |= Modifier::DIM;
    }
    out
}

/// Convert a `StyledSpan` to a ratatui `Span<'static>`, resolving any
/// semantic [`ThemeColor`] variants against the active palette.
pub fn styled_span_to_ratatui(
    span: StyledSpan,
    palette: &crate::palette::Palette,
) -> Span<'static> {
    let mut style = Style::default();
    if let Some(fg) = span.fg {
        style = style.fg(theme_color_to_ratatui(fg, palette));
    }
    if let Some(bg) = span.bg {
        style = style.bg(theme_color_to_ratatui(bg, palette));
    }
    let mods = text_mods_to_modifier(span.modifiers);
    if !mods.is_empty() {
        style = style.add_modifier(mods);
    }
    Span::styled(span.text, style)
}

/// Convert a `StyledLine` to a ratatui `Line<'static>`, resolving any
/// semantic [`ThemeColor`] variants against the active palette.
pub fn styled_line_to_ratatui(
    line: StyledLine,
    palette: &crate::palette::Palette,
) -> Line<'static> {
    Line::from(
        line.spans
            .into_iter()
            .map(|s| styled_span_to_ratatui(s, palette))
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::TextMods;

    #[test]
    fn region_rect_roundtrip() {
        let r = Region {
            x: 1,
            y: 2,
            width: 80,
            height: 24,
        };
        let rect = region_to_rect(r);
        let back = rect_to_region(rect);
        assert_eq!(back, r);
    }

    #[test]
    fn theme_color_cyan_maps_correctly() {
        let palette = crate::palette::Palette::for_theme(
            crate::plugin::builtin::themes::catalog::Theme::Dark,
        );
        assert_eq!(
            theme_color_to_ratatui(ThemeColor::Cyan, &palette),
            Color::Cyan
        );
    }

    #[test]
    fn theme_color_semantic_fg_resolves_against_active_palette() {
        // Dark theme's fg is Color::White.
        let dark = crate::palette::Palette::for_theme(
            crate::plugin::builtin::themes::catalog::Theme::Dark,
        );
        assert_eq!(
            theme_color_to_ratatui(ThemeColor::Fg, &dark),
            Color::White,
            "Dark theme fg should resolve to White"
        );
        // Light theme's fg is Color::Black — confirms the resolution is
        // palette-driven, not literal.
        let light = crate::palette::Palette::for_theme(
            crate::plugin::builtin::themes::catalog::Theme::Light,
        );
        assert_eq!(
            theme_color_to_ratatui(ThemeColor::Fg, &light),
            Color::Black,
            "Light theme fg should resolve to Black"
        );
    }

    #[test]
    fn theme_color_semantic_accent_differs_per_theme() {
        // Dark uses Color::Blue for accent; HighContrast uses Color::Yellow.
        // If these collapse to the same Color, the semantic resolution is
        // not threading the palette through.
        let dark = crate::palette::Palette::for_theme(
            crate::plugin::builtin::themes::catalog::Theme::Dark,
        );
        let hc = crate::palette::Palette::for_theme(
            crate::plugin::builtin::themes::catalog::Theme::HighContrast,
        );
        assert_ne!(
            theme_color_to_ratatui(ThemeColor::Accent, &dark),
            theme_color_to_ratatui(ThemeColor::Accent, &hc),
            "semantic Accent must vary across themes"
        );
    }

    #[test]
    fn text_mods_bold_italic() {
        let m = TextMods {
            bold: true,
            italic: true,
            ..Default::default()
        };
        let mods = text_mods_to_modifier(m);
        assert!(mods.contains(Modifier::BOLD));
        assert!(mods.contains(Modifier::ITALIC));
        assert!(!mods.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn styled_line_to_ratatui_produces_spans() {
        use savvagent_plugin::{StyledSpan, TextMods};
        let line = StyledLine {
            spans: vec![StyledSpan {
                text: "hello".into(),
                fg: Some(ThemeColor::Green),
                bg: None,
                modifiers: TextMods::default(),
            }],
        };
        let palette = crate::palette::Palette::for_theme(
            crate::plugin::builtin::themes::catalog::Theme::Dark,
        );
        let rline = styled_line_to_ratatui(line, &palette);
        assert_eq!(rline.spans.len(), 1);
        assert_eq!(rline.spans[0].content, "hello");
    }

    #[test]
    fn key_event_to_portable_char_ctrl() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let evt = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        let p = super::key_event_to_portable(evt);
        assert!(matches!(
            p.code,
            savvagent_plugin::KeyCodePortable::Char('s')
        ));
        assert!(p.modifiers.ctrl);
        assert!(!p.modifiers.alt);
    }

    #[test]
    fn key_event_to_portable_null_maps_to_unknown() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let evt = KeyEvent::new(KeyCode::Null, KeyModifiers::NONE);
        let p = super::key_event_to_portable(evt);
        assert!(matches!(p.code, savvagent_plugin::KeyCodePortable::Unknown));
    }
}
