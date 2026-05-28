//! Adapter: a `Palette` (Plan 1's `Color32`-backed semantic-slot model) →
//! an `egui_code_editor::ColorTheme` whose per-token colors track the
//! active TUI theme. Mirrors the slot correspondences in
//! `plugin/builtin/themes/editor_theme.rs::build_editor_theme`.
//!
//! Confirmed `egui_code_editor::ColorTheme` field names (against 0.2.17):
//! - `name: &'static str` — display name (e.g. "GRUVBOX")
//! - `dark: bool` — whether the theme is a dark variant
//! - `bg: &'static str` — background hex (e.g. "#1D2021")
//! - `cursor: &'static str` — cursor color hex
//! - `selection: &'static str` — selection background hex
//! - `comments: &'static str` — comment-token color hex
//! - `functions: &'static str` — function-name color hex
//! - `keywords: &'static str` — keyword color hex
//! - `literals: &'static str` — literal color hex (booleans, nil, etc.)
//! - `numerics: &'static str` — numeric-literal color hex
//! - `punctuation: &'static str` — operator/punctuation color hex
//! - `strs: &'static str` — string-literal color hex
//! - `types: &'static str` — type-name color hex
//! - `special: &'static str` — special-token color hex (errors, attributes)
//!
//! All color fields are `&'static str` (hex strings), NOT `egui::Color32`.
//! The plan's `Box::leak(format!("#{:02X}{:02X}{:02X}", r, g, b))` pattern
//! is the canonical way to materialize per-frame `Palette` colors as the
//! `'static` strings `ColorTheme` requires. Construct via struct literal
//! (all 14 fields are `pub`); `Default` is also implemented, and the crate
//! ships nine pre-defined `const` themes (e.g. `ColorTheme::GRUVBOX`,
//! `ColorTheme::GITHUB_DARK`). A single `monocolor()` constructor exists
//! for monochromatic palettes.

use egui_code_editor::ColorTheme;

use crate::egui_app::convert::theme_color_to_color32;
use crate::palette::Palette;
use savvagent_plugin::ThemeColor;

/// Build an `egui_code_editor::ColorTheme` from the active `Palette`.
///
/// Token slots mirror `plugin/builtin/themes/editor_theme.rs`:
/// keywords/namespaces/tags → `Accent`; strings → `Success`;
/// constants/numbers → `Warning`; types/functions/methods → `Secondary`;
/// comments → `Muted`; errors → `Error`; identifiers/punctuation → `Fg`.
/// Background is the palette's `Bg` slot.
pub fn palette_to_color_theme(palette: &Palette) -> ColorTheme {
    let bg = theme_color_to_color32(ThemeColor::Bg, palette);
    let fg = theme_color_to_color32(ThemeColor::Fg, palette);
    let accent = theme_color_to_color32(ThemeColor::Accent, palette);
    let success = theme_color_to_color32(ThemeColor::Success, palette);
    let warning = theme_color_to_color32(ThemeColor::Warning, palette);
    let error = theme_color_to_color32(ThemeColor::Error, palette);
    let secondary = theme_color_to_color32(ThemeColor::Secondary, palette);
    let muted = theme_color_to_color32(ThemeColor::Muted, palette);

    ColorTheme {
        name: "savvagent",
        dark: true, // The palette's lightness is not exposed; default to
        // dark so egui_code_editor uses a dark cursor blink etc.
        // If light themes look wrong, plumb a Palette::is_dark()
        // accessor — out of scope for Plan 3.
        bg: color32_to_hex(bg),
        cursor: color32_to_hex(accent),
        selection: color32_to_hex(muted),
        comments: color32_to_hex(muted),
        functions: color32_to_hex(secondary),
        keywords: color32_to_hex(accent),
        literals: color32_to_hex(success),
        numerics: color32_to_hex(warning),
        punctuation: color32_to_hex(fg),
        strs: color32_to_hex(success),
        types: color32_to_hex(secondary),
        special: color32_to_hex(error),
    }
}

/// Render a `Color32` as `#RRGGBB`. ColorTheme stores `&'static str` per
/// color slot (see header comment), so dynamic per-palette hex strings are
/// interned via `Box::leak`. The leak is bounded — one short string per slot
/// per theme switch — well under the lifetime-cost line.
fn color32_to_hex(c: egui::Color32) -> &'static str {
    Box::leak(format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b()).into_boxed_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::Palette;
    use crate::plugin::builtin::themes::catalog::Theme;

    #[test]
    fn dark_palette_maps_keywords_to_accent() {
        let palette = Palette::for_theme(Theme::Dark);
        let theme = palette_to_color_theme(&palette);
        let expected_accent = color32_to_hex(theme_color_to_color32(ThemeColor::Accent, &palette));
        assert_eq!(theme.keywords, expected_accent);
    }

    #[test]
    fn dark_palette_maps_strings_to_success() {
        let palette = Palette::for_theme(Theme::Dark);
        let theme = palette_to_color_theme(&palette);
        let expected = color32_to_hex(theme_color_to_color32(ThemeColor::Success, &palette));
        assert_eq!(theme.strs, expected);
        assert_eq!(theme.literals, expected);
    }

    #[test]
    fn dark_palette_maps_comments_to_muted() {
        let palette = Palette::for_theme(Theme::Dark);
        let theme = palette_to_color_theme(&palette);
        let expected = color32_to_hex(theme_color_to_color32(ThemeColor::Muted, &palette));
        assert_eq!(theme.comments, expected);
    }

    #[test]
    fn light_palette_maps_background_to_bg() {
        let palette = Palette::for_theme(Theme::Light);
        let theme = palette_to_color_theme(&palette);
        let expected = color32_to_hex(theme_color_to_color32(ThemeColor::Bg, &palette));
        assert_eq!(theme.bg, expected);
    }
}
