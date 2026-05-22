//! ID newtypes and small structural types crossing plugin boundaries.

/// Stable identifier for a registered plugin.
///
/// External callers must use [`PluginId::new`] to construct a value; the inner
/// field is `pub(crate)` so direct tuple construction is only available within
/// this crate.  Built-ins use the prefix `internal:` (e.g. `internal:themes`,
/// `internal:provider-anthropic`). Third-party plugins are expected to use a
/// vendor-scoped prefix (e.g. `acme:my-plugin`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginId(pub(crate) String);

impl PluginId {
    /// Construct a validated plugin id. Must be non-empty, must contain at
    /// least one `:` separator, and the part before the first `:` (the
    /// vendor prefix) must match `^[a-z][a-z0-9_-]*$`.
    pub fn new(s: impl Into<String>) -> Result<Self, crate::error::PluginError> {
        let s: String = s.into();
        let Some((prefix, rest)) = s.split_once(':') else {
            return Err(crate::error::PluginError::InvalidArgs(format!(
                "plugin id must contain a vendor-prefix separator ':' — got {s:?}"
            )));
        };
        if prefix.is_empty() || rest.is_empty() {
            return Err(crate::error::PluginError::InvalidArgs(format!(
                "plugin id vendor prefix and suffix must both be non-empty — got {s:?}"
            )));
        }
        let mut chars = prefix.chars();
        let first = chars.next().expect("non-empty checked above");
        if !first.is_ascii_lowercase() {
            return Err(crate::error::PluginError::InvalidArgs(format!(
                "plugin id vendor prefix must start with [a-z] — got {s:?}"
            )));
        }
        if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
            return Err(crate::error::PluginError::InvalidArgs(format!(
                "plugin id vendor prefix must match [a-z][a-z0-9_-]* — got {s:?}"
            )));
        }
        Ok(Self(s))
    }

    /// Borrow the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub use savvagent_protocol::ProviderId;

/// Transparent newtype identifying a live terminal screen instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenInstanceId(
    /// Numeric handle assigned by the TUI runtime.
    pub u32,
);

/// Axis-aligned rectangle within the terminal grid (columns × rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// X coordinate (columns from the terminal left edge).
    pub x: u16,
    /// Y coordinate (rows from the terminal top edge).
    pub y: u16,
    /// Width in columns.
    pub width: u16,
    /// Height in rows.
    pub height: u16,
}

/// Wall-clock instant with sub-second precision, WIT-portable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp {
    /// Seconds since the Unix epoch (may be negative for pre-epoch instants).
    pub secs: i64,
    /// Sub-second component in nanoseconds. Caller must keep this in `0..1_000_000_000`;
    /// values outside that range have unspecified meaning when converted to wall-clock
    /// times in plugin runtimes.
    pub nanos: u32,
}

/// A keyboard event in a runtime-independent shape (no `crossterm` dep).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyEventPortable {
    /// The key code pressed.
    pub code: KeyCodePortable,
    /// Modifier keys held at the time of the event.
    pub modifiers: KeyMods,
}

/// Runtime-independent representation of a key code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyCodePortable {
    /// A printable character. The `char` is the Unicode scalar value as decoded by the terminal.
    Char(char),
    /// The Backspace key.
    Backspace,
    /// The Enter (Return) key.
    Enter,
    /// The Escape key.
    Esc,
    /// The Tab key (forward).
    Tab,
    /// Shift+Tab (reverse tab).
    BackTab,
    /// The Insert key.
    Insert,
    /// The Delete (forward-delete) key.
    Delete,
    /// The Up arrow key.
    Up,
    /// The Down arrow key.
    Down,
    /// The Left arrow key.
    Left,
    /// The Right arrow key.
    Right,
    /// The Home key.
    Home,
    /// The End key.
    End,
    /// The Page Up key.
    PageUp,
    /// The Page Down key.
    PageDown,
    /// A function key. The `u8` is the function-key number (1–12 common; higher if supported).
    F(u8),
    /// Sentinel for terminal-reported key codes that do not correspond to any
    /// other variant (e.g. unsupported function keys, extended key codes that
    /// the runtime hasn't mapped). Plugins should treat this as a no-op unless
    /// they have a specific reason to react.
    Unknown,
}

/// Modifier keys held during a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyMods {
    /// Whether the Control modifier was held.
    pub ctrl: bool,
    /// Whether the Alt (Option on macOS) modifier was held.
    pub alt: bool,
    /// Whether the Shift modifier was held.
    pub shift: bool,
    /// Whether the Meta (Super / Windows / Command) modifier was held.
    pub meta: bool,
}

