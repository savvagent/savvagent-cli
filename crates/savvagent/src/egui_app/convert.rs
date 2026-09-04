//! egui sinks for the plugin render boundary. Pure functions that mirror
//! `crate::plugin::convert` (the ratatui sink) but target egui types. The
//! color path bridges through the existing ratatui resolver so the two sinks
//! never disagree on semantic-slot resolution.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontFamily, FontId};
use ratatui::style::Color;
use savvagent_plugin::{
    KeyCodePortable, KeyEventPortable, KeyMods, StyledLine, StyledSpan, TextMods, ThemeColor,
};

use crate::palette::Palette;
use crate::plugin::builtin::themes::editor_theme::xterm_256_rgb;

/// Resolve a [`ThemeColor`] (including semantic slots) to an egui [`Color32`].
///
/// Bridges through the existing ratatui resolver
/// ([`crate::plugin::convert::theme_color_to_ratatui`]) so the ratatui and
/// egui sinks always agree on how semantic slots resolve against the active
/// palette; the resulting `ratatui::style::Color` is then mapped to concrete
/// RGB.
pub fn theme_color_to_color32(c: ThemeColor, palette: &Palette) -> Color32 {
    let resolved = crate::plugin::convert::theme_color_to_ratatui(c, palette);
    ratatui_color_to_color32(resolved, palette)
}

/// Map a resolved `ratatui::style::Color` to [`Color32`].
///
/// Named ANSI colors and `Indexed` share the xterm-256 table the code-editor
/// theme uses ([`xterm_256_rgb`]), so indexed colors render identically in the
/// editor and the GUI log. `Reset` (terminal default) falls back to the
/// palette foreground.
fn ratatui_color_to_color32(c: Color, palette: &Palette) -> Color32 {
    let (r, g, b) = match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => xterm_256_rgb(0),
        Color::Red => xterm_256_rgb(1),
        Color::Green => xterm_256_rgb(2),
        Color::Yellow => xterm_256_rgb(3),
        Color::Blue => xterm_256_rgb(4),
        Color::Magenta => xterm_256_rgb(5),
        Color::Cyan => xterm_256_rgb(6),
        Color::Gray => xterm_256_rgb(7),
        Color::DarkGray => xterm_256_rgb(8),
        Color::LightRed => xterm_256_rgb(9),
        Color::LightGreen => xterm_256_rgb(10),
        Color::LightYellow => xterm_256_rgb(11),
        Color::LightBlue => xterm_256_rgb(12),
        Color::LightMagenta => xterm_256_rgb(13),
        Color::LightCyan => xterm_256_rgb(14),
        Color::White => xterm_256_rgb(15),
        Color::Indexed(i) => xterm_256_rgb(i),
        Color::Reset => {
            let fg = crate::plugin::convert::theme_color_to_ratatui(ThemeColor::Fg, palette);
            return ratatui_color_to_color32(fg, palette);
        }
    };
    Color32::from_rgb(r, g, b)
}

/// Build a `(text, TextFormat)` pair for one styled span at `size` px. The
/// log/screen model is monospace, so the section uses [`FontFamily::Monospace`].
///
/// `fg`/`bg` resolve through [`theme_color_to_color32`] — an unset `fg`
/// inherits the palette foreground, an unset `bg` is transparent. `italic` and
/// `underline` map onto egui's [`TextFormat`]. `bold`, `reverse`, and `dim`
/// have no direct `TextFormat` equivalent (bold needs a bold font face;
/// reverse/dim are deferred to a later fidelity pass).
pub fn styled_span_to_format(
    span: &StyledSpan,
    palette: &Palette,
    size: f32,
) -> (String, TextFormat) {
    let color = span
        .fg
        .map(|c| theme_color_to_color32(c, palette))
        .unwrap_or_else(|| theme_color_to_color32(ThemeColor::Fg, palette));
    let background = span
        .bg
        .map(|c| theme_color_to_color32(c, palette))
        .unwrap_or(Color32::TRANSPARENT);
    let TextMods {
        italic, underline, ..
    } = span.modifiers;
    let mut fmt = TextFormat {
        font_id: FontId::new(size, FontFamily::Monospace),
        color,
        background,
        italics: italic,
        ..Default::default()
    };
    if underline {
        fmt.underline = egui::Stroke::new(1.0_f32, color);
    }
    (span.text.clone(), fmt)
}

/// Convert a styled line into a [`LayoutJob`] — one section per span.
pub fn styled_line_to_job(line: &StyledLine, palette: &Palette, size: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    for span in &line.spans {
        let (text, fmt) = styled_span_to_format(span, palette, size);
        job.append(&text, 0.0, fmt);
    }
    job
}

