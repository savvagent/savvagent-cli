//! `lsp_references` MCP tool.

use crate::config::LspConfig;
use crate::convert::{LocationOut, location_to_out};
use crate::language::{extension_of, workspace_root_for};
use crate::pool::LspPool;
use crate::tools::definition::resolve_inside_root;
use anyhow::{Context, Result, anyhow};
use lsp_types::{
    Location, PartialResultParams, Position, ReferenceContext, ReferenceParams,
    TextDocumentIdentifier, TextDocumentPositionParams, WorkDoneProgressParams,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Input to the `lsp_references` tool.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct LspReferencesInput {
    /// Path to the file, relative to the host cwd.
    pub path: String,
    /// 0-indexed line.
    pub line: u32,
    /// 0-indexed character offset.
    pub character: u32,
    /// Whether the declaration of the symbol should be included in the
    /// returned references. Defaults to `true`.
    #[serde(default = "default_include_declaration")]
    pub include_declaration: bool,
}

fn default_include_declaration() -> bool {
    true
}

/// Output of the `lsp_references` tool.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct LspReferencesOutput {
    /// Zero, one, or many reference locations the LSP returned.
    pub locations: Vec<LocationOut>,
}

/// Drive a `textDocument/references` request through the pool and
/// translate the response into MCP-friendly JSON.
pub async fn dispatch(
    input: LspReferencesInput,
    config: &LspConfig,
    pool: &LspPool,
    root_env: &PathBuf,
    on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<LspReferencesOutput> {
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
    let params = ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(input.line, input.character),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: ReferenceContext {
            include_declaration: input.include_declaration,
        },
    };
    let resp: Option<Vec<Location>> = session
        .request::<lsp_types::request::References>(params)
        .await?;
    let out: Result<Vec<LocationOut>> = resp
        .unwrap_or_default()
        .into_iter()
        .map(|l| location_to_out(l, &workspace_root))
        .collect();
    Ok(LspReferencesOutput { locations: out? })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_defaults_include_declaration_true() {
        let v: LspReferencesInput =
            serde_json::from_str(r#"{"path":"x","line":0,"character":0}"#).unwrap();
        assert!(v.include_declaration);
    }
}
