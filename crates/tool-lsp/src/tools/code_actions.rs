//! `lsp_code_actions` MCP tool.

use crate::config::LspConfig;
use crate::convert::{FileEditOut, restrict_workspace_edit};
use crate::language::{extension_of, workspace_root_for};
use crate::pool::LspPool;
use crate::tools::definition::resolve_inside_root;
use anyhow::{Context, Result, anyhow};
use lsp_types::{
    CodeActionContext, CodeActionOrCommand, CodeActionParams, PartialResultParams, Position, Range,
    TextDocumentIdentifier, WorkDoneProgressParams,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Input to the `lsp_code_actions` tool.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct LspCodeActionsInput {
    /// Path to the file containing the range, relative to the host cwd.
    pub path: String,
    /// Range to query for actions. The model usually narrows by passing
    /// a single-line range; we don't constrain shape.
    pub range: RangeIn,
    /// Optional filter by `CodeActionKind` prefix (e.g. `"quickfix"`,
    /// `"refactor.extract"`). When omitted, all actions are returned.
    #[serde(default)]
    pub only: Option<Vec<String>>,
}

/// Half-open input range `[start, end)`. 0-indexed to match LSP.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct RangeIn {
    /// 0-indexed start line.
    pub start_line: u32,
    /// 0-indexed UTF-16 character offset of the start position.
    pub start_character: u32,
    /// 0-indexed end line (exclusive).
    pub end_line: u32,
    /// 0-indexed UTF-16 character offset of the end position (exclusive).
    pub end_character: u32,
}

impl From<RangeIn> for Range {
    fn from(r: RangeIn) -> Self {
        Range::new(
            Position::new(r.start_line, r.start_character),
            Position::new(r.end_line, r.end_character),
        )
    }
}

/// A single code action returned by the LSP, translated for the model.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct CodeActionOut {
    /// Human-readable title (e.g. `"Extract function"`).
    pub title: String,
    /// LSP `CodeActionKind` string, when the server reports one
    /// (e.g. `"quickfix"`, `"refactor.extract"`).
    pub kind: Option<String>,
    /// If the action has an inline edit, it's translated here. Otherwise
    /// the model would need to invoke a `Command` we can't fulfill —
    /// in that case `edit` is `None` and the model is expected to
    /// surface a manual hint to the user.
    pub edit: Option<Vec<FileEditOut>>,
}

/// Output of the `lsp_code_actions` tool.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct LspCodeActionsOutput {
    /// Actions available for the requested range, in the order the LSP
    /// returned them.
    pub actions: Vec<CodeActionOut>,
}

/// Drive a `textDocument/codeAction` request through the pool and translate
/// every returned `CodeAction`/`Command` into a flat
/// `{ title, kind, edit }` shape. `Command`-only actions and actions whose
/// edit shape isn't supported by `restrict_workspace_edit` surface with
/// `edit: None` rather than failing the whole tool — the model decides
/// whether to skip or hint the user.
pub async fn dispatch(
    input: LspCodeActionsInput,
    config: &LspConfig,
    pool: &LspPool,
    root_env: &PathBuf,
    on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<LspCodeActionsOutput> {
    let file = resolve_inside_root(&input.path, root_env)?;
    let ext = extension_of(&file).context("file has no extension")?;
    let lang = config
        .language_for_extension(&ext)
        .ok_or_else(|| anyhow!("no LSP configured for .{ext} files"))?;
    let workspace_root = workspace_root_for(&file, &lang.root_markers).ok_or_else(|| {
        anyhow!(
            "no workspace root for {} (looked for: {})",
            file.display(),
            lang.root_markers.join(", ")
        )
    })?;
    let session = pool
        .get_or_spawn(lang, workspace_root.clone(), on_diagnostics)
        .await?;
    session.ensure_did_open(&file).await?;

    let uri = crate::session::path_to_uri(&file)?;
    let only = input.only.map(|v| {
        v.into_iter()
            .map(lsp_types::CodeActionKind::from)
            .collect::<Vec<_>>()
    });
    let params = CodeActionParams {
        text_document: TextDocumentIdentifier { uri },
        range: input.range.into(),
        context: CodeActionContext {
            diagnostics: Vec::new(),
            only,
            trigger_kind: None,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let resp = session
        .request::<lsp_types::request::CodeActionRequest>(params)
        .await?;
    let raw: Vec<CodeActionOrCommand> = resp.unwrap_or_default();

    let mut actions = Vec::new();
    for it in raw {
        match it {
            CodeActionOrCommand::Command(c) => actions.push(CodeActionOut {
                title: c.title,
                kind: None,
                edit: None,
            }),
            CodeActionOrCommand::CodeAction(a) => {
                let edit = match a.edit {
                    Some(we) => match restrict_workspace_edit(we, &workspace_root) {
                        Ok(files) => Some(files),
                        Err(e) => {
                            tracing::debug!(
                                title = %a.title,
                                error = %e,
                                "code action has unsupported edit shape; surfacing without edit"
                            );
                            None
                        }
                    },
                    None => None,
                };
                actions.push(CodeActionOut {
                    title: a.title,
                    kind: a.kind.map(|k| k.as_str().to_string()),
                    edit,
                });
            }
        }
    }
    Ok(LspCodeActionsOutput { actions })
}