/// Convert a *press* egui event into a [`KeyEventPortable`].
///
/// Returns `None` for key releases and events that carry no key meaning.
/// [`egui::Event::Text`] maps to a `Char` so typed characters reach
/// prompt/editor consumers; the [`egui::Event::Key`] path additionally covers
/// navigation/control keys and modifier accelerators (e.g. Ctrl+S, where egui
/// emits no `Text` event). Mirrors `crate::plugin::convert::key_event_to_portable`
/// (the crossterm sink) but targets egui types.
pub fn egui_event_to_portable(ev: &egui::Event) -> Option<KeyEventPortable> {
    match ev {
        egui::Event::Text(s) => {
            let c = s.chars().next()?;
            Some(KeyEventPortable {
                code: KeyCodePortable::Char(c),
                modifiers: KeyMods::default(),
            })
        }
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => Some(KeyEventPortable {
            code: egui_key_to_portable(*key)?,
            modifiers: KeyMods {
                ctrl: modifiers.ctrl,
                alt: modifiers.alt,
                shift: modifiers.shift,
                meta: modifiers.mac_cmd || modifiers.command,
            },
        }),
        _ => None,
    }
}

/// Map an [`egui::Key`] to its portable code. Printable keys map to `Char`
/// (lower-case for letters; `shift` is carried separately in [`KeyMods`]).
/// Keys with no portable equivalent (clipboard, browser navigation) return
/// `None` so the event is ignored.
fn egui_key_to_portable(k: egui::Key) -> Option<KeyCodePortable> {
    use KeyCodePortable as P;
    use egui::Key as K;
    Some(match k {
        K::Enter => P::Enter,
        K::Escape => P::Esc,
        K::Backspace => P::Backspace,
        K::Tab => P::Tab,
        K::Insert => P::Insert,
        K::Delete => P::Delete,
        K::ArrowUp => P::Up,
        K::ArrowDown => P::Down,
        K::ArrowLeft => P::Left,
        K::ArrowRight => P::Right,
        K::Home => P::Home,
        K::End => P::End,
        K::PageUp => P::PageUp,
        K::PageDown => P::PageDown,
        K::Space => P::Char(' '),
        K::Colon => P::Char(':'),
        K::Comma => P::Char(','),
        K::Backslash => P::Char('\\'),
        K::Slash => P::Char('/'),
        K::Pipe => P::Char('|'),
        K::Questionmark => P::Char('?'),
        K::Exclamationmark => P::Char('!'),
        K::OpenBracket => P::Char('['),
        K::CloseBracket => P::Char(']'),
        K::OpenCurlyBracket => P::Char('{'),
        K::CloseCurlyBracket => P::Char('}'),
        K::Backtick => P::Char('`'),
        K::Minus => P::Char('-'),
        K::Period => P::Char('.'),
        K::Plus => P::Char('+'),
        K::Equals => P::Char('='),
        K::Semicolon => P::Char(';'),
        K::Quote => P::Char('\''),
        K::Num0 => P::Char('0'),
        K::Num1 => P::Char('1'),
        K::Num2 => P::Char('2'),
        K::Num3 => P::Char('3'),
        K::Num4 => P::Char('4'),
        K::Num5 => P::Char('5'),
        K::Num6 => P::Char('6'),
        K::Num7 => P::Char('7'),
        K::Num8 => P::Char('8'),
        K::Num9 => P::Char('9'),
        K::A => P::Char('a'),
        K::B => P::Char('b'),
        K::C => P::Char('c'),
        K::D => P::Char('d'),
        K::E => P::Char('e'),
        K::F => P::Char('f'),
        K::G => P::Char('g'),
        K::H => P::Char('h'),
        K::I => P::Char('i'),
        K::J => P::Char('j'),
        K::K => P::Char('k'),
        K::L => P::Char('l'),
        K::M => P::Char('m'),
        K::N => P::Char('n'),
        K::O => P::Char('o'),
        K::P => P::Char('p'),
        K::Q => P::Char('q'),
        K::R => P::Char('r'),
        K::S => P::Char('s'),
        K::T => P::Char('t'),
        K::U => P::Char('u'),
        K::V => P::Char('v'),
        K::W => P::Char('w'),
        K::X => P::Char('x'),
        K::Y => P::Char('y'),
        K::Z => P::Char('z'),
        K::F1 => P::F(1),
        K::F2 => P::F(2),
        K::F3 => P::F(3),
        K::F4 => P::F(4),
        K::F5 => P::F(5),
        K::F6 => P::F(6),
        K::F7 => P::F(7),
        K::F8 => P::F(8),
        K::F9 => P::F(9),
        K::F10 => P::F(10),
        K::F11 => P::F(11),
        K::F12 => P::F(12),
        K::F13 => P::F(13),
        K::F14 => P::F(14),
        K::F15 => P::F(15),
        K::F16 => P::F(16),
        K::F17 => P::F(17),
        K::F18 => P::F(18),
        K::F19 => P::F(19),
        K::F20 => P::F(20),
        K::F21 => P::F(21),
        K::F22 => P::F(22),
        K::F23 => P::F(23),
        K::F24 => P::F(24),
        K::F25 => P::F(25),
        K::F26 => P::F(26),
        K::F27 => P::F(27),
        K::F28 => P::F(28),
        K::F29 => P::F(29),
        K::F30 => P::F(30),
        K::F31 => P::F(31),
        K::F32 => P::F(32),
        K::F33 => P::F(33),
        K::F34 => P::F(34),
        K::F35 => P::F(35),
        // Clipboard / browser keys have no portable equivalent.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use egui::Color32;
    use savvagent_plugin::ThemeColor;

    use super::*;
    use crate::palette::Palette;
    use crate::plugin::builtin::themes::catalog::Theme;

    #[test]
    fn literal_rgb_maps_directly() {
        let p = Palette::for_theme(Theme::Dark);
        assert_eq!(
            theme_color_to_color32(
                ThemeColor::Rgb {
                    r: 10,
                    g: 20,
                    b: 30
                },
                &p
            ),
            Color32::from_rgb(10, 20, 30)
        );
    }

    #[test]
    fn semantic_fg_differs_dark_vs_light() {
        let dark = Palette::for_theme(Theme::Dark);
        let light = Palette::for_theme(Theme::Light);
        assert_ne!(
            theme_color_to_color32(ThemeColor::Fg, &dark),
            theme_color_to_color32(ThemeColor::Fg, &light),
            "semantic Fg must resolve per-theme"
        );
    }

    #[test]
    fn semantic_accent_differs_dark_vs_highcontrast() {
        let dark = Palette::for_theme(Theme::Dark);
        let hc = Palette::for_theme(Theme::HighContrast);
        assert_ne!(
            theme_color_to_color32(ThemeColor::Accent, &dark),
            theme_color_to_color32(ThemeColor::Accent, &hc)
        );
    }

    #[test]
    fn span_to_format_applies_fg_and_keeps_italics_off_for_bold() {
        use savvagent_plugin::{StyledSpan, TextMods};
        let p = Palette::for_theme(Theme::Dark);
        let span = StyledSpan {
            text: "hi".into(),
            fg: Some(ThemeColor::Green),
            bg: None,
            modifiers: TextMods {
                bold: true,
                ..Default::default()
            },
        };
        let (text, fmt) = styled_span_to_format(&span, &p, 14.0);
        assert_eq!(text, "hi");
        // Green resolves through the shared xterm-256 table (index 2).
        assert_eq!(fmt.color, Color32::from_rgb(0, 128, 0));
        assert!(!fmt.italics, "bold must not set italics");
    }

    #[test]
    fn span_to_format_sets_italics_and_underline() {
        use savvagent_plugin::{StyledSpan, TextMods};
        let p = Palette::for_theme(Theme::Dark);
        let span = StyledSpan {
            text: "x".into(),
            fg: None,
            bg: None,
            modifiers: TextMods {
                italic: true,
                underline: true,
                ..Default::default()
            },
        };
        let (_text, fmt) = styled_span_to_format(&span, &p, 14.0);
        assert!(fmt.italics);
        assert!(fmt.underline.width > 0.0, "underline stroke must be set");
    }

    #[test]
    fn line_to_job_concatenates_spans() {
        use savvagent_plugin::{StyledLine, StyledSpan, TextMods};
        let p = Palette::for_theme(Theme::Dark);
        let line = StyledLine {
            spans: vec![
                StyledSpan {
                    text: "a".into(),
                    fg: None,
                    bg: None,
                    modifiers: TextMods::default(),
                },
                StyledSpan {
                    text: "b".into(),
                    fg: None,
                    bg: None,
                    modifiers: TextMods::default(),
                },
            ],
        };
        let job = styled_line_to_job(&line, &p, 14.0);
        assert_eq!(job.text, "ab");
        assert_eq!(job.sections.len(), 2);
    }

    #[test]
    fn enter_maps_to_enter() {
        let ev = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let p = egui_event_to_portable(&ev).expect("enter maps");
        assert!(matches!(p.code, savvagent_plugin::KeyCodePortable::Enter));
    }

    #[test]
    fn ctrl_s_sets_ctrl_modifier_and_char() {
        let ev = egui::Event::Key {
            key: egui::Key::S,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        };
        let p = egui_event_to_portable(&ev).expect("ctrl-s maps");
        assert!(p.modifiers.ctrl);
        assert!(matches!(
            p.code,
            savvagent_plugin::KeyCodePortable::Char('s')
        ));
    }

    #[test]
    fn text_event_maps_to_char() {
        let ev = egui::Event::Text("x".into());
        let p = egui_event_to_portable(&ev).expect("text maps");
        assert!(matches!(
            p.code,
            savvagent_plugin::KeyCodePortable::Char('x')
        ));
    }

    #[test]
    fn key_release_returns_none() {
        let ev = egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert!(egui_event_to_portable(&ev).is_none());
    }

    #[test]
    fn unmapped_key_returns_none() {
        let ev = egui::Event::Key {
            key: egui::Key::Copy,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert!(egui_event_to_portable(&ev).is_none());
    }
}
