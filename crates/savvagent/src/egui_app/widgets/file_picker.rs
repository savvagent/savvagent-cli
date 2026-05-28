//! `Ctrl+O` file picker for the GUI prompt. Wraps an
//! `egui_file_dialog::FileDialog` and exposes `open()`, `update(ctx)`,
//! and `take_picked() -> Option<PathBuf>`, plus the pure helper
//! `splice_at_reference` that appends `@<path>` to a prompt buffer.
//!
//! Confirmed `egui_file_dialog::FileDialog` API names (against 0.11.0):
//! - `FileDialog::new() -> Self` — creates a new dialog instance with
//!   default values; owned by the caller.
//! - `pick_file(&mut self)` — shortcut to open the dialog in pick-file
//!   mode (NOT `select_file()` — the upstream rename to `pick_*` is the
//!   surface name on 0.11.0).
//! - `update(&mut self, ctx: &egui::Context) -> &Self` — per-frame
//!   update; must be called every frame while the dialog is visible.
//! - `take_picked(&mut self) -> Option<PathBuf>` — returns the picked
//!   path once after the user confirms, then transitions the dialog to
//!   `DialogState::Closed`. (A non-consuming `picked() -> Option<&Path>`
//!   companion also exists if a borrow is preferable.)

use std::path::{Path, PathBuf};

use egui_file_dialog::FileDialog;

/// State for the GUI file picker. Default-constructed once when
/// `SavvagentApp` is built; the dialog itself is lazy-initialized in
/// `Self::new` and re-used across opens.
pub struct FilePicker {
    inner: FileDialog,
}

impl Default for FilePicker {
    fn default() -> Self {
        Self::new()
    }
}

impl FilePicker {
    pub fn new() -> Self {
        Self {
            inner: FileDialog::new(),
        }
    }

    /// Open the dialog in pick-file mode. Idempotent on consecutive
    /// calls within one frame.
    pub fn open(&mut self) {
        self.inner.pick_file();
    }

    /// Paint + drive the dialog. Must be called every frame the dialog
    /// might be visible.
    pub fn update(&mut self, ctx: &egui::Context) {
        self.inner.update(ctx);
    }

    /// Consume a confirmed pick (the user clicked OK). Returns `None`
    /// while the dialog is still open or after the user cancelled.
    pub fn take_picked(&mut self) -> Option<PathBuf> {
        self.inner.take_picked()
    }
}

/// Splice an `@<path>` reference into `prompt`, mimicking the TUI's
/// `App::file_picker_select` behavior (`app.rs:1542-1571`): if the
/// prompt contains a trailing `@`, replace from the last `@` to end with
/// `@<path>`; otherwise append ` @<path>`.
pub fn splice_at_reference(prompt: &mut String, path: &Path) {
    let path_str = path.display().to_string();
    if let Some(idx) = prompt.rfind('@') {
        prompt.truncate(idx);
        prompt.push('@');
        prompt.push_str(&path_str);
    } else {
        if !prompt.is_empty() && !prompt.ends_with(char::is_whitespace) {
            prompt.push(' ');
        }
        prompt.push('@');
        prompt.push_str(&path_str);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splice_replaces_trailing_at_marker() {
        let mut prompt = String::from("explain @");
        splice_at_reference(&mut prompt, Path::new("src/main.rs"));
        assert_eq!(prompt, "explain @src/main.rs");
    }

    #[test]
    fn splice_appends_when_no_at_marker() {
        let mut prompt = String::from("explain");
        splice_at_reference(&mut prompt, Path::new("src/main.rs"));
        assert_eq!(prompt, "explain @src/main.rs");
    }

    #[test]
    fn splice_appends_without_double_space() {
        let mut prompt = String::from("explain ");
        splice_at_reference(&mut prompt, Path::new("a.rs"));
        assert_eq!(prompt, "explain @a.rs");
    }

    #[test]
    fn splice_into_empty_prompt() {
        let mut prompt = String::new();
        splice_at_reference(&mut prompt, Path::new("a.rs"));
        assert_eq!(prompt, "@a.rs");
    }
}
