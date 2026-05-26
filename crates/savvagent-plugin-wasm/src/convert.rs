//! Free-function conversions between WIT-generated bindings and
//! `savvagent_plugin` data types.
//!
//! Why free functions instead of `From`/`Into`: the WIT-bindgen types live
//! in `savvagent-plugin-wasm` (this crate, where the
//! `wasmtime::component::bindgen!` macro expands) and the Rust trait-surface
//! types live in the upstream `savvagent-plugin` crate. Both sides are
//! "foreign" from each other's perspective per Rust's orphan rules, so
//! `impl From<wit::X> for sp::Y` would not compile.
//!
//! All conversions are total: every WIT variant has a matching arm in its
//! Rust counterpart. The WIT contract is a strict subset of the Rust enum
//! (`Effect`, `ThemeColor`); variants Rust has and WIT doesn't (e.g. all of
//! `Effect`'s post-v0.9 variants such as `Quit`, `RegisterInProcessTool`,
//! …; `ThemeColor`'s semantic slots `Fg`/`Bg`/`Accent`/…) are deliberately
//! unreachable from a wasm guest in v0.18.0 and therefore have *no* wit_to
//! arm. The to_wit direction encodes these Rust-only variants as the closest
//! conservative WIT value: semantic theme slots fall back to `ThemeColor::Reset`
//! when serialized into the WIT `current-theme()` map, since the wasm guest
//! has no machinery to render a semantic slot anyway.
//!
//! Tests live alongside each conversion. The contract is "any value
//! produced by `*_from_wit` round-trips through `*_to_wit` byte-equal for
//! WIT-side types"; the Rust-side types may grow new variants without
//! breaking WIT (WIT remains a fixed-shape subset).

use crate::error::WasmPluginError;
use crate::static_world::savvagent::plugin::types as wit;

use savvagent_plugin as sp;
use savvagent_plugin::manifest as spm;
use savvagent_plugin::styled as sps;

// ---------------------------------------------------------------------
// ThemeColor
// ---------------------------------------------------------------------

/// Convert a WIT `ThemeColor` into the Rust `ThemeColor`.
///
/// WIT's enum is a literal subset of Rust's (no semantic slots), so this
/// conversion is total without a catchall.
pub fn theme_color_from_wit(c: wit::ThemeColor) -> sps::ThemeColor {
    match c {
        wit::ThemeColor::Reset => sps::ThemeColor::Default,
        wit::ThemeColor::Black => sps::ThemeColor::Black,
        wit::ThemeColor::Red => sps::ThemeColor::Red,
        wit::ThemeColor::Green => sps::ThemeColor::Green,
        wit::ThemeColor::Yellow => sps::ThemeColor::Yellow,
        wit::ThemeColor::Blue => sps::ThemeColor::Blue,
        wit::ThemeColor::Magenta => sps::ThemeColor::Magenta,
        wit::ThemeColor::Cyan => sps::ThemeColor::Cyan,
        wit::ThemeColor::Gray => sps::ThemeColor::Gray,
        wit::ThemeColor::DarkGray => sps::ThemeColor::DarkGray,
        wit::ThemeColor::LightRed => sps::ThemeColor::LightRed,
        wit::ThemeColor::LightGreen => sps::ThemeColor::LightGreen,
        wit::ThemeColor::LightYellow => sps::ThemeColor::LightYellow,
        wit::ThemeColor::LightBlue => sps::ThemeColor::LightBlue,
        wit::ThemeColor::LightMagenta => sps::ThemeColor::LightMagenta,
        wit::ThemeColor::LightCyan => sps::ThemeColor::LightCyan,
        wit::ThemeColor::White => sps::ThemeColor::White,
        wit::ThemeColor::Indexed(i) => sps::ThemeColor::Indexed(i),
        wit::ThemeColor::Rgb(rgb) => sps::ThemeColor::Rgb {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        },
    }
}

