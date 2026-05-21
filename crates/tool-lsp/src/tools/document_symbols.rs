//! `lsp_document_symbols` MCP tool.

use crate::config::LspConfig;
use crate::convert::RangeOut;
use crate::language::{extension_of, workspace_root_for};
use crate::pool::LspPool;
use crate::tools::definition::resolve_inside_root;
use anyhow::{Context, Result, anyhow};
use lsp_types::{
    DocumentSymbol, DocumentSymbolParams, PartialResultParams, SymbolInformation,
    TextDocumentIdentifier, WorkDoneProgressParams,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Input to the `lsp_document_symbols` tool.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct LspDocumentSymbolsInput {
    /// Path to the file, relative to the host cwd.
    pub path: String,
}

/// One node of the symbol tree returned by `lsp_document_symbols`.
///
/// Flat (`SymbolInformation`) responses collapse to a list of leaves
/// (each with `children` empty); nested (`DocumentSymbol`) responses
/// preserve the LSP's hierarchy.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct SymbolOut {
    /// Symbol name as reported by the LSP.
    pub name: String,
    /// Lowercase `SymbolKind` (e.g. `"function"`, `"struct"`, `"method"`).
    pub kind: String,
    /// Source range the symbol covers.
    pub range: RangeOut,
    /// Nested child symbols. Empty for flat responses.
    pub children: Vec<SymbolOut>,
}

/// Output of the `lsp_document_symbols` tool.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct LspDocumentSymbolsOutput {
    /// Top-level symbols defined in the document.
    pub symbols: Vec<SymbolOut>,
}

/// Drive a `textDocument/documentSymbol` request through the pool and
/// translate either response shape into a uniform [`SymbolOut`] tree.
pub async fn dispatch(
    input: LspDocumentSymbolsInput,
    config: &LspConfig,
    pool: &LspPool,
    root_env: &PathBuf,
    on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<LspDocumentSymbolsOutput> {
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
    let params = DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let resp = session
        .request::<lsp_types::request::DocumentSymbolRequest>(params)
        .await?;
    let symbols = match resp {
        None => Vec::new(),
        Some(lsp_types::DocumentSymbolResponse::Flat(flat)) => {
            flat.into_iter().map(convert_flat).collect()
        }
        Some(lsp_types::DocumentSymbolResponse::Nested(nested)) => {
            nested.into_iter().map(convert_nested).collect()
        }
    };
    Ok(LspDocumentSymbolsOutput { symbols })
}

fn convert_flat(s: SymbolInformation) -> SymbolOut {
    SymbolOut {
        name: s.name,
        kind: symbol_kind(s.kind),
        range: s.location.range.into(),
        children: Vec::new(),
    }
}

fn convert_nested(s: DocumentSymbol) -> SymbolOut {
    SymbolOut {
        name: s.name,
        kind: symbol_kind(s.kind),
        range: s.range.into(),
        children: s
            .children
            .unwrap_or_default()
            .into_iter()
            .map(convert_nested)
            .collect(),
    }
}

fn symbol_kind(k: lsp_types::SymbolKind) -> String {
    format!("{:?}", k).to_lowercase()
}
