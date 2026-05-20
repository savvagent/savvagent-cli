//! Owned styled-text types crossing plugin boundaries.
//!
//! Plugins return `Vec<StyledLine>` from `render_slot` and `Screen::render`.
//! The runtime converts these into ratatui `Span` / `Line` at the boundary.

/// A line of styled text, owned, with no ratatui dep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledLine {
    /// Ordered sequence of styled spans that make up this line.
    pub spans: Vec<StyledSpan>,
}

/// A single styled run of text within a [`StyledLine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    /// The text content of this span.
    pub text: String,
    /// Optional foreground color; `None` means inherit from the terminal theme.
    pub fg: Option<ThemeColor>,
    /// Optional background color; `None` means inherit from the terminal theme.
    pub bg: Option<ThemeColor>,
    /// Text attribute modifiers applied to this span.
    pub modifiers: TextMods,
}

/// A terminal color that can be used as a foreground or background.
///
/// Variants cover the 16 ANSI named colors, the 256-color indexed palette,
/// direct RGB, and a set of *semantic* slots (`Fg`, `Bg`, `Accent`, …)
/// that the runtime resolves against the active theme's palette.
///
/// Prefer the semantic variants in plugin code that wants to look correct
/// across every theme — they adapt to upstream palettes (Dracula, Nord,
/// Solarized Light, Catppuccin, …) where literal ANSI colors would either
/// disappear into the background or clash with it. Literal ANSI variants
/// (`Cyan`, `Red`, etc.) remain valid for cases where a specific color is
/// intended regardless of theme.
///
/// This enum is `#[non_exhaustive]` so the runtime can add new semantic
/// slots without breaking exhaustive matches in downstream code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ThemeColor {
    /// Terminal default color (inherits from the current theme).
    Default,
    /// ANSI color: black.
    Black,
    /// ANSI color: red.
    Red,
    /// ANSI color: green.
    Green,
    /// ANSI color: yellow.
    Yellow,
    /// ANSI color: blue.
    Blue,
    /// ANSI color: magenta.
    Magenta,
    /// ANSI color: cyan.
    Cyan,
    /// ANSI color: white.
    White,
    /// ANSI color: dark gray (bright black).
    DarkGray,
    /// ANSI color: light red (bright red).
    LightRed,
    /// ANSI color: light green (bright green).
    LightGreen,
    /// ANSI color: light yellow (bright yellow).
    LightYellow,
    /// ANSI color: light blue (bright blue).
    LightBlue,
    /// ANSI color: light magenta (bright magenta).
    LightMagenta,
    /// ANSI color: light cyan (bright cyan).
    LightCyan,
    /// ANSI color: gray (bright white).
    Gray,
    /// 256-color terminal palette index (0..=255).
    Indexed(u8),
    /// Direct RGB color; each component is in 0..=255.
    Rgb {
        /// Red component (0..=255).
        r: u8,
        /// Green component (0..=255).
        g: u8,
        /// Blue component (0..=255).
        b: u8,
    },

    // --- Semantic slots ---------------------------------------------------
    //
    // Resolved by the runtime against the active theme's palette. Use these
    // in preference to literal ANSI colors whenever the intent is "match
    // the theme" rather than "be specifically this color".
    /// Active theme's primary foreground color.
    Fg,
    /// Active theme's primary background color.
    Bg,
    /// Active theme's accent color (selected / highlighted / active).
    Accent,
    /// Active theme's muted color (descriptions, secondary text).
    Muted,
    /// Active theme's error color.
    Error,
    /// Active theme's warning color.
    Warning,
    /// Active theme's success color.
    Success,
    /// Active theme's secondary color.
    Secondary,
    /// Active theme's chrome / border color.
    Border,
}

/// Plain-bool text attribute flags for WIT portability.
///
/// Using individual booleans instead of ratatui's `Modifier` bitflags
/// keeps this type serialisable over WIT without a ratatui dependency.
/// See `docs/superpowers/specs/2026-05-12-v0.9.0-plugin-system-design.md`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TextMods {
    /// Bold text.
    pub bold: bool,
    /// Italic text.
    pub italic: bool,
    /// Underlined text.
    pub underline: bool,
    /// Reverse video (swap fg/bg).
    pub reverse: bool,
    /// Dim (faint) text.
    pub dim: bool,
}

