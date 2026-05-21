//! `lsp_definition` MCP tool.

use crate::config::LspConfig;
use crate::convert::{LocationOut, location_to_out};
use crate::language::{extension_of, workspace_root_for};
use crate::pool::LspPool;
use anyhow::{Context, Result, anyhow};
use lsp_types::{
    GotoDefinitionParams, GotoDefinitionResponse, Location, PartialResultParams, Position,
    TextDocumentIdentifier, TextDocumentPositionParams, Uri, WorkDoneProgressParams,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Input to the `lsp_definition` tool.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct LspDefinitionInput {
    /// Path to the file, relative to the host cwd.
    pub path: String,
    /// 0-indexed line.
    pub line: u32,
    /// 0-indexed character offset.
    pub character: u32,
}

/// Output of the `lsp_definition` tool.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct LspDefinitionOutput {
    /// Zero, one, or many definition locations the LSP returned.
    pub locations: Vec<LocationOut>,
}

/// Drive a `textDocument/definition` request through the pool and
/// translate the response into MCP-friendly JSON.
pub async fn dispatch(
    input: LspDefinitionInput,
    config: &LspConfig,
    pool: &LspPool,
    root_env: &PathBuf,
    on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<LspDefinitionOutput> {
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

    let uri: Uri = crate::session::path_to_uri(&file)?;
    let params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position::new(input.line, input.character),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let resp: Option<GotoDefinitionResponse> = session
        .request::<lsp_types::request::GotoDefinition>(params)
        .await?;
    let locations: Vec<Location> = match resp {
        None => Vec::new(),
        Some(GotoDefinitionResponse::Scalar(loc)) => vec![loc],
        Some(GotoDefinitionResponse::Array(v)) => v,
        Some(GotoDefinitionResponse::Link(links)) => links
            .into_iter()
            .map(|l| Location {
                uri: l.target_uri,
                range: l.target_selection_range,
            })
            .collect(),
    };
    let out: Result<Vec<LocationOut>> = locations
        .into_iter()
        .map(|l| location_to_out(l, &workspace_root))
        .collect();
    Ok(LspDefinitionOutput { locations: out? })
}

/// Resolve `relative` against `root_env`, rejecting anything that
/// escapes via `..`. Mirrors the discipline `tool-fs` uses.
///
/// The traversal check is component-based — a literal substring search
/// for `..` would (incorrectly) reject legitimate filenames like
/// `foo..bar.rs`. Only `..` standing alone as a path component (i.e.
/// `Path::Component::ParentDir`) is treated as an escape attempt.
pub(crate) fn resolve_inside_root(relative: &str, root_env: &PathBuf) -> Result<PathBuf> {
    let candidate = std::path::Path::new(relative);
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(anyhow!("path traversal not allowed in `{relative}`"));
    }
    let joined = root_env.join(relative);
    let canonical = joined
        .canonicalize()
        .with_context(|| format!("resolving {}", joined.display()))?;
    if !canonical.starts_with(root_env) {
        return Err(anyhow!(
            "resolved path {} escapes SAVVAGENT_TOOL_LSP_ROOT",
            canonical.display()
        ));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn resolve_inside_root_rejects_dotdot() {
        let root = tempdir().unwrap();
        let err = resolve_inside_root("../etc/passwd", &root.path().to_path_buf()).unwrap_err();
        assert!(err.to_string().contains("traversal"));
    }

    #[test]
    fn resolve_inside_root_canonicalizes_within_root() {
        let root = tempdir().unwrap();
        let file = root.path().join("src/lib.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "").unwrap();
        // On macOS, /tmp is a symlink to /private/tmp; canonicalize the
        // root the test passes in so the starts_with check holds.
        let canonical_root = root.path().canonicalize().unwrap();
        let resolved = resolve_inside_root("src/lib.rs", &canonical_root).unwrap();
        assert!(resolved.starts_with(&canonical_root));
    }

    #[test]
    fn resolve_inside_root_accepts_dots_in_filename() {
        // Regression: the previous substring-based `..` check would
        // (incorrectly) reject `foo..bar.rs` as a traversal attempt.
        // Component-based matching only rejects `..` standing alone.
        let root = tempdir().unwrap();
        let canonical_root = root.path().canonicalize().unwrap();
        let file = canonical_root.join("foo..bar.rs");
        fs::write(&file, "").unwrap();
        let resolved = resolve_inside_root("foo..bar.rs", &canonical_root).unwrap();
        assert!(resolved.ends_with("foo..bar.rs"));
    }
}
