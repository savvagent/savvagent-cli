//! `lsp_workspace_symbols` MCP tool.

use crate::config::{LanguageEntry, LspConfig};
use crate::convert::RangeOut;
use crate::pool::LspPool;
use anyhow::{Result, anyhow};
use lsp_types::{PartialResultParams, WorkDoneProgressParams, WorkspaceSymbolParams};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// Input to the `lsp_workspace_symbols` tool.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct LspWorkspaceSymbolsInput {
    /// Query string the LSP fuzzy-matches against symbol names.
    pub query: String,
    /// Optional language id ("rust", "typescript", …). When omitted, the
    /// tool queries every language in the config that has an active
    /// session — useful for "find this symbol anywhere I'm working."
    #[serde(default)]
    pub language: Option<String>,
}

/// One symbol returned by `lsp_workspace_symbols`.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct WorkspaceSymbolOut {
    /// Symbol name as reported by the LSP.
    pub name: String,
    /// Lowercase `SymbolKind` (e.g. `"function"`, `"struct"`, `"method"`).
    pub kind: String,
    /// Path to the file containing the symbol, relative to `root_env`
    /// when possible and absolute otherwise.
    pub path: String,
    /// Source range the symbol covers.
    pub range: RangeOut,
}

/// Output of the `lsp_workspace_symbols` tool.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct LspWorkspaceSymbolsOutput {
    /// Matching symbols across all queried languages.
    pub symbols: Vec<WorkspaceSymbolOut>,
}

/// Drive a `workspace/symbol` request for each language (or just the one
/// specified) and flatten the results into a uniform list. The session
/// pool is keyed at `root_env`, so the LSP indexes the whole workspace.
///
/// Only the [`lsp_types::WorkspaceSymbolResponse::Flat`] variant is
/// honored in v1; the `Nested` variant would require a second
/// `workspaceSymbol/resolve` round-trip and is deferred.
pub async fn dispatch(
    input: LspWorkspaceSymbolsInput,
    config: &LspConfig,
    pool: &LspPool,
    root_env: &Path,
    on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<LspWorkspaceSymbolsOutput> {
    let langs: Vec<&LanguageEntry> = match input.language.as_deref() {
        Some(id) => vec![
            config
                .language(id)
                .ok_or_else(|| anyhow!("no language configured with id `{id}`"))?,
        ],
        None => config.languages.iter().collect(),
    };

    let mut out = Vec::new();
    for lang in langs {
        let session = pool
            .get_or_spawn(lang, root_env.to_path_buf(), Arc::clone(&on_diagnostics))
            .await?;
        let params = WorkspaceSymbolParams {
            query: input.query.clone(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        let resp = session
            .request::<lsp_types::request::WorkspaceSymbolRequest>(params)
            .await?;
        let flat = match resp {
            None => Vec::new(),
            Some(lsp_types::WorkspaceSymbolResponse::Flat(v)) => v,
            // Nested variant uses `WorkspaceSymbol` and may require a
            // follow-up `workspaceSymbol/resolve` to materialize the
            // location. v1 sticks to flat; most servers still return it.
            Some(lsp_types::WorkspaceSymbolResponse::Nested(_)) => Vec::new(),
        };
        for s in flat {
            // Route through `convert::relativize_against_root` so the
            // Windows UNC long-path normalization (and forward-slash
            // rendering) applies here too. The URI-string fallback
            // stays for the rare case where `uri_to_path` itself fails.
            let path = match crate::session::uri_to_path(&s.location.uri) {
                Ok(p) => crate::convert::relativize_against_root(&p, root_env),
                Err(_) => s.location.uri.to_string(),
            };
            out.push(WorkspaceSymbolOut {
                name: s.name,
                kind: format!("{:?}", s.kind).to_lowercase(),
                path,
                range: s.location.range.into(),
            });
        }
    }
    Ok(LspWorkspaceSymbolsOutput { symbols: out })
}
