//! `lsp_rename` MCP tool.

use crate::config::LspConfig;
use crate::convert::{FileEditOut, restrict_workspace_edit};
use crate::language::{extension_of, workspace_root_for};
use crate::pool::LspPool;
use crate::tools::definition::resolve_inside_root;
use anyhow::{Context, Result, anyhow};
use lsp_types::{
    Position, RenameParams, TextDocumentIdentifier, TextDocumentPositionParams,
    WorkDoneProgressParams, WorkspaceEdit,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Input to the `lsp_rename` tool.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct LspRenameInput {
    /// Path to the file containing the symbol, relative to the host cwd.
    pub path: String,
    /// 0-indexed line of the symbol to rename.
    pub line: u32,
    /// 0-indexed character offset of the symbol to rename.
    pub character: u32,
    /// The new identifier to substitute at every reference.
    pub new_name: String,
}

/// Output of the `lsp_rename` tool.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct LspRenameOutput {
    /// File-by-file edit descriptors. The host's model uses these to
    /// drive tool-fs::write_file calls; tool-lsp never applies them
    /// itself.
    pub files: Vec<FileEditOut>,
}

/// Drive a `textDocument/rename` request through the pool, then translate
/// the returned `WorkspaceEdit` into a flat `[{path, edits}]` shape via
/// `convert::restrict_workspace_edit` (which rejects file create/rename/
/// delete operations and annotated/versioned edits).
pub async fn dispatch(
    input: LspRenameInput,
    config: &LspConfig,
    pool: &LspPool,
    root_env: &PathBuf,
    on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<LspRenameOutput> {
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
    let params = RenameParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(input.line, input.character),
        },
        new_name: input.new_name,
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let we: Option<WorkspaceEdit> = session
        .request::<lsp_types::request::Rename>(params)
        .await?;
    let we = we.ok_or_else(|| anyhow!("LSP returned no edits for the rename"))?;
    let files = restrict_workspace_edit(we, &workspace_root)?;
    Ok(LspRenameOutput { files })
}
