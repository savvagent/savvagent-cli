//! `lsp_hover` MCP tool.

use crate::config::LspConfig;
use crate::convert::RangeOut;
use crate::language::{extension_of, workspace_root_for};
use crate::pool::LspPool;
use crate::tools::definition::resolve_inside_root;
use anyhow::{Context, Result, anyhow};
use lsp_types::{
    HoverContents, HoverParams, MarkedString, Position, TextDocumentIdentifier,
    TextDocumentPositionParams, WorkDoneProgressParams,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Input to the `lsp_hover` tool.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct LspHoverInput {
    /// Path to the file, relative to the host cwd.
    pub path: String,
    /// 0-indexed line.
    pub line: u32,
    /// 0-indexed character offset.
    pub character: u32,
}

/// Output of the `lsp_hover` tool.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct LspHoverOutput {
    /// Markdown-formatted hover contents. Empty string if the LSP
    /// returned no hover for this position.
    pub contents: String,
    /// Range the hover applies to (often the identifier under the cursor).
    pub range: Option<RangeOut>,
}

/// Drive a `textDocument/hover` request through the pool and translate
/// the response into MCP-friendly JSON.
pub async fn dispatch(
    input: LspHoverInput,
    config: &LspConfig,
    pool: &LspPool,
    root_env: &PathBuf,
    on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<LspHoverOutput> {
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
    let params = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(input.line, input.character),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let resp: Option<lsp_types::Hover> = session
        .request::<lsp_types::request::HoverRequest>(params)
        .await?;
    let (contents, range) = match resp {
        None => (String::new(), None),
        Some(h) => {
            let txt = render_hover_contents(h.contents);
            (txt, h.range.map(Into::into))
        }
    };
    Ok(LspHoverOutput { contents, range })
}

fn render_hover_contents(c: HoverContents) -> String {
    match c {
        HoverContents::Scalar(MarkedString::String(s)) => s,
        HoverContents::Scalar(MarkedString::LanguageString(ls)) => {
            format!("```{}\n{}\n```", ls.language, ls.value)
        }
        HoverContents::Array(arr) => arr
            .into_iter()
            .map(|m| match m {
                MarkedString::String(s) => s,
                MarkedString::LanguageString(ls) => {
                    format!("```{}\n{}\n```", ls.language, ls.value)
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        HoverContents::Markup(mc) => mc.value,
    }
}
