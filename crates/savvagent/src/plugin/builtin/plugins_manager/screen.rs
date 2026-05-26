//! `plugins.manager` Screen — list plugins with a per-row toggle.
//!
//! Rows are owned by the screen because the runtime injects them after
//! constructing the empty screen via the plugin's `create_screen`
//! callback (see `apply_effects::open_screen`). The screen itself never
//! reaches into `App` state; it only emits effects on key events.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, PluginId, PluginKind, Region, Screen,
    StyledLine, StyledSpan, TextMods, ThemeColor,
};

/// Per-open instance of the plugins-manager modal.
pub(crate) struct PluginsManagerScreen {
    pub(crate) rows: Vec<PluginRow>,
    pub(crate) cursor: usize,
}

/// One row in [`PluginsManagerScreen`]. Cloned from the plugin's manifest
/// and the registry's enabled-set at open time; the screen mutates
/// `enabled` optimistically when the user toggles a row, then emits
/// [`Effect::TogglePlugin`] for the runtime to apply persistently.
pub(crate) struct PluginRow {
    pub(crate) id: PluginId,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) kind: PluginKind,
    pub(crate) enabled: bool,
    pub(crate) contribution_summary: String,
    /// `true` when the plugin id's vendor prefix is something other than
    /// `internal:`. Populated by the runtime via
    /// [`super::is_external_id`] when it builds rows from the registry.
    /// The screen renders a trailing `(external)` or `(built-in)` label
    /// based on this flag — it's surface-only and never affects toggling
    /// behaviour.
    pub(crate) external: bool,
}

impl PluginsManagerScreen {
    /// Construct an empty screen — used by the plugin's `create_screen`
    /// callback. `apply_effects::open_screen` then replaces the top of
    /// the stack with [`PluginsManagerScreen::with_rows`] once it has
    /// queried the registry.
    pub(crate) fn empty() -> Self {
        Self {
            rows: vec![],
            cursor: 0,
        }
    }

    /// Construct with a pre-populated row list. Used by the runtime after
    /// it builds rows from the registry + manifests.
    pub(crate) fn with_rows(rows: Vec<PluginRow>) -> Self {
        Self { rows, cursor: 0 }
    }
}

#[async_trait]
impl Screen for PluginsManagerScreen {
    fn id(&self) -> String {
        "plugins.manager".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        let mut out = Vec::with_capacity(self.rows.len());
        if self.rows.is_empty() {
            out.push(StyledLine {
                spans: vec![StyledSpan {
                    text: rust_i18n::t!("picker.plugins-manager.no-plugins").to_string(),
                    fg: Some(ThemeColor::Warning),
                    bg: None,
                    modifiers: TextMods::default(),
                }],
            });
            return out;
        }
        for (i, row) in self.rows.iter().enumerate() {
            let marker = if i == self.cursor { "> " } else { "  " };
            let toggle = match (row.kind, row.enabled) {
                (PluginKind::Core, _) => {
                    rust_i18n::t!("picker.plugins-manager.row-core").to_string()
                }
                (PluginKind::Optional, true) => {
                    rust_i18n::t!("picker.plugins-manager.row-on").to_string()
                }
                (PluginKind::Optional, false) => {
                    rust_i18n::t!("picker.plugins-manager.row-off").to_string()
                }
            };
            let color = if i == self.cursor {
                ThemeColor::Accent
            } else {
                ThemeColor::Fg
            };
            // TextMods is not #[non_exhaustive], so FRU is safe here.
            let mods_active = TextMods {
                bold: i == self.cursor,
                ..Default::default()
            };
            let origin_label = if row.external {
                rust_i18n::t!("picker.plugins-manager.row-external").to_string()
            } else {
                rust_i18n::t!("picker.plugins-manager.row-builtin").to_string()
            };
            out.push(StyledLine {
                spans: vec![
                    StyledSpan {
                        text: format!("{marker}{toggle} "),
                        fg: Some(color),
                        bg: None,
                        modifiers: mods_active,
                    },
                    StyledSpan {
                        text: format!("{:<28} v{}", row.name, row.version),
                        fg: Some(color),
                        bg: None,
                        modifiers: mods_active,
                    },
                    StyledSpan {
                        text: format!("  {}", row.contribution_summary),
                        fg: Some(ThemeColor::Muted),
                        bg: None,
                        modifiers: TextMods::default(),
                    },
                    StyledSpan {
                        text: format!("  {origin_label}"),
                        fg: Some(ThemeColor::Muted),
                        bg: None,
                        modifiers: TextMods::default(),
                    },
                ],
            });
        }
        out
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            KeyCodePortable::Esc => Ok(vec![Effect::CloseScreen]),
            KeyCodePortable::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                Ok(vec![])
            }
            KeyCodePortable::Down => {
                let max = self.rows.len().saturating_sub(1);
                if self.cursor < max {
                    self.cursor += 1;
                }
                Ok(vec![])
            }
            KeyCodePortable::Char(' ') | KeyCodePortable::Enter => {
                let Some(row) = self.rows.get_mut(self.cursor) else {
                    return Ok(vec![]);
                };
                if matches!(row.kind, PluginKind::Core) {
                    return Ok(vec![Effect::PushNote {
                        line: StyledLine {
                            spans: vec![StyledSpan {
                                text: rust_i18n::t!("picker.plugins-manager.core-cannot-disable")
                                    .to_string(),
                                fg: Some(ThemeColor::Warning),
                                bg: None,
                                modifiers: TextMods::default(),
                            }],
                        },
                    }]);
                }
                row.enabled = !row.enabled;
                Ok(vec![Effect::TogglePlugin {
                    id: row.id.clone(),
                    enabled: row.enabled,
                }])
            }
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            rust_i18n::t!("picker.plugins-manager.tips").to_string(),
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