impl StyledLine {
    /// Create a plain (unstyled) line containing a single span with the given text.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            spans: vec![StyledSpan {
                text: text.into(),
                fg: None,
                bg: None,
                modifiers: TextMods::default(),
            }],
        }
    }
}

/// Format a byte count using binary (1024-based) units.
///
/// - `0..1024` → `"<N> B"` (no decimal).
/// - `1024..` → `"<N.D> KiB"`, `"<N.D> MiB"`, `"<N.D> GiB"` (one decimal place).
///
/// Used by tool-summary plugins to render `bytes`, `bytes_written`, file
/// sizes, etc. Pure; no allocation beyond the returned `String`.
pub fn pretty_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else if bytes < GIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    }
}

/// Maximum characters retained from a single JSON string value before the
/// renderer truncates with an ellipsis. Whole-blob truncation is the caller's
/// responsibility (e.g. ratatui `Paragraph::wrap`).
const JSON_VALUE_MAX_CHARS: usize = 40;

/// Render a `serde_json::Value` as a one-line, theme-aware sequence of
/// `StyledSpan`s.
///
/// Colour palette (semantic slots — resolved by the host at render time):
/// - Object keys → [`ThemeColor::Accent`]
/// - String values → [`ThemeColor::Success`]
/// - Numbers, booleans, `null` → [`ThemeColor::Secondary`]
/// - Structural punctuation (`{`, `}`, `[`, `]`, `,`, `:`, key/value quotes) → [`ThemeColor::Muted`]
///
/// String values longer than 40 *characters* are truncated with a trailing
/// `…` (the character cap, not byte cap, keeps multi-byte UTF-8 safe).
///
/// Used by the TUI as the fallback rendering when no plugin claims a given
/// tool name; also exposed for plugins that want highlighted JSON for
/// fields they choose not to format specially.
pub fn json_spans(value: &serde_json::Value) -> Vec<StyledSpan> {
    let mut out: Vec<StyledSpan> = Vec::new();
    push_value(&mut out, value);
    out
}

fn push_value(out: &mut Vec<StyledSpan>, value: &serde_json::Value) {
    match value {
        serde_json::Value::Null => push(out, "null", ThemeColor::Secondary),
        serde_json::Value::Bool(b) => push(
            out,
            if *b { "true" } else { "false" },
            ThemeColor::Secondary,
        ),
        serde_json::Value::Number(n) => push(out, n.to_string(), ThemeColor::Secondary),
        serde_json::Value::String(s) => push_string_value(out, s),
        serde_json::Value::Array(items) => {
            push(out, "[", ThemeColor::Muted);
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    push(out, ", ", ThemeColor::Muted);
                }
                push_value(out, item);
            }
            push(out, "]", ThemeColor::Muted);
        }
        serde_json::Value::Object(map) => {
            push(out, "{", ThemeColor::Muted);
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    push(out, ", ", ThemeColor::Muted);
                }
                push(out, "\"", ThemeColor::Muted);
                push(out, k.clone(), ThemeColor::Accent);
                push(out, "\"", ThemeColor::Muted);
                push(out, ": ", ThemeColor::Muted);
                push_value(out, v);
            }
            push(out, "}", ThemeColor::Muted);
        }
    }
}

fn push_string_value(out: &mut Vec<StyledSpan>, s: &str) {
    let truncated: String = {
        let n = s.chars().count();
        if n <= JSON_VALUE_MAX_CHARS {
            s.to_string()
        } else {
            let mut t: String = s.chars().take(JSON_VALUE_MAX_CHARS).collect();
            t.push('…');
            t
        }
    };
    push(out, "\"", ThemeColor::Muted);
    push(out, truncated, ThemeColor::Success);
    push(out, "\"", ThemeColor::Muted);
}

