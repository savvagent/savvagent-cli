//! GUI-only widget integrations layered on Plan 1+2's render-boundary.
//!
//! These modules own the egui-specific state for the heavy widgets the
//! generic styled-line painter cannot render: the syntax-highlighted code
//! editor (`view-file`/`edit-file` marker screens), and the `Ctrl+O`
//! file-picker that inserts `@<path>` references into the prompt buffer.

pub mod editor;
pub mod editor_theme;
pub mod file_picker;