/// A single-key chord. Reserved for future multi-key chord extension.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ChordPortable {
    /// The key event that forms this chord.
    pub key: KeyEventPortable,
}

impl ChordPortable {
    /// Build a single-key chord. Reserved for future multi-key chord
    /// extension; `#[non_exhaustive]` lets us add new fields without
    /// breaking existing call sites.
    pub fn new(key: KeyEventPortable) -> Self {
        Self { key }
    }
}

use crate::styled::ThemeColor;

/// A theme catalog entry exposed by the `internal:themes` plugin (and any
/// third-party theme plugin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeEntry {
    /// Machine-readable identifier for this theme (e.g. `"light"`, `"dracula"`).
    pub slug: String,
    /// Human-readable display name shown in the theme picker UI.
    pub label: String,
    /// Whether this theme is considered a dark-background theme.
    pub dark: bool,
    /// Color palette associated with this theme.
    pub palette: ThemePalette,
}

/// Minimal palette in PR 1. PR 6 (`internal:themes` extraction) ports the
/// full field set from `crates/savvagent/src/theme.rs` into this struct.
/// Extensions are additive — `non_exhaustive` reserves the ability to grow
/// the palette without breaking trait-surface clients.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ThemePalette {
    /// Primary background color of the theme.
    pub bg: ThemeColor,
    /// Primary foreground (text) color of the theme.
    pub fg: ThemeColor,
    /// Accent color used for highlights and active elements.
    pub accent: ThemeColor,
    /// Muted color used for less prominent text and borders.
    pub muted: ThemeColor,
}

impl ThemePalette {
    /// Constructor that prevents external code from depending on field order.
    /// Required by `#[non_exhaustive]`.
    pub fn new(bg: ThemeColor, fg: ThemeColor, accent: ThemeColor, muted: ThemeColor) -> Self {
        Self {
            bg,
            fg,
            accent,
            muted,
        }
    }
}

/// Catalog entry for one model advertised by the active provider, surfaced
/// in the `/model` picker. Constructed from
/// [`savvagent_protocol::ListModelsResponse::models`] at host bring-up and
/// on every `/model` change so the picker stays in sync with what the
/// provider actually serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    /// Bare model id (e.g. `"gemini-2.5-flash"`, no `"models/"` prefix).
    pub id: String,
    /// Human-readable display name shown in the picker UI. Falls back to
    /// `id` when the provider doesn't return one.
    pub display_name: String,
}

/// Identifying handle for a saved conversation transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptHandle {
    /// Stable, unique identifier for this transcript.
    pub id: String,
    /// Human-readable display label shown in the resume picker UI.
    pub label: String,
    /// Wall-clock time at which this transcript was saved.
    pub saved_at: Timestamp,
}

/// Per-screen open arguments passed across the plugin boundary when a screen is activated.
///
/// Marked `#[non_exhaustive]` so that adding new screens in future PRs does not
/// break existing match arms in plugin crates.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScreenArgs {
    /// No screen-specific args; the screen reads any context it needs from its own factory.
    None,
    /// Open the theme picker, scrolled to the currently-active theme.
    ThemePicker {
        /// Slug of the theme that is currently active (used to pre-select the cursor row).
        current_slug: String,
    },
    /// Open the provider connection picker with no pre-selected entry.
    ConnectPicker,
    /// Open the resume-session picker populated with the given transcript handles.
    ResumePicker {
        /// Ordered list of saved transcripts to display in the picker.
        transcripts: Vec<TranscriptHandle>,
    },
    /// Open a read-only file viewer for the given path.
    ViewFile {
        /// Absolute or workspace-relative path of the file to display.
        path: String,
    },
    /// Open an editor for the given path.
    EditFile {
        /// Absolute or workspace-relative path of the file to edit.
        path: String,
    },
    /// Open the installed-plugins manager screen.
    PluginsManager,
    /// Open the language picker, scrolled to the currently-active locale.
    LanguagePicker {
        /// Code of the locale currently active (used to pre-select the cursor row).
        current_code: String,
    },
    /// Open the model picker, scrolled to the currently-active model.
    ModelPicker {
        /// Id of the model currently active on the connected provider (used
        /// to pre-select the cursor row and render the active-row marker).
        current_id: String,
        /// Catalog of models advertised by the active provider. May be
        /// empty if the provider's `list_models` failed or returned no
        /// entries; the picker renders an explanatory note in that case.
        models: Vec<ModelEntry>,
    },
    /// Open the changelog viewer; takes no parameters today.
    ///
    /// The variant exists rather than reusing [`ScreenArgs::None`] so the
    /// `screen_id()` table stays exhaustive — a future "scroll to a
    /// specific version" feature can land an arg here without breaking
    /// the public surface.
    Changelog,
    /// Open the first-launch migration picker, populated with the detected
    /// provider ids that have stored keyring credentials.
    MigrationPicker {
        /// Provider ids that have stored keyring credentials.
        detected: Vec<ProviderId>,
    },
    /// Open the LSP-installer progress modal with the given catalog ids.
    /// Emitted by the picker's `Confirm` outcome; the screen owns the
    /// per-id install state and the spawned driver task.
    LspInstallProgress {
        /// Catalog ids the picker confirmed (e.g. `"rust-analyzer"`,
        /// `"pyright"`). Unknown ids surface as `Failed` entries inside
        /// the screen so a typo in the picker doesn't disappear silently.
        entry_ids: Vec<String>,
    },
}