    #[tokio::test]
    async fn toggling_core_emits_warning_push_note() {
        let rows = vec![PluginRow {
            id: PluginId::new("internal:home-footer").expect("valid"),
            name: "Home footer".into(),
            version: "0".into(),
            kind: PluginKind::Core,
            enabled: true,
            contribution_summary: "".into(),
            external: false,
        }];
        let mut s = PluginsManagerScreen::with_rows(rows);
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match &effs[0] {
            Effect::PushNote { line } => {
                let joined: String = line.spans.iter().map(|s| s.text.clone()).collect();
                assert!(
                    joined.contains(
                        rust_i18n::t!("picker.plugins-manager.core-cannot-disable").as_ref()
                    ),
                    "expected core-cannot-disable text, got {joined:?}"
                );
            }
            other => panic!("expected PushNote, got {other:?}"),
        }
        // The row stayed enabled — Core is not flipped optimistically.
        assert!(s.rows[0].enabled);
    }

    #[tokio::test]
    async fn toggling_optional_emits_toggleplugin_effect() {
        let rows = vec![PluginRow {
            id: PluginId::new("internal:provider-anthropic").expect("valid"),
            name: "Anthropic".into(),
            version: "0".into(),
            kind: PluginKind::Optional,
            enabled: true,
            contribution_summary: "".into(),
            external: false,
        }];
        let mut s = PluginsManagerScreen::with_rows(rows);
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match &effs[0] {
            Effect::TogglePlugin { id, enabled } => {
                assert_eq!(id.as_str(), "internal:provider-anthropic");
                assert!(!*enabled);
            }
            other => panic!("expected TogglePlugin, got {other:?}"),
        }
        // Optimistic local flip: the row should now read disabled.
        assert!(!s.rows[0].enabled);
    }

    #[tokio::test]
    async fn space_also_toggles_optional() {
        let rows = vec![PluginRow {
            id: PluginId::new("internal:provider-openai").expect("valid"),
            name: "OpenAI".into(),
            version: "0".into(),
            kind: PluginKind::Optional,
            enabled: false,
            contribution_summary: "".into(),
            external: false,
        }];
        let mut s = PluginsManagerScreen::with_rows(rows);
        let effs = s.on_key(key(KeyCodePortable::Char(' '))).await.unwrap();
        match &effs[0] {
            Effect::TogglePlugin { enabled, .. } => assert!(*enabled),
            other => panic!("expected TogglePlugin, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn esc_emits_close_screen() {
        let mut s = PluginsManagerScreen::empty();
        let effs = s.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn down_arrow_advances_cursor_up_to_last_row() {
        let rows = vec![
            PluginRow {
                id: PluginId::new("internal:a").expect("valid"),
                name: "A".into(),
                version: "0".into(),
                kind: PluginKind::Core,
                enabled: true,
                contribution_summary: "".into(),
                external: false,
            },
            PluginRow {
                id: PluginId::new("internal:b").expect("valid"),
                name: "B".into(),
                version: "0".into(),
                kind: PluginKind::Optional,
                enabled: true,
                contribution_summary: "".into(),
                external: false,
            },
        ];
        let mut s = PluginsManagerScreen::with_rows(rows);
        assert_eq!(s.cursor, 0);
        let _ = s.on_key(key(KeyCodePortable::Down)).await.unwrap();
        assert_eq!(s.cursor, 1);
        // Cursor saturates at the last row instead of wrapping.
        let _ = s.on_key(key(KeyCodePortable::Down)).await.unwrap();
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn id_is_plugins_manager() {
        let s = PluginsManagerScreen::empty();
        assert_eq!(s.id(), "plugins.manager");
    }

    /// Confirm the trailing origin suffix renders distinctly for built-in
    /// vs. external rows. Locks in the surface contract for Task 10
    /// (sub-project D): the manager screen visibly distinguishes wasm
    /// plugins from in-process built-ins so users can tell at a glance
    /// where each plugin came from.
    #[test]
    fn render_includes_origin_suffix_per_row() {
        let rows = vec![
            PluginRow {
                id: PluginId::new("internal:home-footer").expect("valid"),
                name: "Home footer".into(),
                version: "0".into(),
                kind: PluginKind::Core,
                enabled: true,
                contribution_summary: "".into(),
                external: false,
            },
            PluginRow {
                id: PluginId::new("acme:demo").expect("valid"),
                name: "Demo".into(),
                version: "0".into(),
                kind: PluginKind::Optional,
                enabled: true,
                contribution_summary: "".into(),
                external: true,
            },
        ];
        let s = PluginsManagerScreen::with_rows(rows);
        let lines = s.render(Region {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        assert_eq!(lines.len(), 2);
        let line0: String = lines[0].spans.iter().map(|sp| sp.text.clone()).collect();
        let line1: String = lines[1].spans.iter().map(|sp| sp.text.clone()).collect();
        let builtin = rust_i18n::t!("picker.plugins-manager.row-builtin").to_string();
        let external = rust_i18n::t!("picker.plugins-manager.row-external").to_string();
        assert!(
            line0.contains(&builtin),
            "expected built-in suffix in row 0: {line0:?}"
        );
        assert!(
            line1.contains(&external),
            "expected external suffix in row 1: {line1:?}"
        );
    }
}
