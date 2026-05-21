//! LSP wire types ↔ MCP-friendly JSON.
//!
//! The model sees simple `{path, range}` objects; we hide LSP's `Uri`
//! and absolute-path leakage by mapping URIs back to paths relative to
//! the call's workspace root.

use crate::session::uri_to_path;
use anyhow::{Result, anyhow};
use lsp_types::{Diagnostic, Location, Position, Range, WorkspaceEdit};
use serde::Serialize;
use std::path::Path;

/// MCP-friendly position. 0-indexed, matches LSP.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct PositionOut {
    /// 0-indexed line number within the file.
    pub line: u32,
    /// 0-indexed UTF-16 character offset within the line.
    pub character: u32,
}

impl From<Position> for PositionOut {
    fn from(p: Position) -> Self {
        Self {
            line: p.line,
            character: p.character,
        }
    }
}

/// MCP-friendly half-open range `[start, end)`.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct RangeOut {
    /// Inclusive start position.
    pub start: PositionOut,
    /// Exclusive end position.
    pub end: PositionOut,
}

impl From<Range> for RangeOut {
    fn from(r: Range) -> Self {
        Self {
            start: r.start.into(),
            end: r.end.into(),
        }
    }
}

/// File path + range, the MCP-friendly replacement for LSP `Location`.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct LocationOut {
    /// Workspace-relative path when the URI sits under the workspace root,
    /// otherwise an absolute path.
    pub path: String,
    /// Range within the file.
    pub range: RangeOut,
}

/// Translate an LSP `Location` to an MCP-friendly form. `workspace_root`
/// is used to relativize the path; if the URI sits outside the root we
/// return an absolute path so the model doesn't see misleading
/// `../../../etc/passwd` segments.
pub fn location_to_out(loc: Location, workspace_root: &Path) -> Result<LocationOut> {
    let path = uri_to_path(&loc.uri)?;
    Ok(LocationOut {
        path: relativize_against_root(&path, workspace_root),
        range: loc.range.into(),
    })
}

/// Relativize `path` against `workspace_root`, returning a forward-slash
/// rendering. Falls back to `path`'s absolute display when the path sits
/// outside the root.
///
/// On Windows, `Path::strip_prefix` is byte-wise: `\\?\C:\foo` (the UNC
/// long-path form `canonicalize` returns) does NOT match `C:\foo` (the
/// form `url::Url::to_file_path` yields). We strip the `\\?\` (and
/// `\\?\UNC\` server prefix) before comparing so both sides share the
/// same shape. On non-Windows, `strip_unc_prefix` is a no-op.
pub(crate) fn relativize_against_root(path: &Path, workspace_root: &Path) -> String {
    let normalized_root = strip_unc_prefix(workspace_root);
    let normalized_path = strip_unc_prefix(path);
    match normalized_path.strip_prefix(&normalized_root) {
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => normalized_path.display().to_string(),
    }
}

