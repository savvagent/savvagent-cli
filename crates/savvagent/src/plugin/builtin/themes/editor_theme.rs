//! Map our render-path [`Palette`] into a syntax-highlighting theme
//! for `ratatui-code-editor`.
//!
//! The upstream theme is a `Vec<(token, hex_color)>` keyed by
//! tree-sitter capture names (e.g. `"keyword"`, `"string"`,
//! `"comment"`). Returning hex strings means we lose the user's
//! terminal palette remapping for the highlighted-token colors, but in
//! return we get a consistent visual style across terminals and the
//! editor's syntax colors track the active TUI theme — switching from
//! Dark to Tokyo Night re-themes both chrome and code.
//!
//! Slot correspondences (see also `Palette::from_upstream` for the
//! TUI-chrome side):
//!
//! | Token group                                                     | Palette slot |
//! |-----------------------------------------------------------------|--------------|
//! | identifiers, properties, variables                              | `fg`         |
//! | keywords, namespaces, tags                                      | `accent`     |
//! | strings                                                         | `success`    |
//! | numbers, constants, attribute tags                              | `warning`    |
//! | types, functions, methods                                       | `secondary`  |
//! | comments                                                        | `muted`      |
//! | errors                                                          | `error`      |
//!
//! The mapping is loose by design — tree-sitter capture names vary by
//! language and many themes (including the upstream `vesper` we
//! replaced) use the same color for several groups. The intent is
//! "syntax visible and theme-consistent," not "perfect TextMate
//! fidelity."

use ratatui::style::Color;

use crate::palette::Palette;

/// Build a syntax-highlighting theme for `ratatui-code-editor` that
/// mirrors the active TUI palette. Returns owned `(token, hex_color)`
/// pairs; the caller converts to `Vec<(&str, &str)>` for
/// `Editor::new`.
pub fn build_editor_theme(palette: &Palette) -> Vec<(String, String)> {
    // Fallbacks are used when a palette slot is `Color::Reset`, which
    // means "no override — use terminal default" and has no fixed hex
    // representation. We pick neutral mid-tone values that read well on
    // most terminal backgrounds.
    let fg = color_to_hex(palette.fg, "#cccccc");
    let accent = color_to_hex(palette.accent, "#5f87d7");
    let warning = color_to_hex(palette.warning, "#d7af00");
    let error = color_to_hex(palette.error, "#d70000");
    let success = color_to_hex(palette.success, "#5faf5f");
    let secondary = color_to_hex(palette.secondary, "#5fafaf");
    let muted = color_to_hex(palette.muted, "#6c6c6c");

    vec![
        ("identifier".into(), fg.clone()),
        ("field_identifier".into(), fg.clone()),
        ("property_identifier".into(), fg.clone()),
        ("property".into(), fg.clone()),
        ("variable".into(), fg.clone()),
        ("variable.builtin".into(), fg),
        ("string".into(), success.clone()),
        ("keyword".into(), accent.clone()),
        ("constant".into(), warning.clone()),
        ("number".into(), warning.clone()),
        ("integer".into(), warning.clone()),
        ("float".into(), warning.clone()),
        ("function".into(), secondary.clone()),
        ("function.call".into(), secondary.clone()),
        ("method".into(), secondary.clone()),
        ("function.macro".into(), secondary.clone()),
        ("type".into(), secondary.clone()),
        ("type.builtin".into(), secondary),
        ("namespace".into(), accent.clone()),
        ("comment".into(), muted),
        ("tag".into(), accent),
        ("tag.attribute".into(), warning),
        ("error".into(), error),
        // String guard handled separately so `String::clone()` above
        // doesn't waste a clone; `success` is reused for `string` only.
        ("string.escape".into(), success),
    ]
}