/// Convert a Rust `ThemeColor` into a WIT `ThemeColor`.
///
/// Semantic slots (`Fg`, `Bg`, `Accent`, …) are not representable in WIT
/// and collapse to `Reset` here. The `current-theme()` host import resolves
/// semantic slots against the active palette *before* exporting the map to
/// wasm, so guests never see a semantic slot in practice.
pub fn theme_color_to_wit(c: sps::ThemeColor) -> wit::ThemeColor {
    match c {
        sps::ThemeColor::Default => wit::ThemeColor::Reset,
        sps::ThemeColor::Black => wit::ThemeColor::Black,
        sps::ThemeColor::Red => wit::ThemeColor::Red,
        sps::ThemeColor::Green => wit::ThemeColor::Green,
        sps::ThemeColor::Yellow => wit::ThemeColor::Yellow,
        sps::ThemeColor::Blue => wit::ThemeColor::Blue,
        sps::ThemeColor::Magenta => wit::ThemeColor::Magenta,
        sps::ThemeColor::Cyan => wit::ThemeColor::Cyan,
        sps::ThemeColor::White => wit::ThemeColor::White,
        sps::ThemeColor::Gray => wit::ThemeColor::Gray,
        sps::ThemeColor::DarkGray => wit::ThemeColor::DarkGray,
        sps::ThemeColor::LightRed => wit::ThemeColor::LightRed,
        sps::ThemeColor::LightGreen => wit::ThemeColor::LightGreen,
        sps::ThemeColor::LightYellow => wit::ThemeColor::LightYellow,
        sps::ThemeColor::LightBlue => wit::ThemeColor::LightBlue,
        sps::ThemeColor::LightMagenta => wit::ThemeColor::LightMagenta,
        sps::ThemeColor::LightCyan => wit::ThemeColor::LightCyan,
        sps::ThemeColor::Indexed(i) => wit::ThemeColor::Indexed(i),
        sps::ThemeColor::Rgb { r, g, b } => wit::ThemeColor::Rgb(wit::RgbColor { r, g, b }),
        // Semantic slots — unrepresentable in WIT. The host should resolve
        // these against the active palette before they ever reach the
        // boundary. We collapse to Reset as a safe fallback.
        sps::ThemeColor::Fg
        | sps::ThemeColor::Bg
        | sps::ThemeColor::Accent
        | sps::ThemeColor::Muted
        | sps::ThemeColor::Error
        | sps::ThemeColor::Warning
        | sps::ThemeColor::Success
        | sps::ThemeColor::Secondary
        | sps::ThemeColor::Border => wit::ThemeColor::Reset,
        // `ThemeColor` is `#[non_exhaustive]` so a future minor release
        // may add new semantic slots. Until convert.rs grows a matching
        // arm we route those through Reset — the same conservative
        // default semantic slots get today.
        _ => wit::ThemeColor::Reset,
    }
}

// ---------------------------------------------------------------------
// TextMods
// ---------------------------------------------------------------------

/// Convert a WIT `TextMods` into the Rust `TextMods`.
pub fn text_mods_from_wit(m: wit::TextMods) -> sps::TextMods {
    sps::TextMods {
        bold: m.bold,
        italic: m.italic,
        underline: m.underline,
        reverse: m.reverse,
        dim: m.dim,
    }
}

/// Convert a Rust `TextMods` into a WIT `TextMods`.
pub fn text_mods_to_wit(m: sps::TextMods) -> wit::TextMods {
    wit::TextMods {
        bold: m.bold,
        italic: m.italic,
        underline: m.underline,
        reverse: m.reverse,
        dim: m.dim,
    }
}

// ---------------------------------------------------------------------
// StyledSpan / StyledLine
// ---------------------------------------------------------------------

/// Convert a WIT `StyledSpan` into a Rust `StyledSpan`.
///
/// WIT has no equivalent of `Option<ThemeColor>`; we treat `Reset` as `None`
/// on the way back so `theme_color_from_wit(Reset)` maps to "inherit theme".
pub fn styled_span_from_wit(s: wit::StyledSpan) -> sps::StyledSpan {
    sps::StyledSpan {
        text: s.text,
        fg: option_from_wit_color(s.fg),
        bg: option_from_wit_color(s.bg),
        modifiers: text_mods_from_wit(s.mods),
    }
}

/// Convert a Rust `StyledSpan` into a WIT `StyledSpan`.
///
/// `None` fg/bg collapse to `Reset` (the WIT-side equivalent of "inherit
/// from the active theme").
pub fn styled_span_to_wit(s: sps::StyledSpan) -> wit::StyledSpan {
    wit::StyledSpan {
        text: s.text,
        fg: s
            .fg
            .map(theme_color_to_wit)
            .unwrap_or(wit::ThemeColor::Reset),
        bg: s
            .bg
            .map(theme_color_to_wit)
            .unwrap_or(wit::ThemeColor::Reset),
        mods: text_mods_to_wit(s.modifiers),
    }
}

/// Convert a WIT `StyledLine` into a Rust `StyledLine`.
pub fn styled_line_from_wit(l: wit::StyledLine) -> sps::StyledLine {
    sps::StyledLine {
        spans: l.spans.into_iter().map(styled_span_from_wit).collect(),
    }
}

/// Convert a Rust `StyledLine` into a WIT `StyledLine`.
pub fn styled_line_to_wit(l: sps::StyledLine) -> wit::StyledLine {
    wit::StyledLine {
        spans: l.spans.into_iter().map(styled_span_to_wit).collect(),
    }
}

fn option_from_wit_color(c: wit::ThemeColor) -> Option<sps::ThemeColor> {
    match c {
        wit::ThemeColor::Reset => None,
        other => Some(theme_color_from_wit(other)),
    }
}

// ---------------------------------------------------------------------
// HookKind
// ---------------------------------------------------------------------