#[cfg(windows)]
fn strip_unc_prefix(p: &Path) -> std::path::PathBuf {
    let s = p.as_os_str().to_string_lossy().into_owned();
    let stripped = s
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .or_else(|| s.strip_prefix(r"\\?\").map(str::to_string))
        .unwrap_or(s);
    std::path::PathBuf::from(stripped)
}

#[cfg(not(windows))]
fn strip_unc_prefix(p: &Path) -> std::path::PathBuf {
    p.to_path_buf()
}

/// MCP-friendly diagnostic shape. Used in tool replies and in the
/// `lsp://diagnostics/<path>` resource.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct DiagnosticOut {
    /// Range the diagnostic applies to.
    pub range: RangeOut,
    /// `"error"` | `"warning"` | `"info"` | `"hint"` | `"unknown"`.
    pub severity: String,
    /// Source string from the server (e.g. `"rustc"`, `"clippy"`).
    pub source: Option<String>,
    /// Diagnostic code, stringified if numeric.
    pub code: Option<String>,
    /// Human-readable message.
    pub message: String,
}

impl From<Diagnostic> for DiagnosticOut {
    fn from(d: Diagnostic) -> Self {
        Self {
            range: d.range.into(),
            severity: match d.severity {
                Some(lsp_types::DiagnosticSeverity::ERROR) => "error".into(),
                Some(lsp_types::DiagnosticSeverity::WARNING) => "warning".into(),
                Some(lsp_types::DiagnosticSeverity::INFORMATION) => "info".into(),
                Some(lsp_types::DiagnosticSeverity::HINT) => "hint".into(),
                _ => "unknown".into(),
            },
            source: d.source,
            code: d.code.map(|c| match c {
                lsp_types::NumberOrString::Number(n) => n.to_string(),
                lsp_types::NumberOrString::String(s) => s,
            }),
            message: d.message,
        }
    }
}

/// Restrict a `WorkspaceEdit` to plain text-edit shape:
///
/// `[ { "path": "...", "edits": [ { "range": {...}, "new_text": "..." } ] } ]`
///
/// Returns an error for any document operation (create/rename/delete file)
/// or for version-tagged text edits, which v1 doesn't support.
pub fn restrict_workspace_edit(
    we: WorkspaceEdit,
    workspace_root: &Path,
) -> Result<Vec<FileEditOut>> {
    if let Some(doc_ops) = we.document_changes {
        // Either Edits or Operations variant — only Edits is acceptable,
        // and only if every TextDocumentEdit's edits are plain TextEdit
        // (not AnnotatedTextEdit) and have no `version` constraint.
        return match doc_ops {
            lsp_types::DocumentChanges::Edits(edits) => {
                let mut out = Vec::new();
                for e in edits {
                    let path = uri_to_path(&e.text_document.uri)?;
                    let display = relativize_against_root(&path, workspace_root);
                    let mut edits = Vec::new();
                    for one in e.edits {
                        let lsp_types::OneOf::Left(te) = one else {
                            return Err(annot_err());
                        };
                        edits.push(TextEditOut {
                            range: te.range.into(),
                            new_text: te.new_text,
                        });
                    }
                    out.push(FileEditOut {
                        path: display,
                        edits,
                    });
                }
                Ok(out)
            }
            lsp_types::DocumentChanges::Operations(_) => Err(create_rename_delete_err()),
        };
    }
    // Fallback: flat `changes` map. No file ops here; just translate.
    if let Some(changes) = we.changes {
        let mut out = Vec::new();
        for (uri, edits) in changes {
            let path = uri_to_path(&uri)?;
            let display = relativize_against_root(&path, workspace_root);
            out.push(FileEditOut {
                path: display,
                edits: edits
                    .into_iter()
                    .map(|te| TextEditOut {
                        range: te.range.into(),
                        new_text: te.new_text,
                    })
                    .collect(),
            });
        }
        return Ok(out);
    }
    Ok(Vec::new())
}

fn create_rename_delete_err() -> anyhow::Error {
    anyhow!(
        "WorkspaceEdit includes file rename/create/delete which is not \
         supported in tool-lsp v1; please perform the change manually \
         with tool-fs"
    )
}

fn annot_err() -> anyhow::Error {
    anyhow!(
        "WorkspaceEdit includes annotated/versioned edits which are not \
         supported in tool-lsp v1; please perform the change manually \
         with tool-fs"
    )
}

/// A single text edit within a file.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct TextEditOut {
    /// Range to replace.
    pub range: RangeOut,
    /// Replacement text.
    pub new_text: String,
}

/// All edits to apply to one file.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct FileEditOut {
    /// Workspace-relative path (or absolute path if outside the workspace).
    pub path: String,
    /// Ordered list of edits to apply.
    pub edits: Vec<TextEditOut>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{NumberOrString, TextEdit};
    use std::collections::HashMap;

    #[test]
    fn diagnostic_severity_maps_to_strings() {
        let mut d = Diagnostic::new_simple(
            Range::new(Position::new(0, 0), Position::new(0, 1)),
            "boom".into(),
        );
        d.severity = Some(lsp_types::DiagnosticSeverity::ERROR);
        d.code = Some(NumberOrString::String("E0308".into()));
        d.source = Some("rustc".into());
        let out: DiagnosticOut = d.into();
        assert_eq!(out.severity, "error");
        assert_eq!(out.code.as_deref(), Some("E0308"));
        assert_eq!(out.source.as_deref(), Some("rustc"));
    }

    #[test]
    #[allow(clippy::mutable_key_type)] // `Uri`'s interior mutability is unavoidable here: it's the key type used by `WorkspaceEdit::changes`.
    fn restrict_workspace_edit_translates_plain_text_edits() {
        let path = std::env::current_dir().unwrap().join("test.rs");
        let uri = crate::session::path_to_uri(&path).unwrap();
        let we = WorkspaceEdit {
            changes: Some({
                let mut m = HashMap::new();
                m.insert(
                    uri,
                    vec![TextEdit {
                        range: Range::new(Position::new(0, 0), Position::new(0, 3)),
                        new_text: "Foo".into(),
                    }],
                );
                m
            }),
            document_changes: None,
            change_annotations: None,
        };
        let out = restrict_workspace_edit(we, &std::env::current_dir().unwrap()).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "test.rs");
        assert_eq!(out[0].edits.len(), 1);
        assert_eq!(out[0].edits[0].new_text, "Foo");
    }

    #[test]
    fn restrict_workspace_edit_rejects_file_operations() {
        let we = WorkspaceEdit {
            changes: None,
            document_changes: Some(lsp_types::DocumentChanges::Operations(vec![])),
            change_annotations: None,
        };
        let err = restrict_workspace_edit(we, Path::new("/")).unwrap_err();
        assert!(
            err.to_string().contains("rename/create/delete"),
            "error must mention the restriction reason: {err}"
        );
    }
}
