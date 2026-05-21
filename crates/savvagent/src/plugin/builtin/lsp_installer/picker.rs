//! Wraps `MultiSelectList<&CatalogEntry>` for the `/lsp` UI.

use crate::plugin::builtin::lsp_installer::catalog::{CATALOG, CatalogEntry};
use crate::plugin::widgets::MultiSelectList;

/// Picker state for the `/lsp` modal. Holds the catalog by reference
/// so the `Confirm` payload is cheap to look up by id (and the catalog
/// itself is `'static`).
pub struct LspPicker {
    /// Underlying generic widget. Public to the module so
    /// [`super::screen::LspPickerScreen`] can forward keys directly.
    pub inner: MultiSelectList<&'static CatalogEntry>,
}

impl LspPicker {
    /// Construct a fresh picker over every catalog entry, with empty
    /// filter and cursor at row 0. Filter matches **case-insensitively**
    /// against `id`, `display_name`, or `language_label`.
    pub fn new() -> Self {
        let items: Vec<&'static CatalogEntry> = CATALOG.iter().collect();
        let widget = MultiSelectList::new(
            items,
            |entry: &&'static CatalogEntry, filter: &str| {
                let f = filter.to_ascii_lowercase();
                entry.id.to_ascii_lowercase().contains(&f)
                    || entry.display_name.to_ascii_lowercase().contains(&f)
                    || entry.language_label.to_ascii_lowercase().contains(&f)
            },
            |entry: &&'static CatalogEntry| entry.id.to_string(),
        );
        Self { inner: widget }
    }
}

impl Default for LspPicker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_loads_full_catalog() {
        let p = LspPicker::new();
        assert_eq!(p.inner.filtered().len(), CATALOG.len());
    }

    #[test]
    fn filter_is_case_insensitive() {
        let mut p = LspPicker::new();
        // Type "RUST" — should match the rust-analyzer entry even though
        // the catalog id is lowercase.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for c in ['R', 'U', 'S', 'T'] {
            p.inner
                .on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
        }
        let filtered: Vec<&str> = p.inner.filtered().iter().map(|e| e.id).collect();
        assert!(
            filtered.contains(&"rust-analyzer"),
            "case-insensitive filter should match rust-analyzer; got {filtered:?}"
        );
    }
}