/// Convert a WIT `HookKind` into a Rust `HookKind`.
pub fn hook_kind_from_wit(h: wit::HookKind) -> sp::HookKind {
    match h {
        wit::HookKind::HostStarting => sp::HookKind::HostStarting,
        wit::HookKind::Connect => sp::HookKind::Connect,
        wit::HookKind::Disconnect => sp::HookKind::Disconnect,
        wit::HookKind::TurnStart => sp::HookKind::TurnStart,
        wit::HookKind::TurnEnd => sp::HookKind::TurnEnd,
        wit::HookKind::ToolCallStart => sp::HookKind::ToolCallStart,
        wit::HookKind::ToolCallEnd => sp::HookKind::ToolCallEnd,
        wit::HookKind::PromptSubmitted => sp::HookKind::PromptSubmitted,
        wit::HookKind::TranscriptSaved => sp::HookKind::TranscriptSaved,
        wit::HookKind::ProviderRegistered => sp::HookKind::ProviderRegistered,
        wit::HookKind::ContextSizeChanged => sp::HookKind::ContextSizeChanged,
        wit::HookKind::ActiveProviderChanged => sp::HookKind::ActiveProviderChanged,
        wit::HookKind::SubagentStop => sp::HookKind::SubagentStop,
    }
}

/// Convert a Rust `HookKind` into a WIT `HookKind`.
pub fn hook_kind_to_wit(h: sp::HookKind) -> wit::HookKind {
    match h {
        sp::HookKind::HostStarting => wit::HookKind::HostStarting,
        sp::HookKind::Connect => wit::HookKind::Connect,
        sp::HookKind::Disconnect => wit::HookKind::Disconnect,
        sp::HookKind::TurnStart => wit::HookKind::TurnStart,
        sp::HookKind::TurnEnd => wit::HookKind::TurnEnd,
        sp::HookKind::ToolCallStart => wit::HookKind::ToolCallStart,
        sp::HookKind::ToolCallEnd => wit::HookKind::ToolCallEnd,
        sp::HookKind::PromptSubmitted => wit::HookKind::PromptSubmitted,
        sp::HookKind::TranscriptSaved => wit::HookKind::TranscriptSaved,
        sp::HookKind::ProviderRegistered => wit::HookKind::ProviderRegistered,
        sp::HookKind::ContextSizeChanged => wit::HookKind::ContextSizeChanged,
        sp::HookKind::ActiveProviderChanged => wit::HookKind::ActiveProviderChanged,
        sp::HookKind::SubagentStop => wit::HookKind::SubagentStop,
    }
}

// ---------------------------------------------------------------------
// PluginKind
// ---------------------------------------------------------------------

/// Convert a WIT `PluginKind` into a Rust `PluginKind`.
pub fn plugin_kind_from_wit(k: wit::PluginKind) -> spm::PluginKind {
    match k {
        wit::PluginKind::Core => spm::PluginKind::Core,
        wit::PluginKind::Optional => spm::PluginKind::Optional,
    }
}

/// Convert a Rust `PluginKind` into a WIT `PluginKind`.
pub fn plugin_kind_to_wit(k: spm::PluginKind) -> wit::PluginKind {
    match k {
        spm::PluginKind::Core => wit::PluginKind::Core,
        spm::PluginKind::Optional => wit::PluginKind::Optional,
    }
}

// ---------------------------------------------------------------------
// NoteLevel + Note (Effect::PushNote payload)
// ---------------------------------------------------------------------

/// WIT note-level discriminant; mirrored by Rust as a styled-line modifier
/// rather than a first-class enum, so the conversion lives inside
/// [`effect_from_wit`].
fn note_level_to_color(level: wit::NoteLevel) -> sps::ThemeColor {
    match level {
        wit::NoteLevel::Info => sps::ThemeColor::Default,
        wit::NoteLevel::Warning => sps::ThemeColor::Warning,
        wit::NoteLevel::Error => sps::ThemeColor::Error,
    }
}

fn note_to_styled_line(n: wit::Note) -> sps::StyledLine {
    let fg = note_level_to_color(n.level);
    sps::StyledLine {
        spans: vec![sps::StyledSpan {
            text: n.text,
            fg: if matches!(fg, sps::ThemeColor::Default) {
                None
            } else {
                Some(fg)
            },
            bg: None,
            modifiers: sps::TextMods::default(),
        }],
    }
}

// ---------------------------------------------------------------------
// ThemeEntry
// ---------------------------------------------------------------------

/// Convert a WIT `ThemeEntry` into a Rust `ThemeEntry`.
///
/// The WIT-side `theme-entry` ships a flat `(name, color)` list rather than
/// the Rust-side typed `ThemePalette`. We map the first four named slots
/// (`bg`, `fg`, `accent`, `muted`) into the palette and drop the rest;
/// downstream theme code that needs additional slots reads them out of the
/// raw map at registration time.
pub fn theme_entry_from_wit(t: wit::ThemeEntry) -> sp::ThemeEntry {
    let mut bg = sps::ThemeColor::Default;
    let mut fg = sps::ThemeColor::Default;
    let mut accent = sps::ThemeColor::Default;
    let mut muted = sps::ThemeColor::Default;
    for (name, color) in &t.colors {
        let c = theme_color_from_wit(*color);
        match name.as_str() {
            "bg" => bg = c,
            "fg" => fg = c,
            "accent" => accent = c,
            "muted" => muted = c,
            _ => {}
        }
    }
    sp::ThemeEntry {
        slug: t.slug,
        label: t.name,
        // WIT side currently has no "dark" bit; default to false. A future
        // WIT-version bump may add it; until then theme authors must rely
        // on the slug naming convention.
        dark: false,
        palette: sp::ThemePalette::new(bg, fg, accent, muted),
    }
}