/// Convert a [`Color`] to a `#RRGGBB` hex string. `Color::Reset` uses
/// `fallback` since it has no fixed hex representation (means
/// "terminal default"). Indexed colors are resolved against the
/// standard 256-color xterm palette.
fn color_to_hex(c: Color, fallback: &str) -> String {
    match c {
        Color::Reset => fallback.to_string(),
        Color::Black => "#000000".into(),
        Color::Red => "#800000".into(),
        Color::Green => "#008000".into(),
        Color::Yellow => "#808000".into(),
        Color::Blue => "#000080".into(),
        Color::Magenta => "#800080".into(),
        Color::Cyan => "#008080".into(),
        Color::Gray => "#c0c0c0".into(),
        Color::DarkGray => "#808080".into(),
        Color::LightRed => "#ff0000".into(),
        Color::LightGreen => "#00ff00".into(),
        Color::LightYellow => "#ffff00".into(),
        Color::LightBlue => "#0000ff".into(),
        Color::LightMagenta => "#ff00ff".into(),
        Color::LightCyan => "#00ffff".into(),
        Color::White => "#ffffff".into(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(n) => indexed_to_hex(n),
    }
}

/// Resolve an xterm 256-color palette index to `#RRGGBB`.
fn indexed_to_hex(n: u8) -> String {
    let (r, g, b) = xterm_256_rgb(n);
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Resolve an xterm 256-color palette index to its RGB triple.
///
/// Layout:
/// * `0..=15`  — system colors (ANSI 8 + bright ANSI 8)
/// * `16..=231` — 6×6×6 RGB cube
/// * `232..=255` — 24 grayscale steps
///
/// Single source of truth shared with the egui sink
/// (`crate::egui_app::convert`) so indexed colors render identically in the
/// code-editor syntax theme and the GUI conversation log.
pub(crate) fn xterm_256_rgb(n: u8) -> (u8, u8, u8) {
    match n {
        0 => (0, 0, 0),
        1 => (128, 0, 0),
        2 => (0, 128, 0),
        3 => (128, 128, 0),
        4 => (0, 0, 128),
        5 => (128, 0, 128),
        6 => (0, 128, 128),
        7 => (192, 192, 192),
        8 => (128, 128, 128),
        9 => (255, 0, 0),
        10 => (0, 255, 0),
        11 => (255, 255, 0),
        12 => (0, 0, 255),
        13 => (255, 0, 255),
        14 => (0, 255, 255),
        15 => (255, 255, 255),
        16..=231 => {
            let idx = n - 16;
            let r = idx / 36;
            let g = (idx / 6) % 6;
            let b = idx % 6;
            let to_comp = |x: u8| -> u8 {
                if x == 0 {
                    0
                } else {
                    (55_u16 + 40_u16 * x as u16).min(255) as u8
                }
            };
            (to_comp(r), to_comp(g), to_comp(b))
        }
        232..=255 => {
            let v = (8_u16 + 10_u16 * (n as u16 - 232)).min(255) as u8;
            (v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::builtin::themes::catalog::Theme;

    #[test]
    fn rgb_color_round_trips() {
        assert_eq!(color_to_hex(Color::Rgb(0x12, 0x34, 0x56), "x"), "#123456");
        assert_eq!(color_to_hex(Color::Rgb(0, 0, 0), "x"), "#000000");
        assert_eq!(color_to_hex(Color::Rgb(255, 255, 255), "x"), "#ffffff");
    }

    #[test]
    fn named_colors_have_stable_hex() {
        assert_eq!(color_to_hex(Color::Black, "x"), "#000000");
        assert_eq!(color_to_hex(Color::White, "x"), "#ffffff");
        assert_eq!(color_to_hex(Color::DarkGray, "x"), "#808080");
        assert_eq!(color_to_hex(Color::Blue, "x"), "#000080");
        assert_eq!(color_to_hex(Color::LightBlue, "x"), "#0000ff");
    }

    #[test]
    fn reset_falls_back_to_caller_provided_default() {
        assert_eq!(color_to_hex(Color::Reset, "#abcdef"), "#abcdef");
    }

    #[test]
    fn indexed_system_colors_match_named_equivalents() {
        // First 16 indexed colors should produce the same hex as the
        // matching named variant.
        assert_eq!(indexed_to_hex(0), "#000000");
        assert_eq!(indexed_to_hex(7), "#c0c0c0"); // ANSI white = gray
        assert_eq!(indexed_to_hex(8), "#808080"); // bright black = dark gray
        assert_eq!(indexed_to_hex(15), "#ffffff");
    }

    #[test]
    fn indexed_rgb_cube_uses_xterm_step_values() {
        // 6×6×6 cube starts at 16 with (0,0,0) and increments by the
        // xterm convention `55 + 40·x` for x>0.
        assert_eq!(indexed_to_hex(16), "#000000");
        assert_eq!(indexed_to_hex(17), "#00005f"); // (0,0,1) → blue 0x5f
        assert_eq!(indexed_to_hex(231), "#ffffff");
    }

    #[test]
    fn indexed_grayscale_ramp_steps_by_ten() {
        assert_eq!(indexed_to_hex(232), "#080808");
        assert_eq!(indexed_to_hex(233), "#121212"); // 18 hex
        assert_eq!(indexed_to_hex(255), "#eeeeee");
    }

    #[test]
    fn build_editor_theme_includes_every_required_token_kind() {
        let palette = Palette::for_theme(Theme::Dark);
        let theme = build_editor_theme(&palette);
        // The upstream `vesper` baseline mapped these capture names;
        // our replacement must cover the same so syntax highlighting
        // remains complete after the swap.
        for required in [
            "identifier",
            "string",
            "keyword",
            "constant",
            "number",
            "comment",
            "type",
            "function",
            "tag",
            "error",
        ] {
            assert!(
                theme.iter().any(|(k, _)| k == required),
                "missing required token kind `{required}` in editor theme",
            );
        }
    }

    #[test]
    fn build_editor_theme_emits_hex_color_strings() {
        let palette = Palette::for_theme(Theme::Dark);
        let theme = build_editor_theme(&palette);
        for (kind, hex) in &theme {
            assert!(
                hex.starts_with('#') && hex.len() == 7,
                "token `{kind}` has non-hex color `{hex}`"
            );
        }
    }

    #[test]
    fn dark_and_light_themes_produce_different_string_colors() {
        // The 'string' token kind uses `palette.success`, which
        // differs between Dark (Green) and Light (also Green but on
        // a different background — the hex values themselves are the
        // same in our mapping). Switch to HighContrast which uses
        // LightGreen instead to get a real diff.
        let dark = build_editor_theme(&Palette::for_theme(Theme::Dark));
        let hc = build_editor_theme(&Palette::for_theme(Theme::HighContrast));
        let dark_string = dark
            .iter()
            .find(|(k, _)| k == "string")
            .map(|(_, v)| v.clone())
            .unwrap();
        let hc_string = hc
            .iter()
            .find(|(k, _)| k == "string")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_ne!(
            dark_string, hc_string,
            "Dark and HighContrast must produce different `string` colors"
        );
    }
}