impl ScreenArgs {
    /// Returns the canonical screen id this args variant pairs with, or
    /// `None` for the `None` variant.
    ///
    /// Use this when constructing an `Effect::OpenScreen` to keep the
    /// `id` and `args` in sync without hardcoding the same string in
    /// two places:
    ///
    /// ```ignore
    /// let args = ScreenArgs::ThemePicker { current_slug: "dark".into() };
    /// let id = args.screen_id().expect("not the None variant").to_string();
    /// Effect::OpenScreen { id, args }
    /// ```
    pub fn screen_id(&self) -> Option<&'static str> {
        match self {
            ScreenArgs::None => None,
            ScreenArgs::ThemePicker { .. } => Some("themes.picker"),
            ScreenArgs::ConnectPicker => Some("connect.picker"),
            ScreenArgs::ResumePicker { .. } => Some("resume.picker"),
            ScreenArgs::ViewFile { .. } => Some("view-file"),
            ScreenArgs::EditFile { .. } => Some("edit-file"),
            ScreenArgs::PluginsManager => Some("plugins.manager"),
            ScreenArgs::LanguagePicker { .. } => Some("language.picker"),
            ScreenArgs::ModelPicker { .. } => Some("model.picker"),
            ScreenArgs::Changelog => Some("changelog"),
            ScreenArgs::MigrationPicker { .. } => Some("migration.picker"),
            ScreenArgs::LspInstallProgress { .. } => Some("lsp_installer.progress"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_string_newtypes() {
        let p = PluginId("internal:themes".to_string());
        let q = ProviderId::new("anthropic").unwrap();
        assert_eq!(p.0, "internal:themes");
        assert_eq!(q.as_str(), "anthropic");
    }

    #[test]
    fn plugin_id_new_accepts_internal_themes() {
        let id = PluginId::new("internal:themes").unwrap();
        assert_eq!(id.as_str(), "internal:themes");
    }

    #[test]
    fn plugin_id_new_rejects_no_colon() {
        assert!(matches!(
            PluginId::new("themes").unwrap_err(),
            crate::error::PluginError::InvalidArgs(_)
        ));
    }

    #[test]
    fn plugin_id_new_rejects_uppercase_prefix() {
        assert!(matches!(
            PluginId::new("Internal:themes").unwrap_err(),
            crate::error::PluginError::InvalidArgs(_)
        ));
    }

    #[test]
    fn plugin_id_new_rejects_empty_suffix() {
        assert!(matches!(
            PluginId::new("internal:").unwrap_err(),
            crate::error::PluginError::InvalidArgs(_)
        ));
    }

    #[test]
    fn provider_id_new_accepts_anthropic() {
        let id = ProviderId::new("anthropic").unwrap();
        assert_eq!(id.as_str(), "anthropic");
    }

    #[test]
    fn provider_id_new_rejects_uppercase() {
        // ProviderId now lives in savvagent-protocol; its error type is
        // ProviderIdError, not PluginError.
        assert!(ProviderId::new("Anthropic").is_err());
    }

    #[test]
    fn region_fields_are_u16() {
        let r = Region {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let area: u32 = r.width as u32 * r.height as u32;
        assert_eq!(area, 1920);
    }

    #[test]
    fn timestamp_is_i64_secs_plus_u32_nanos() {
        let t = Timestamp {
            secs: 1_700_000_000,
            nanos: 500_000_000,
        };
        assert_eq!(t.secs, 1_700_000_000);
        assert_eq!(t.nanos, 500_000_000);
    }

    #[test]
    fn screen_instance_id_is_u32() {
        let s = ScreenInstanceId(42);
        assert_eq!(s.0, 42u32);
    }

    #[test]
    fn key_event_portable_is_constructible() {
        let k = KeyEventPortable {
            code: KeyCodePortable::Char('a'),
            modifiers: KeyMods {
                ctrl: true,
                alt: false,
                shift: false,
                meta: false,
            },
        };
        assert!(k.modifiers.ctrl);
        match k.code {
            KeyCodePortable::Char(c) => assert_eq!(c, 'a'),
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn chord_portable_wraps_one_key() {
        let c = ChordPortable {
            key: KeyEventPortable {
                code: KeyCodePortable::Char('s'),
                modifiers: KeyMods {
                    ctrl: true,
                    ..Default::default()
                },
            },
        };
        match c.key.code {
            KeyCodePortable::Char(c) => assert_eq!(c, 's'),
            _ => panic!(),
        }
    }

    #[test]
    fn theme_entry_is_constructible() {
        let entry = ThemeEntry {
            slug: "light".to_string(),
            label: "Light".to_string(),
            dark: false,
            palette: ThemePalette {
                bg: crate::styled::ThemeColor::White,
                fg: crate::styled::ThemeColor::Black,
                accent: crate::styled::ThemeColor::Blue,
                muted: crate::styled::ThemeColor::Gray,
            },
        };
        assert_eq!(entry.slug, "light");
        assert!(!entry.dark);
    }

    #[test]
    fn screen_args_variants_are_typed() {
        let _none = ScreenArgs::None;
        let _theme = ScreenArgs::ThemePicker {
            current_slug: "dark".into(),
        };
        let _view = ScreenArgs::ViewFile {
            path: "/tmp/x.rs".into(),
        };
        let _resume = ScreenArgs::ResumePicker {
            transcripts: vec![TranscriptHandle {
                id: "t1".into(),
                label: "yesterday".into(),
                saved_at: Timestamp { secs: 0, nanos: 0 },
            }],
        };
    }

    #[test]
    fn model_entry_is_constructible() {
        let m = ModelEntry {
            id: "gemini-2.5-flash".into(),
            display_name: "Gemini 2.5 Flash".into(),
        };
        assert_eq!(m.id, "gemini-2.5-flash");
        assert_eq!(m.display_name, "Gemini 2.5 Flash");
    }

    #[test]
    fn screen_args_model_picker_pairs_with_model_picker_id() {
        let args = ScreenArgs::ModelPicker {
            current_id: "gemini-2.5-flash".into(),
            models: vec![ModelEntry {
                id: "gemini-2.5-flash".into(),
                display_name: "Gemini 2.5 Flash".into(),
            }],
        };
        assert_eq!(args.screen_id(), Some("model.picker"));
    }

    #[test]
    fn lsp_install_progress_carries_entry_ids() {
        let args = ScreenArgs::LspInstallProgress {
            entry_ids: vec!["rust-analyzer".into(), "pyright".into()],
        };
        match args {
            ScreenArgs::LspInstallProgress { entry_ids } => {
                assert_eq!(entry_ids, vec!["rust-analyzer", "pyright"]);
            }
            _ => panic!("expected LspInstallProgress"),
        }
    }

    #[test]
    fn lsp_install_progress_screen_id_is_lsp_installer_progress() {
        let args = ScreenArgs::LspInstallProgress { entry_ids: vec![] };
        assert_eq!(args.screen_id(), Some("lsp_installer.progress"));
    }

    #[test]
    fn screen_args_screen_id_pairs_every_non_none_variant() {
        assert_eq!(ScreenArgs::None.screen_id(), None);
        assert_eq!(
            ScreenArgs::ThemePicker {
                current_slug: "x".into()
            }
            .screen_id(),
            Some("themes.picker")
        );
        assert_eq!(
            ScreenArgs::ConnectPicker.screen_id(),
            Some("connect.picker")
        );
        assert_eq!(
            ScreenArgs::ResumePicker {
                transcripts: vec![]
            }
            .screen_id(),
            Some("resume.picker")
        );
        assert_eq!(
            ScreenArgs::ViewFile { path: "/x".into() }.screen_id(),
            Some("view-file")
        );
        assert_eq!(
            ScreenArgs::EditFile { path: "/x".into() }.screen_id(),
            Some("edit-file")
        );
        assert_eq!(
            ScreenArgs::LanguagePicker {
                current_code: "en".into()
            }
            .screen_id(),
            Some("language.picker")
        );
        assert_eq!(
            ScreenArgs::ModelPicker {
                current_id: "gemini-2.5-flash".into(),
                models: vec![],
            }
            .screen_id(),
            Some("model.picker")
        );
        assert_eq!(
            ScreenArgs::PluginsManager.screen_id(),
            Some("plugins.manager")
        );
        assert_eq!(ScreenArgs::Changelog.screen_id(), Some("changelog"));
        assert_eq!(
            ScreenArgs::MigrationPicker {
                detected: vec![ProviderId::new("anthropic").unwrap()]
            }
            .screen_id(),
            Some("migration.picker")
        );
    }
}