/// Convert a Rust `ThemeEntry` into a WIT `ThemeEntry`.
pub fn theme_entry_to_wit(t: sp::ThemeEntry) -> wit::ThemeEntry {
    wit::ThemeEntry {
        slug: t.slug,
        name: t.label,
        colors: vec![
            ("bg".into(), theme_color_to_wit(t.palette.bg)),
            ("fg".into(), theme_color_to_wit(t.palette.fg)),
            ("accent".into(), theme_color_to_wit(t.palette.accent)),
            ("muted".into(), theme_color_to_wit(t.palette.muted)),
        ],
    }
}

// ---------------------------------------------------------------------
// Region
// ---------------------------------------------------------------------

/// Convert a WIT `Region` into a Rust `Region`.
pub fn region_from_wit(r: wit::Region) -> sp::Region {
    sp::Region {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

/// Convert a Rust `Region` into a WIT `Region`.
pub fn region_to_wit(r: sp::Region) -> wit::Region {
    wit::Region {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

// ---------------------------------------------------------------------
// Contributions
// ---------------------------------------------------------------------

/// Convert a WIT `Contributions` into a Rust `Contributions`.
///
/// Keybindings on the WIT side are deliberately not surfaced through the
/// runtime in v0.18.0: the trait surface `KeybindingSpec` carries an
/// in-process [`sp::BoundAction`] that the WIT-side `Keybinding` cannot
/// fully represent. Plugin-supplied keybindings are an out-of-band feature
/// to land in a later release.
pub fn contributions_from_wit(c: wit::Contributions) -> spm::Contributions {
    // `Contributions` is `#[non_exhaustive]` in `savvagent-plugin` and that
    // crate lives outside this one — Rust forbids struct-expression
    // construction (even with `..Default::default()`) across the crate
    // boundary, so we start from `Default::default()` and assign fields.
    let mut out = spm::Contributions::default();
    out.slash_commands = c
        .slash_commands
        .into_iter()
        .map(|name| spm::SlashSpec {
            name,
            summary: String::new(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: Vec::new(),
        })
        .collect();
    out.screens = c
        .screens
        .into_iter()
        .map(|id| spm::ScreenSpec {
            id,
            layout: spm::ScreenLayout::Fullscreen { hide_chrome: false },
        })
        .collect();
    out.hooks = c.hooks.into_iter().map(hook_kind_from_wit).collect();
    out.slots = c
        .render_slots
        .into_iter()
        .map(|slot_id| spm::SlotSpec {
            slot_id,
            priority: 100,
        })
        .collect();
    out
}

// ---------------------------------------------------------------------
// PluginManifest
// ---------------------------------------------------------------------

/// Translate a disk-side plugin id (`<org>.<name>`, lowercase-kebab on both
/// segments per `crate::manifest::validate_id`) into the trait-surface form
/// (`<vendor>:<rest>`) that [`sp::PluginId`] expects.
///
/// The transformation is total once the disk validator has accepted the
/// id: it splits on the first `.` and joins on `:`. Built-in plugins use
/// `:` directly (`internal:themes`); external plugins use `.` on disk
/// (`acme.demo`) and we surface them in the runtime as `acme:demo`.
pub fn disk_id_to_plugin_id(disk_id: &str) -> Result<sp::PluginId, WasmPluginError> {
    let runtime_id = match disk_id.split_once('.') {
        Some((vendor, rest)) => format!("{vendor}:{rest}"),
        None if disk_id.contains(':') => disk_id.to_string(),
        None => {
            return Err(WasmPluginError::InvalidId(
                disk_id.to_string(),
                "external plugin id must contain a '.' separator".to_string(),
            ));
        }
    };
    sp::PluginId::new(&runtime_id)
        .map_err(|e| WasmPluginError::InvalidId(disk_id.to_string(), e.to_string()))
}

/// Convert a WIT `PluginManifest` into the Rust trait-surface `Manifest`.
///
/// The WIT-side `id` mirrors the on-disk form (`<org>.<name>`); this
/// converter translates it to the runtime form (`<vendor>:<rest>`) via
/// [`disk_id_to_plugin_id`]. An invalid id surfaces as
/// [`WasmPluginError::InvalidId`].
pub fn manifest_from_wit(m: wit::PluginManifest) -> Result<spm::Manifest, WasmPluginError> {
    let id = disk_id_to_plugin_id(&m.id)?;
    Ok(spm::Manifest {
        id,
        name: m.name,
        version: m.version,
        description: m.description,
        kind: plugin_kind_from_wit(m.kind),
        contributions: contributions_from_wit(m.contributions),
    })
}

// ---------------------------------------------------------------------
// Effect (the closed runtime vocabulary the static world emits)
// ---------------------------------------------------------------------

/// Convert a WIT `Effect` into a Rust `Effect`.
///
/// The WIT-side variant set is a strict subset of the Rust enum; arms here
/// cover exactly those variants the WIT contract declares. WIT's
/// `register-keybinding` variant exists for forward compatibility but is
/// not consumed at runtime in v0.18.0 — see the doc on
/// [`contributions_from_wit`] for why.
pub fn effect_from_wit(e: wit::Effect) -> Result<sp::Effect, WasmPluginError> {
    match e {
        wit::Effect::PushNote(n) => Ok(sp::Effect::PushNote {
            line: note_to_styled_line(n),
        }),
        wit::Effect::OpenScreen(t) => {
            // The plugin-side WIT does not yet model the typed
            // `ScreenArgs` discriminants; it carries a serialized JSON
            // shape. We accept any well-formed JSON for forward
            // compatibility, but a v0.18.0 wasm plugin will typically emit
            // `"{}"` — round-trip via `ScreenArgs::None` if it's empty.
            //
            // The WIT-side `ScreenTarget` carries `plugin_id` separately
            // for future routing (Task 6/9 cross-plugin OpenScreen); for
            // now we only honour `screen_id` and require args_json to be
            // a JSON value we can map to `ScreenArgs::None` (the only
            // shape that has no required fields).
            //
            // Forward-compat note: a future ScreenArgs deserializer can
            // replace `_args_json` here; until then the host treats every
            // plugin-issued `OpenScreen` as `ScreenArgs::None` and lets
            // the screen factory pull its own state from elsewhere.
            let _args_json = t.args_json;
            let _plugin_id = t.plugin_id;
            Ok(sp::Effect::OpenScreen {
                id: t.screen_id,
                args: sp::ScreenArgs::None,
            })
        }
        wit::Effect::SetTheme(slug) => Ok(sp::Effect::SetActiveTheme {
            slug,
            persist: false,
        }),
        wit::Effect::RunSlash(s) => Ok(sp::Effect::RunSlash {
            name: s.name,
            args: s.args,
        }),
        wit::Effect::SaveTranscript => Ok(sp::Effect::SaveTranscript {
            // WIT has no path argument; the host resolves the default
            // location at apply-effect time.
            path: String::new(),
        }),
        wit::Effect::ClearLog => Ok(sp::Effect::ClearLog),
        wit::Effect::RegisterKeybinding(_) => Err(WasmPluginError::CapabilityDenied(
            "Effect::RegisterKeybinding is not honored by the v0.18.0 runtime; \
             plugins must declare keybindings statically via the manifest"
                .to_string(),
        )),
    }
}

// ---------------------------------------------------------------------
// PluginError (the WIT-side error variant returned from result<_, …>)
// ---------------------------------------------------------------------

/// Convert a WIT `PluginError` into a Rust `PluginError`.
pub fn plugin_error_from_wit(e: wit::PluginError) -> sp::PluginError {
    match e {
        wit::PluginError::InvalidInput(s) => sp::PluginError::InvalidArgs(s),
        wit::PluginError::Io(s) => sp::PluginError::Internal(format!("io: {s}")),
        wit::PluginError::CapabilityDenied(s) => {
            sp::PluginError::Internal(format!("capability denied: {s}"))
        }
        wit::PluginError::Unsupported(s) => sp::PluginError::Internal(format!("unsupported: {s}")),
        wit::PluginError::ScreenNotFound(s) => sp::PluginError::ScreenNotFound(s),
    }
}

// ---------------------------------------------------------------------
// LogLevel
// ---------------------------------------------------------------------

/// Convert a WIT `LogLevel` into a `tracing::Level` for the host's
/// structured-logging pipeline.
pub fn log_level_to_tracing(l: wit::LogLevel) -> tracing::Level {
    match l {
        wit::LogLevel::Trace => tracing::Level::TRACE,
        wit::LogLevel::Debug => tracing::Level::DEBUG,
        wit::LogLevel::Info => tracing::Level::INFO,
        wit::LogLevel::Warn => tracing::Level::WARN,
        wit::LogLevel::Error => tracing::Level::ERROR,
    }
}

// ---------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_color_round_trips_for_every_wit_variant() {
        let variants: Vec<wit::ThemeColor> = vec![
            wit::ThemeColor::Reset,
            wit::ThemeColor::Black,
            wit::ThemeColor::Red,
            wit::ThemeColor::Green,
            wit::ThemeColor::Yellow,
            wit::ThemeColor::Blue,
            wit::ThemeColor::Magenta,
            wit::ThemeColor::Cyan,
            wit::ThemeColor::Gray,
            wit::ThemeColor::DarkGray,
            wit::ThemeColor::LightRed,
            wit::ThemeColor::LightGreen,
            wit::ThemeColor::LightYellow,
            wit::ThemeColor::LightBlue,
            wit::ThemeColor::LightMagenta,
            wit::ThemeColor::LightCyan,
            wit::ThemeColor::White,
            wit::ThemeColor::Indexed(42),
            wit::ThemeColor::Rgb(wit::RgbColor { r: 1, g: 2, b: 3 }),
        ];
        for v in variants {
            let rust = theme_color_from_wit(v);
            let back = theme_color_to_wit(rust);
            assert!(matches!(
                (v, back),
                (wit::ThemeColor::Reset, wit::ThemeColor::Reset)
                    | (wit::ThemeColor::Black, wit::ThemeColor::Black)
                    | (wit::ThemeColor::Red, wit::ThemeColor::Red)
                    | (wit::ThemeColor::Green, wit::ThemeColor::Green)
                    | (wit::ThemeColor::Yellow, wit::ThemeColor::Yellow)
                    | (wit::ThemeColor::Blue, wit::ThemeColor::Blue)
                    | (wit::ThemeColor::Magenta, wit::ThemeColor::Magenta)
                    | (wit::ThemeColor::Cyan, wit::ThemeColor::Cyan)
                    | (wit::ThemeColor::Gray, wit::ThemeColor::Gray)
                    | (wit::ThemeColor::DarkGray, wit::ThemeColor::DarkGray)
                    | (wit::ThemeColor::LightRed, wit::ThemeColor::LightRed)
                    | (wit::ThemeColor::LightGreen, wit::ThemeColor::LightGreen)
                    | (wit::ThemeColor::LightYellow, wit::ThemeColor::LightYellow)
                    | (wit::ThemeColor::LightBlue, wit::ThemeColor::LightBlue)
                    | (wit::ThemeColor::LightMagenta, wit::ThemeColor::LightMagenta)
                    | (wit::ThemeColor::LightCyan, wit::ThemeColor::LightCyan)
                    | (wit::ThemeColor::White, wit::ThemeColor::White)
                    | (wit::ThemeColor::Indexed(_), wit::ThemeColor::Indexed(_))
                    | (wit::ThemeColor::Rgb(_), wit::ThemeColor::Rgb(_))
            ));
        }
    }

    #[test]
    fn semantic_theme_slots_collapse_to_reset() {
        let slots = [
            sps::ThemeColor::Fg,
            sps::ThemeColor::Bg,
            sps::ThemeColor::Accent,
            sps::ThemeColor::Muted,
            sps::ThemeColor::Error,
            sps::ThemeColor::Warning,
            sps::ThemeColor::Success,
            sps::ThemeColor::Secondary,
            sps::ThemeColor::Border,
        ];
        for s in slots {
            assert!(matches!(theme_color_to_wit(s), wit::ThemeColor::Reset));
        }
    }

    #[test]
    fn text_mods_round_trips() {
        let m = sps::TextMods {
            bold: true,
            italic: false,
            underline: true,
            reverse: false,
            dim: true,
        };
        let back = text_mods_from_wit(text_mods_to_wit(m));
        assert!(back.bold);
        assert!(!back.italic);
        assert!(back.underline);
        assert!(!back.reverse);
        assert!(back.dim);
    }

    #[test]
    fn styled_span_round_trips_with_colors() {
        let span = sps::StyledSpan {
            text: "hello".into(),
            fg: Some(sps::ThemeColor::Red),
            bg: Some(sps::ThemeColor::Blue),
            modifiers: sps::TextMods {
                bold: true,
                ..Default::default()
            },
        };
        let wit_side = styled_span_to_wit(span.clone());
        let back = styled_span_from_wit(wit_side);
        assert_eq!(back.text, span.text);
        assert_eq!(back.fg, span.fg);
        assert_eq!(back.bg, span.bg);
        assert_eq!(back.modifiers.bold, span.modifiers.bold);
    }

    #[test]
    fn styled_span_none_collapses_to_reset_round_trip_yields_none() {
        let span = sps::StyledSpan {
            text: "x".into(),
            fg: None,
            bg: None,
            modifiers: sps::TextMods::default(),
        };
        let wit_side = styled_span_to_wit(span);
        let back = styled_span_from_wit(wit_side);
        assert_eq!(back.fg, None);
        assert_eq!(back.bg, None);
    }

    #[test]
    fn styled_line_round_trips() {
        let line = sps::StyledLine {
            spans: vec![
                sps::StyledSpan {
                    text: "a".into(),
                    fg: Some(sps::ThemeColor::Green),
                    bg: None,
                    modifiers: sps::TextMods::default(),
                },
                sps::StyledSpan {
                    text: "b".into(),
                    fg: None,
                    bg: Some(sps::ThemeColor::Black),
                    modifiers: sps::TextMods::default(),
                },
            ],
        };
        let back = styled_line_from_wit(styled_line_to_wit(line.clone()));
        assert_eq!(back.spans.len(), 2);
        assert_eq!(back.spans[0].text, "a");
        assert_eq!(back.spans[1].text, "b");
    }

    #[test]
    fn hook_kind_round_trips_for_every_variant() {
        let cases = [
            sp::HookKind::HostStarting,
            sp::HookKind::Connect,
            sp::HookKind::Disconnect,
            sp::HookKind::TurnStart,
            sp::HookKind::TurnEnd,
            sp::HookKind::ToolCallStart,
            sp::HookKind::ToolCallEnd,
            sp::HookKind::PromptSubmitted,
            sp::HookKind::TranscriptSaved,
            sp::HookKind::ProviderRegistered,
            sp::HookKind::ContextSizeChanged,
            sp::HookKind::ActiveProviderChanged,
            sp::HookKind::SubagentStop,
        ];
        for h in cases {
            assert_eq!(hook_kind_from_wit(hook_kind_to_wit(h.clone())), h);
        }
    }

    #[test]
    fn plugin_kind_round_trips() {
        assert_eq!(
            plugin_kind_from_wit(plugin_kind_to_wit(spm::PluginKind::Core)),
            spm::PluginKind::Core
        );
        assert_eq!(
            plugin_kind_from_wit(plugin_kind_to_wit(spm::PluginKind::Optional)),
            spm::PluginKind::Optional
        );
    }

    #[test]
    fn region_round_trips() {
        let r = sp::Region {
            x: 10,
            y: 20,
            width: 80,
            height: 24,
        };
        let back = region_from_wit(region_to_wit(r));
        assert_eq!(back, r);
    }

    #[test]
    fn theme_entry_round_trips_via_named_palette_slots() {
        let entry = sp::ThemeEntry {
            slug: "dracula".into(),
            label: "Dracula".into(),
            dark: false, // round-trip drops dark flag (WIT doesn't carry it)
            palette: sp::ThemePalette::new(
                sps::ThemeColor::Black,
                sps::ThemeColor::White,
                sps::ThemeColor::Cyan,
                sps::ThemeColor::Gray,
            ),
        };
        let back = theme_entry_from_wit(theme_entry_to_wit(entry.clone()));
        assert_eq!(back.slug, entry.slug);
        assert_eq!(back.label, entry.label);
        assert_eq!(back.palette, entry.palette);
    }

    #[test]
    fn manifest_from_wit_translates_disk_id_to_runtime_id() {
        let m = wit::PluginManifest {
            id: "fixture.static".into(),
            name: "fixture".into(),
            version: "0.1.0".into(),
            description: "desc".into(),
            kind: wit::PluginKind::Optional,
            contributions: wit::Contributions {
                slash_commands: vec!["echo".into()],
                hooks: vec![wit::HookKind::TurnStart],
                screens: vec![],
                render_slots: vec![],
                keybindings: vec![],
                themes: false,
            },
        };
        let rust = manifest_from_wit(m).expect("valid id");
        // `<org>.<name>` becomes `<org>:<name>` for the runtime trait
        // surface.
        assert_eq!(rust.id.as_str(), "fixture:static");
        assert_eq!(rust.contributions.slash_commands.len(), 1);
        assert_eq!(rust.contributions.slash_commands[0].name, "echo");
        assert_eq!(rust.contributions.hooks, vec![sp::HookKind::TurnStart]);
    }

    #[test]
    fn manifest_from_wit_rejects_bad_id() {
        let m = wit::PluginManifest {
            id: "no_dot_no_colon".into(),
            name: "fixture".into(),
            version: "0.1.0".into(),
            description: "desc".into(),
            kind: wit::PluginKind::Optional,
            contributions: wit::Contributions {
                slash_commands: vec![],
                hooks: vec![],
                screens: vec![],
                render_slots: vec![],
                keybindings: vec![],
                themes: false,
            },
        };
        let err = manifest_from_wit(m).unwrap_err();
        match err {
            WasmPluginError::InvalidId(id, _) => assert_eq!(id, "no_dot_no_colon"),
            other => panic!("expected InvalidId, got {other:?}"),
        }
    }

    #[test]
    fn disk_id_to_plugin_id_handles_internal_colon_form() {
        // Built-ins already use `:`; the conversion is a no-op.
        let id = disk_id_to_plugin_id("internal:themes").expect("valid");
        assert_eq!(id.as_str(), "internal:themes");
    }

    #[test]
    fn disk_id_to_plugin_id_handles_external_dot_form() {
        let id = disk_id_to_plugin_id("acme.demo").expect("valid");
        assert_eq!(id.as_str(), "acme:demo");
    }

    #[test]
    fn disk_id_to_plugin_id_rejects_no_separator() {
        let err = disk_id_to_plugin_id("foo").unwrap_err();
        match err {
            WasmPluginError::InvalidId(id, _) => assert_eq!(id, "foo"),
            other => panic!("expected InvalidId, got {other:?}"),
        }
    }

    #[test]
    fn effect_push_note_carries_text_and_level_color() {
        let e = wit::Effect::PushNote(wit::Note {
            text: "boom".into(),
            level: wit::NoteLevel::Error,
        });
        let r = effect_from_wit(e).expect("conversion");
        match r {
            sp::Effect::PushNote { line } => {
                assert_eq!(line.spans.len(), 1);
                assert_eq!(line.spans[0].text, "boom");
                assert_eq!(line.spans[0].fg, Some(sps::ThemeColor::Error));
            }
            other => panic!("expected PushNote, got {other:?}"),
        }
    }

    #[test]
    fn effect_push_note_info_has_no_explicit_color() {
        let e = wit::Effect::PushNote(wit::Note {
            text: "hi".into(),
            level: wit::NoteLevel::Info,
        });
        let r = effect_from_wit(e).expect("conversion");
        match r {
            sp::Effect::PushNote { line } => {
                assert_eq!(line.spans[0].fg, None, "Info level inherits theme fg");
            }
            other => panic!("expected PushNote, got {other:?}"),
        }
    }

    #[test]
    fn effect_open_screen_drops_args_json_to_none() {
        let e = wit::Effect::OpenScreen(wit::ScreenTarget {
            plugin_id: "internal:themes".into(),
            screen_id: "themes.picker".into(),
            args_json: r#"{"current_slug":"dark"}"#.into(),
        });
        let r = effect_from_wit(e).expect("conversion");
        match r {
            sp::Effect::OpenScreen { id, args } => {
                assert_eq!(id, "themes.picker");
                assert!(matches!(args, sp::ScreenArgs::None));
            }
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    #[test]
    fn effect_set_theme_returns_set_active_theme_non_persisting() {
        let e = wit::Effect::SetTheme("dracula".into());
        let r = effect_from_wit(e).expect("conversion");
        match r {
            sp::Effect::SetActiveTheme { slug, persist } => {
                assert_eq!(slug, "dracula");
                assert!(!persist);
            }
            other => panic!("expected SetActiveTheme, got {other:?}"),
        }
    }

    #[test]
    fn effect_run_slash_preserves_name_and_args() {
        let e = wit::Effect::RunSlash(wit::SlashCall {
            name: "view".into(),
            args: vec!["README.md".into()],
        });
        let r = effect_from_wit(e).expect("conversion");
        match r {
            sp::Effect::RunSlash { name, args } => {
                assert_eq!(name, "view");
                assert_eq!(args, vec!["README.md".to_string()]);
            }
            other => panic!("expected RunSlash, got {other:?}"),
        }
    }

    #[test]
    fn effect_save_transcript_produces_empty_path_for_host_resolution() {
        let e = wit::Effect::SaveTranscript;
        let r = effect_from_wit(e).expect("conversion");
        match r {
            sp::Effect::SaveTranscript { path } => assert_eq!(path, ""),
            other => panic!("expected SaveTranscript, got {other:?}"),
        }
    }

    #[test]
    fn effect_clear_log_constructs() {
        let r = effect_from_wit(wit::Effect::ClearLog).expect("conversion");
        assert!(matches!(r, sp::Effect::ClearLog));
    }

    #[test]
    fn effect_register_keybinding_rejected_in_v0_18_0() {
        let e = wit::Effect::RegisterKeybinding(wit::Keybinding {
            key: wit::KeyEventPortable {
                code: wit::KeyCode::Char("s".into()),
                modifiers: wit::KeyModifiers {
                    ctrl: true,
                    shift: false,
                    alt: false,
                    meta: false,
                },
            },
            action: wit::KeybindingAction::EmitEffect(wit::EffectName::SaveTranscriptAction),
        });
        let err = effect_from_wit(e).unwrap_err();
        assert!(matches!(err, WasmPluginError::CapabilityDenied(_)));
    }

    #[test]
    fn plugin_error_invalid_input_maps_to_invalid_args() {
        let r = plugin_error_from_wit(wit::PluginError::InvalidInput("bad".into()));
        assert_eq!(r, sp::PluginError::InvalidArgs("bad".into()));
    }

    #[test]
    fn plugin_error_screen_not_found_round_trips_identifier() {
        let r = plugin_error_from_wit(wit::PluginError::ScreenNotFound("themes.picker".into()));
        assert_eq!(r, sp::PluginError::ScreenNotFound("themes.picker".into()));
    }

    #[test]
    fn log_level_maps_every_variant_to_tracing() {
        assert_eq!(
            log_level_to_tracing(wit::LogLevel::Trace),
            tracing::Level::TRACE
        );
        assert_eq!(
            log_level_to_tracing(wit::LogLevel::Debug),
            tracing::Level::DEBUG
        );
        assert_eq!(
            log_level_to_tracing(wit::LogLevel::Info),
            tracing::Level::INFO
        );
        assert_eq!(
            log_level_to_tracing(wit::LogLevel::Warn),
            tracing::Level::WARN
        );
        assert_eq!(
            log_level_to_tracing(wit::LogLevel::Error),
            tracing::Level::ERROR
        );
    }

    #[test]
    fn contributions_lifts_slash_screens_hooks_slots() {
        let c = wit::Contributions {
            slash_commands: vec!["a".into(), "b".into()],
            hooks: vec![wit::HookKind::Connect, wit::HookKind::TurnStart],
            screens: vec!["s1".into()],
            render_slots: vec!["home.tips".into()],
            keybindings: vec![],
            themes: true,
        };
        let r = contributions_from_wit(c);
        assert_eq!(r.slash_commands.len(), 2);
        assert_eq!(r.hooks.len(), 2);
        assert_eq!(r.screens.len(), 1);
        assert_eq!(r.slots.len(), 1);
        assert_eq!(r.slots[0].slot_id, "home.tips");
        // Keybindings are not surfaced from WIT in v0.18.0.
        assert!(r.keybindings.is_empty());
        // Themes catalog comes from `themes()` export, not the contribs bit.
        assert!(r.themes.is_empty());
    }
}
