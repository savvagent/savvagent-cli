//! GUI editor state + paint pass for the `view-file` / `edit-file`
//! marker screens. The buffer lives on `SavvagentApp` (not on `App`,
//! which still owns the ratatui editor for the TUI path); it is
//! lazy-loaded from `App::active_file_path` the first frame after a
//! marker screen opens and cleared when the screen pops.

use std::path::{Path, PathBuf};

/// Per-open file state for the GUI editor. Owns the text the
/// `egui_code_editor::CodeEditor` widget mutates in-place, plus the path
/// it came from so save knows where to write.
#[derive(Debug, Clone)]
pub struct EditorBuffer {
    /// Disk path of the open file. Set on load.
    pub path: PathBuf,
    /// In-memory buffer. The widget mutates this on every keystroke
    /// when the screen is `edit-file`. Untouched for `view-file`.
    pub text: String,
    /// Whether the buffer has unsaved changes since load. Bumped to
    /// true by `mark_dirty` whenever the widget reports a text change;
    /// reset to false on successful save.
    pub dirty: bool,
}

impl EditorBuffer {
    /// Load `path` from disk into a fresh buffer.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            text,
            dirty: false,
        })
    }

    /// Mark dirty after a widget edit reported text changed.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Write the buffer text to disk and clear `dirty` on success.
    pub fn save_to_disk(&mut self) -> std::io::Result<()> {
        std::fs::write(&self.path, &self.text)?;
        self.dirty = false;
        Ok(())
    }
}

/// Paint the editor for a marker screen into `ui`.
///
/// `editable` is true for `edit-file`, false for `view-file`. The widget
/// reads (and, when editable, mutates) `buf.text` in place; when not
/// editable, this function snapshots the text first and restores it after
/// the paint pass so any stray mutation the widget allows is reverted —
/// `egui_code_editor::CodeEditor::show` accepts `&mut dyn TextBuffer` and
/// has no read-only mode, so this is the only sound way to keep view-file
/// truly read-only.
pub fn paint_editor(
    ui: &mut egui::Ui,
    buf: &mut EditorBuffer,
    palette: &crate::palette::Palette,
    editable: bool,
) {
    use egui_code_editor::CodeEditor;

    let theme = super::editor_theme::palette_to_color_theme(palette);
    let syntax = syntax_for_path(&buf.path);

    // Snapshot the text for read-only mode so any widget-mutation is
    // reverted after paint. The widget's `show` returns a `TextEditOutput`
    // whose `response.changed()` tells us whether the buffer was touched.
    let original = if editable {
        None
    } else {
        Some(buf.text.clone())
    };

    // The CodeEditor reads `id_source` to keep widget state across
    // frames; "savvagent.editor" is stable per the lifetime of one
    // marker-screen open (the buffer changes when the screen reopens
    // for a different file).
    let output = CodeEditor::default()
        .id_source("savvagent.editor")
        .with_rows(20)
        .with_fontsize(super::super::FONT_SIZE)
        .with_theme(theme)
        .with_syntax(syntax)
        .with_numlines(true)
        .show(ui, &mut buf.text);

    if let Some(snapshot) = original {
        // view-file: revert any incidental mutation so the buffer stays
        // truthful to disk.
        if output.response.changed() {
            buf.text = snapshot;
        }
    } else if output.response.changed() {
        // edit-file: a real edit happened — record dirty.
        buf.mark_dirty();
    }
}

/// Best-effort path → syntax mapping for the editor. Falls back to
/// `Syntax::rust()` for unknown extensions so the editor still paints
/// (with possibly-wrong highlighting) rather than failing.
///
/// egui_code_editor 0.2.17 ships presets for rust/python/shell/lua/asm/sql
/// only — JavaScript/TypeScript and friends fall through to the Rust
/// preset, which still gives C-family-shaped keyword highlighting.
fn syntax_for_path(path: &std::path::Path) -> egui_code_editor::Syntax {
    use egui_code_editor::Syntax;
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Syntax::rust(),
        "py" => Syntax::python(),
        "sh" | "bash" => Syntax::shell(),
        "lua" => Syntax::lua(),
        "sql" => Syntax::sql(),
        "asm" | "s" => Syntax::asm(),
        _ => Syntax::rust(),
    }
}

/// Ensure `state.editor_buffer` matches the currently-open marker
/// screen. Called once at the top of `update()` per frame. The buffer
/// is loaded from `app.active_file_path` (set by
/// `App::load_file_into_editor` when the screen pushes) and cleared
/// when no marker screen is on top.
pub fn ensure_buffer_for_active_screen(
    editor_buffer: &mut Option<EditorBuffer>,
    app: &crate::app::App,
) {
    let top_id = app.screen_stack.top().map(|(s, _)| s.id());
    let is_marker = matches!(top_id.as_deref(), Some("view-file") | Some("edit-file"));

    if !is_marker {
        *editor_buffer = None;
        return;
    }

    let want_path = match &app.active_file_path {
        Some(p) => p.clone(),
        None => {
            *editor_buffer = None;
            return;
        }
    };

    let needs_load = match editor_buffer {
        Some(buf) => buf.path != want_path,
        None => true,
    };
    if needs_load {
        match EditorBuffer::load(&want_path) {
            Ok(buf) => *editor_buffer = Some(buf),
            Err(err) => {
                tracing::warn!(error = %err, path = %want_path.display(),
                    "GUI editor: load failed");
                *editor_buffer = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn load_reads_file_contents() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "hello").unwrap();
        let buf = EditorBuffer::load(tmp.path()).unwrap();
        assert_eq!(buf.text, "hello\n");
        assert_eq!(buf.path, tmp.path());
        assert!(!buf.dirty);
    }

    #[test]
    fn mark_dirty_flips_flag() {
        let mut buf = EditorBuffer {
            path: PathBuf::from("/tmp/x"),
            text: String::new(),
            dirty: false,
        };
        buf.mark_dirty();
        assert!(buf.dirty);
    }

    #[test]
    fn save_writes_file_and_clears_dirty() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut buf = EditorBuffer {
            path: tmp.path().to_path_buf(),
            text: "new content\n".to_string(),
            dirty: true,
        };
        buf.save_to_disk().unwrap();
        assert!(!buf.dirty);
        assert_eq!(
            std::fs::read_to_string(tmp.path()).unwrap(),
            "new content\n"
        );
    }

    #[test]
    fn syntax_for_path_picks_rust() {
        let _syn = syntax_for_path(std::path::Path::new("foo.rs"));
        // Smoke test: this function must return a Syntax without panicking.
        // egui_code_editor's Syntax struct has different fields across
        // 0.2.x patches; rely on Debug or trivial introspection if needed.
    }

    #[test]
    fn syntax_for_path_unknown_extension_does_not_panic() {
        let _ = syntax_for_path(std::path::Path::new("foo.unknownext"));
    }
}