fn push(out: &mut Vec<StyledSpan>, text: impl Into<String>, fg: ThemeColor) {
    out.push(StyledSpan {
        text: text.into(),
        fg: Some(fg),
        bg: None,
        modifiers: TextMods::default(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_bytes_formats_small_values_in_bytes() {
        assert_eq!(pretty_bytes(0), "0 B");
        assert_eq!(pretty_bytes(512), "512 B");
        assert_eq!(pretty_bytes(1023), "1023 B");
    }

    #[test]
    fn pretty_bytes_formats_kibi_and_mebi() {
        assert_eq!(pretty_bytes(1024), "1.0 KiB");
        assert_eq!(pretty_bytes(1536), "1.5 KiB");
        assert_eq!(pretty_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(pretty_bytes(1024 * 1024 * 4 + 1024 * 700), "4.7 MiB");
    }

    #[test]
    fn pretty_bytes_formats_gibi() {
        assert_eq!(pretty_bytes(1024 * 1024 * 1024), "1.0 GiB");
    }

    use serde_json::json;

    fn join_text(spans: &[StyledSpan]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn json_spans_renders_empty_object() {
        let spans = json_spans(&json!({}));
        assert_eq!(join_text(&spans), "{}");
    }

    #[test]
    fn json_spans_renders_empty_array() {
        let spans = json_spans(&json!([]));
        assert_eq!(join_text(&spans), "[]");
    }

    #[test]
    fn json_spans_renders_flat_object_in_key_value_order() {
        let spans = json_spans(&json!({"path": "src/main.rs", "size": 42}));
        // Order in serde_json::Map is insertion order, which is what `json!` preserves.
        assert_eq!(join_text(&spans), r#"{"path": "src/main.rs", "size": 42}"#);
    }

    #[test]
    fn json_spans_truncates_long_string_values_at_40_chars() {
        let long = "x".repeat(60);
        let spans = json_spans(&json!({"v": long}));
        // First 40 chars retained, then '…'.
        let expected_value = format!("\"{}…\"", "x".repeat(40));
        assert_eq!(join_text(&spans), format!("{{\"v\": {expected_value}}}"));
    }

    #[test]
    fn json_spans_renders_nested_array_and_object() {
        let spans = json_spans(&json!({"xs": [1, true, null]}));
        assert_eq!(join_text(&spans), r#"{"xs": [1, true, null]}"#);
    }

    #[test]
    fn json_spans_colors_keys_with_accent_and_punctuation_with_muted() {
        let spans = json_spans(&json!({"k": "v"}));
        // First span is `{` (muted), then `"` (muted), then `k` (accent), …
        // Collect (text, fg) pairs and assert against a small expected slice.
        let pairs: Vec<(String, Option<ThemeColor>)> =
            spans.iter().map(|s| (s.text.clone(), s.fg)).collect();
        assert_eq!(pairs[0], ("{".to_string(), Some(ThemeColor::Muted)));
        // `"k"` is emitted as three muted-quote / accent-key spans.
        // We assert the key body is Accent and the surrounding quotes are Muted.
        assert!(
            pairs
                .iter()
                .any(|(t, c)| t == "k" && *c == Some(ThemeColor::Accent)),
            "expected an Accent-coloured span containing the key 'k'; got {pairs:?}"
        );
        assert!(
            pairs
                .iter()
                .any(|(t, c)| t == "v" && *c == Some(ThemeColor::Success)),
            "expected a Success-coloured span containing the string value 'v'; got {pairs:?}"
        );
    }

    #[test]
    fn styled_line_holds_owned_spans() {
        let line = StyledLine {
            spans: vec![StyledSpan {
                text: "hello".to_string(),
                fg: Some(ThemeColor::Green),
                bg: None,
                modifiers: TextMods {
                    bold: true,
                    ..Default::default()
                },
            }],
        };
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].text, "hello");
    }

    #[test]
    fn theme_color_supports_named_indexed_and_rgb() {
        let _named = ThemeColor::Red;
        let _idx = ThemeColor::Indexed(208);
        let _rgb = ThemeColor::Rgb {
            r: 255,
            g: 128,
            b: 64,
        };
    }

    #[test]
    fn text_mods_default_is_all_false() {
        let m = TextMods::default();
        assert!(!m.bold);
        assert!(!m.italic);
        assert!(!m.underline);
        assert!(!m.reverse);
        assert!(!m.dim);
    }
}
