//! Filesystem tools as a Model Context Protocol stdio server.
//!
//! Exposes four tools that an agent host can call over MCP:
//!
//! - [`read_file`](FsTools::read_file) — read a UTF-8 file with a size cap.
//! - [`write_file`](FsTools::write_file) — write/overwrite a UTF-8 file.
//! - [`list_dir`](FsTools::list_dir) — list directory entries (optionally recursively).
//! - [`glob`](FsTools::glob) — expand a glob pattern relative to a root.
//!
//! The binary `savvagent-tool-fs` wraps [`FsTools`] in an `rmcp` stdio
//! transport; see `src/main.rs`.
//!
//! v0.1 ships **without sandboxing**. Tools run with the full privileges of the
//! invoking user. Hosts are expected to confirm destructive calls before
//! routing them.
//!
//! Optional Layer 1 path hygiene: construct via [`FsTools::with_root`] (or set
//! `SAVVAGENT_TOOL_FS_ROOT` for the bundled binary) to confine all four tools
//! to a single project root. Inputs containing `..` are rejected, relative
//! paths resolve against the root, and symlink escapes are caught by
//! `std::fs::canonicalize`.

#![allow(clippy::collapsible_if)] // pre-existing debt; many sites under rustc 1.95 new lint
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod edit;
pub use edit::{
    InsertInput, InsertOutput, MultiEdit, MultiEditInput, MultiEditOutput, ReplaceCount,
    ReplaceInput, ReplaceOutput,
};

use std::path::{Path, PathBuf};

use rmcp::{
    ErrorData, ServerHandler,
    handler::server::{
        router::tool::ToolRouter,
        wrapper::{Json, Parameters},
    },
    model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};

/// Default upper bound on bytes returned by `read_file`. Override per call via
/// [`ReadFileInput::max_bytes`].
pub const DEFAULT_MAX_READ_BYTES: u64 = 1 << 20;

/// Default upper bound on entries returned by `list_dir`. Override per call via
/// [`ListDirInput::max_entries`].
pub const DEFAULT_MAX_LIST_ENTRIES: u32 = 1024;

/// Default upper bound on matches returned by `glob`. Override per call via
/// [`GlobInput::max_matches`].
pub const DEFAULT_MAX_GLOB_MATCHES: u32 = 1024;

// ---------------------------------------------------------------------------
// Tool input/output types
// ---------------------------------------------------------------------------

/// Arguments to `read_file`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ReadFileInput {
    /// Path to the file. Relative paths resolve against the server's CWD.
    pub path: String,
    /// Reject files larger than this many bytes. Default: 1 MiB.
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

/// Result of `read_file`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReadFileOutput {
    /// The path that was read, as supplied by the caller.
    pub path: String,
    /// Size of the file in bytes.
    pub bytes: u64,
    /// File contents, decoded as UTF-8.
    pub content: String,
}

/// Arguments to `write_file`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct WriteFileInput {
    /// Destination path. Relative paths resolve against the server's CWD.
    pub path: String,
    /// UTF-8 contents to write. The file is fully overwritten.
    pub content: String,
    /// Create missing parent directories if true. Default: false.
    #[serde(default)]
    pub create_dirs: bool,
}

/// Result of `write_file`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WriteFileOutput {
    /// The path that was written.
    pub path: String,
    /// Number of bytes written.
    pub bytes_written: u64,
}

/// Arguments to `list_dir`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ListDirInput {
    /// Directory path. Relative paths resolve against the server's CWD.
    pub path: String,
    /// Walk subdirectories if true. Default: false.
    #[serde(default)]
    pub recursive: bool,
    /// Cap on the number of entries returned. Default: 1024.
    #[serde(default)]
    pub max_entries: Option<u32>,
}

/// One row returned by `list_dir`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DirEntry {
    /// File or directory name (final path component).
    pub name: String,
    /// Full path as returned by the underlying filesystem walk.
    pub path: String,
    /// True if the entry is a directory.
    pub is_dir: bool,
    /// File size in bytes. Zero for directories.
    pub size_bytes: u64,
}

/// Result of `list_dir`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ListDirOutput {
    /// The directory that was listed.
    pub path: String,
    /// Entries discovered, in unspecified order.
    pub entries: Vec<DirEntry>,
    /// True if the result was capped by `max_entries`.
    pub truncated: bool,
}

/// Arguments to `glob`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct GlobInput {
    /// Glob pattern (e.g. `**/*.rs`). Standard `gitignore`-style glob syntax;
    /// brace alternation like `**/*.{rs,toml}` is supported.
    pub pattern: String,
    /// Directory the pattern is rooted at. Default: server CWD (`.`).
    #[serde(default)]
    pub root: Option<String>,
    /// Cap on the number of matches returned. Default: 1024.
    #[serde(default)]
    pub max_matches: Option<u32>,
    /// Honor `.gitignore`, `.git/info/exclude`, and global gitignore. Default:
    /// true. `.git/` is always excluded regardless of this flag.
    #[serde(default)]
    pub respect_gitignore: Option<bool>,
}

/// Result of `glob`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GlobOutput {
    /// Echo of the input pattern.
    pub pattern: String,
    /// Echo of the resolved root.
    pub root: String,
    /// Matched paths, relative to `root` when possible.
    pub matches: Vec<String>,
    /// True if the result was capped by `max_matches`.
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors a tool handler can surface to the caller.
#[derive(Debug, thiserror::Error)]
pub enum FsToolError {
    /// Underlying filesystem I/O failed.
    #[error("{op}: {source}")]
    Io {
        /// Short label describing the operation that failed.
        op: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// File exceeded the byte cap requested by the caller.
    #[error("file too large: {bytes} bytes (limit {limit})")]
    FileTooLarge {
        /// Actual file size.
        bytes: u64,
        /// Cap that was applied.
        limit: u64,
    },
    /// File contents were not valid UTF-8.
    #[error("file is not valid UTF-8: {0}")]
    NotUtf8(String),
    /// Caller passed an invalid argument (bad pattern, empty path, …).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// Glob pattern failed to compile.
    #[error("invalid glob pattern: {0}")]
    Glob(String),
    /// Resolved path falls outside the configured project root.
    #[error("path is outside project root: {path}")]
    OutsideRoot {
        /// The offending path, as supplied by the caller.
        path: String,
    },
}

impl From<FsToolError> for ErrorData {
    fn from(err: FsToolError) -> Self {
        match err {
            FsToolError::InvalidArgument(_) | FsToolError::Glob(_) => {
                ErrorData::invalid_params(err.to_string(), None)
            }
            FsToolError::FileTooLarge { .. }
            | FsToolError::NotUtf8(_)
            | FsToolError::OutsideRoot { .. } => ErrorData::invalid_request(err.to_string(), None),
            FsToolError::Io { .. } => ErrorData::internal_error(err.to_string(), None),
        }
    }
}

fn io_err(op: &str, source: std::io::Error) -> FsToolError {
    FsToolError::Io {
        op: op.to_string(),
        source,
    }
}

/// Map a blocking-task `JoinError` to `ErrorData::internal_error`. Used by
/// the edit handlers that delegate their actual filesystem write to
/// [`tokio::task::spawn_blocking`].
fn join_err(e: tokio::task::JoinError) -> ErrorData {
    ErrorData::internal_error(format!("write task failed: {e}"), None)
}

/// Drive [`edit::atomic_write`] on a blocking task and surface its errors.
/// Shared by `replace`, `insert`, and `multi_edit` so each handler stays a
/// single readable expression.
async fn spawn_atomic_write(path: PathBuf, contents: String) -> Result<(), ErrorData> {
    tokio::task::spawn_blocking(move || edit::atomic_write(&path, contents.as_bytes()))
        .await
        .map_err(join_err)?
        .map_err(Into::into)
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// MCP server exposing the filesystem tools.
#[derive(Debug, Clone)]
pub struct FsTools {
    #[allow(dead_code)] // Read by the `#[tool_handler]` macro expansion.
    tool_router: ToolRouter<Self>,
    /// When `Some`, all tools confine inputs to this canonicalized directory.
    root: Option<PathBuf>,
}

impl Default for FsTools {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl FsTools {
    /// Construct a new server instance with the default tool router and **no**
    /// path containment. Tools resolve paths against the process CWD with the
    /// full privileges of the invoking user.
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            root: None,
        }
    }

    /// Construct a containment-mode server. All four tools will refuse to
    /// touch paths outside `root`, even via `..` or symlinks. `root` must
    /// exist and is canonicalized once at construction.
    pub fn with_root(root: impl AsRef<Path>) -> std::io::Result<Self> {
        let canon = std::fs::canonicalize(root.as_ref())?;
        Ok(Self {
            tool_router: Self::tool_router(),
            root: Some(canon),
        })
    }

    /// Returns the configured project root, if containment is enabled.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Read a UTF-8 text file from disk, capped at `max_bytes`.
    #[tool(
        name = "read_file",
        description = "Read a UTF-8 text file. Errors if the file exceeds max_bytes (default 1 MiB) or is not valid UTF-8."
    )]
    pub async fn read_file(
        &self,
        Parameters(input): Parameters<ReadFileInput>,
    ) -> Result<Json<ReadFileOutput>, ErrorData> {
        if input.path.is_empty() {
            return Err(FsToolError::InvalidArgument("path is empty".into()).into());
        }
        let limit = input.max_bytes.unwrap_or(DEFAULT_MAX_READ_BYTES);
        let path = self.resolve_existing(&input.path)?;

        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|e| io_err("stat", e))?;
        if !metadata.is_file() {
            return Err(FsToolError::InvalidArgument(format!(
                "{} is not a regular file",
                input.path
            ))
            .into());
        }
        let bytes = metadata.len();
        if bytes > limit {
            return Err(FsToolError::FileTooLarge { bytes, limit }.into());
        }

        let raw = tokio::fs::read(&path)
            .await
            .map_err(|e| io_err("read", e))?;
        let content = String::from_utf8(raw).map_err(|e| FsToolError::NotUtf8(e.to_string()))?;

        Ok(Json(ReadFileOutput {
            path: input.path,
            bytes,
            content,
        }))
    }

    /// Write `content` to `path`, fully overwriting any existing file.
    #[tool(
        name = "write_file",
        description = "Overwrite (or create) a UTF-8 text file. Set create_dirs=true to create missing parent directories."
    )]
    pub async fn write_file(
        &self,
        Parameters(input): Parameters<WriteFileInput>,
    ) -> Result<Json<WriteFileOutput>, ErrorData> {
        if input.path.is_empty() {
            return Err(FsToolError::InvalidArgument("path is empty".into()).into());
        }
        let path = self.resolve_for_write(&input.path)?;

        if input.create_dirs {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| io_err("create_dir_all", e))?;
            }
        }

        let bytes_written = input.content.len() as u64;
        tokio::fs::write(&path, input.content.as_bytes())
            .await
            .map_err(|e| io_err("write", e))?;

        Ok(Json(WriteFileOutput {
            path: input.path,
            bytes_written,
        }))
    }

    /// List the entries of a directory.
    #[tool(
        name = "list_dir",
        description = "List entries in a directory. Set recursive=true to walk subdirectories."
    )]
    pub async fn list_dir(
        &self,
        Parameters(input): Parameters<ListDirInput>,
    ) -> Result<Json<ListDirOutput>, ErrorData> {
        if input.path.is_empty() {
            return Err(FsToolError::InvalidArgument("path is empty".into()).into());
        }
        let limit = input.max_entries.unwrap_or(DEFAULT_MAX_LIST_ENTRIES) as usize;
        let root = self.resolve_existing(&input.path)?;

        let metadata = tokio::fs::metadata(&root)
            .await
            .map_err(|e| io_err("stat", e))?;
        if !metadata.is_dir() {
            return Err(
                FsToolError::InvalidArgument(format!("{} is not a directory", input.path)).into(),
            );
        }

        let walk_root = root.clone();
        let recursive = input.recursive;
        let entries =
            tokio::task::spawn_blocking(move || -> Result<(Vec<DirEntry>, bool), FsToolError> {
                walk_dir(&walk_root, recursive, limit)
            })
            .await
            .map_err(|e| FsToolError::InvalidArgument(format!("walk task panicked: {e}")))??;

        Ok(Json(ListDirOutput {
            path: input.path,
            entries: entries.0,
            truncated: entries.1,
        }))
    }

    /// Expand a glob pattern relative to `root`.
    #[tool(
        name = "glob",
        description = "Expand a glob pattern (e.g. **/*.rs) under a root directory. Honors .gitignore / .git/info/exclude / global gitignore by default (disable with respect_gitignore=false); .git/ is always excluded. Returns regular files only — directories and symlinks are skipped. Uses gitignore-style pattern grammar (brace alternation supported)."
    )]
    pub async fn glob(
        &self,
        Parameters(input): Parameters<GlobInput>,
    ) -> Result<Json<GlobOutput>, ErrorData> {
        if input.pattern.is_empty() {
            return Err(FsToolError::InvalidArgument("pattern is empty".into()).into());
        }
        if path_has_parent_dir(Path::new(&input.pattern)) {
            return Err(FsToolError::OutsideRoot {
                path: input.pattern.clone(),
            }
            .into());
        }
        let limit = input.max_matches.unwrap_or(DEFAULT_MAX_GLOB_MATCHES) as usize;
        let root_str = input.root.clone().unwrap_or_else(|| ".".into());

        // Resolve the glob's own root. With containment, "." means the project
        // root; without, it means the process CWD (existing behavior).
        let resolved_root = if let Some(project_root) = self.root.as_deref() {
            let r = self.resolve_existing(&root_str)?;
            // Belt-and-suspenders: resolve_existing already enforces this.
            if !is_within(&r, project_root) {
                return Err(FsToolError::OutsideRoot { path: root_str }.into());
            }
            r
        } else {
            PathBuf::from(&root_str)
        };

        let pattern = input.pattern.clone();
        let project_root = self.root.clone();
        let respect = input.respect_gitignore.unwrap_or(true);

        let (matches, truncated) =
            tokio::task::spawn_blocking(move || -> Result<(Vec<String>, bool), FsToolError> {
                // Build the user's pattern matcher separately from the walker.
                // The `ignore` crate's `OverrideBuilder` treats `add()` patterns
                // as *whitelists* with the highest precedence, which bypasses
                // `.gitignore` for matching files (see `ignore::dir::matched`).
                // We want the opposite: gitignore should hide files even if
                // they match the user's glob. So we let `WalkBuilder` enumerate
                // gitignore-respecting entries unrestricted, then post-filter
                // each entry against a `globset` matcher.
                let pat_glob = globset::GlobBuilder::new(&pattern)
                    .literal_separator(false)
                    .build()
                    .map_err(|e| FsToolError::Glob(e.to_string()))?
                    .compile_matcher();

                let mut builder = ignore::WalkBuilder::new(&resolved_root);
                builder
                    .git_ignore(respect)
                    .git_global(respect)
                    .git_exclude(respect)
                    // Honor `.gitignore` even when the search root isn't
                    // inside a git repo — agents commonly invoke `glob`
                    // outside a checkout, but still expect a project's
                    // top-level `.gitignore` to apply.
                    .require_git(false)
                    // Allow dotfiles to be matched when the pattern asks
                    // for them (e.g. `**/.config/*.toml`). `.git/` is
                    // pruned below via `filter_entry` regardless of
                    // `respect_gitignore`.
                    .hidden(false)
                    .follow_links(false)
                    // Prune `.git/` before the walker descends into it.
                    // `WalkBuilder` would otherwise only filter `.git/` via
                    // the gitignore matcher, which lets it leak through
                    // when `respect_gitignore=false`. Pruning at the entry
                    // level also avoids the cost of enumerating its
                    // contents.
                    .filter_entry(|e| e.file_name() != std::ffi::OsStr::new(".git"));

                let mut out = Vec::new();
                let mut truncated = false;
                for entry in builder.build() {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(e) => {
                            tracing::debug!(error = %e, "skipping walker entry");
                            continue;
                        }
                    };
                    if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        continue;
                    }
                    let p = entry.into_path();
                    // Match the user's pattern against the path relative to
                    // the search root so a pattern like `**/*.rs` does what
                    // an agent expects, regardless of where the root sits.
                    let rel_path = p.strip_prefix(&resolved_root).unwrap_or(&p);
                    if !pat_glob.is_match(rel_path) {
                        continue;
                    }
                    if out.len() >= limit {
                        truncated = true;
                        break;
                    }
                    // Containment filter: drop any match whose canonical path
                    // falls outside the project root (e.g. via a symlink).
                    if let Some(root) = project_root.as_deref() {
                        match std::fs::canonicalize(&p) {
                            Ok(canon) if is_within(&canon, root) => {}
                            _ => continue,
                        }
                    }
                    out.push(to_forward_slashes(rel_path));
                }
                Ok((out, truncated))
            })
            .await
            .map_err(|e| FsToolError::InvalidArgument(format!("glob task panicked: {e}")))??;

        Ok(Json(GlobOutput {
            pattern: input.pattern,
            root: root_str,
            matches,
            truncated,
        }))
    }

    /// Replace `old` with `new` in a file. Default contract: `old` must be
    /// unique. Pass `count: { exactly: N }` or `count: "all"` to relax.
    #[tool(
        name = "replace",
        description = "Replace `old` with `new` in a file. By default `old` must be unique; pass count to relax."
    )]
    pub async fn replace(
        &self,
        Parameters(input): Parameters<edit::ReplaceInput>,
    ) -> Result<Json<edit::ReplaceOutput>, ErrorData> {
        let (path, text) = self.read_text_for_edit(&input.path).await?;
        let (new_text, n) = edit::apply_replace(&text, &input.old, &input.new, input.count)?;
        spawn_atomic_write(path, new_text).await?;
        Ok(Json(edit::ReplaceOutput {
            path: input.path,
            replacements: n,
        }))
    }

    /// Insert a block of text after a 1-indexed line. `after_line=0` prepends.
    #[tool(
        name = "insert",
        description = "Insert text after the Nth line of a file (1-indexed; 0 prepends)."
    )]
    pub async fn insert(
        &self,
        Parameters(input): Parameters<edit::InsertInput>,
    ) -> Result<Json<edit::InsertOutput>, ErrorData> {
        let (path, text) = self.read_text_for_edit(&input.path).await?;
        let (new_text, n) = edit::apply_insert(&text, input.after_line, &input.text)?;
        spawn_atomic_write(path, new_text).await?;
        Ok(Json(edit::InsertOutput {
            path: input.path,
            lines_inserted: n,
        }))
    }

    /// Apply a sequence of edits with logical-failure atomicity: if any edit
    /// in the batch fails, the file on disk is left untouched.
    ///
    /// Note: this is *logical* atomicity (we don't commit until every edit
    /// computes), not OS-crash atomicity. The atomic-write step itself does
    /// give crash-safe replacement of the final file via tmp + rename + parent
    /// fsync; see [`edit::atomic_write`].
    #[tool(
        name = "multi_edit",
        description = "Apply a batch of replace/insert edits atomically. On any failure, the original file is unchanged."
    )]
    pub async fn multi_edit(
        &self,
        Parameters(input): Parameters<edit::MultiEditInput>,
    ) -> Result<Json<edit::MultiEditOutput>, ErrorData> {
        let (path, text) = self.read_text_for_edit(&input.path).await?;
        let mut current = text;
        for (i, entry) in input.edits.iter().enumerate() {
            // Identify the failing step so the agent can correct just that one
            // rather than re-deriving its batch from a generic error.
            let kind = match entry {
                edit::MultiEdit::Replace { .. } => "replace",
                edit::MultiEdit::Insert { .. } => "insert",
            };
            let step = i + 1;
            current = match entry {
                edit::MultiEdit::Replace { old, new, count } => {
                    edit::apply_replace(&current, old, new, *count)
                        .map_err(|e| {
                            FsToolError::InvalidArgument(format!("edit #{step} ({kind}): {e}"))
                        })?
                        .0
                }
                edit::MultiEdit::Insert { after_line, text } => {
                    edit::apply_insert(&current, *after_line, text)
                        .map_err(|e| {
                            FsToolError::InvalidArgument(format!("edit #{step} ({kind}): {e}"))
                        })?
                        .0
                }
            };
        }
        spawn_atomic_write(path, current).await?;
        Ok(Json(edit::MultiEditOutput { path: input.path }))
    }

    // -- path resolution helpers --------------------------------------------

    /// Shared setup for the three edit handlers: empty-path check, write
    /// resolution, deny-floor check, read, and UTF-8 decode. Returns the
    /// resolved (canonical) target path and the current file contents.
    async fn read_text_for_edit(&self, raw: &str) -> Result<(PathBuf, String), FsToolError> {
        if raw.is_empty() {
            return Err(FsToolError::InvalidArgument("path is empty".into()));
        }
        let path = self.resolve_for_write(raw)?;
        self.check_deny_floor(raw, &path)?;
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| io_err("read", e))?;
        let text = String::from_utf8(bytes).map_err(|e| FsToolError::NotUtf8(e.to_string()))?;
        Ok((path, text))
    }

    /// Evaluate the sensitive-path deny floor against the path *relative to
    /// the project root*. With containment off, the raw input is used — that
    /// preserves the no-sandbox semantics of [`FsTools::new`] while still
    /// catching obvious shapes like `.env` or `secrets/credentials.json`.
    fn check_deny_floor(&self, raw: &str, resolved: &Path) -> Result<(), FsToolError> {
        let relative = match self.root.as_deref() {
            Some(root) => resolved
                .strip_prefix(root)
                .expect("resolve_for_write guarantees containment under root"),
            None => Path::new(raw),
        };
        if edit::is_denied(relative) {
            return Err(FsToolError::OutsideRoot {
                path: raw.to_string(),
            });
        }
        Ok(())
    }

    /// Resolve an input path that **must already exist** (read/list).
    ///
    /// With containment on:
    /// - reject any input containing `..`,
    /// - join relative paths onto the project root,
    /// - canonicalize the result and require it to lie under the root.
    ///
    /// With containment off, the path is returned verbatim — preserves the
    /// pre-existing CWD-relative behavior used by [`FsTools::new`].
    fn resolve_existing(&self, raw: &str) -> Result<PathBuf, FsToolError> {
        let Some(root) = self.root.as_deref() else {
            return Ok(PathBuf::from(raw));
        };
        let input = Path::new(raw);
        if path_has_parent_dir(input) {
            return Err(FsToolError::OutsideRoot {
                path: raw.to_string(),
            });
        }
        let candidate = if input.is_absolute() {
            input.to_path_buf()
        } else {
            root.join(input)
        };
        let canon = std::fs::canonicalize(&candidate).map_err(|e| io_err("canonicalize", e))?;
        if !is_within(&canon, root) {
            return Err(FsToolError::OutsideRoot {
                path: raw.to_string(),
            });
        }
        Ok(canon)
    }

    /// Resolve an input path that **may not yet exist** (write).
    ///
    /// Walks up to the first existing ancestor, canonicalizes it, then
    /// re-appends the missing tail. This catches symlinked ancestors before
    /// any directory creation happens.
    fn resolve_for_write(&self, raw: &str) -> Result<PathBuf, FsToolError> {
        let Some(root) = self.root.as_deref() else {
            return Ok(PathBuf::from(raw));
        };
        let input = Path::new(raw);
        if path_has_parent_dir(input) {
            return Err(FsToolError::OutsideRoot {
                path: raw.to_string(),
            });
        }
        let candidate = if input.is_absolute() {
            input.to_path_buf()
        } else {
            root.join(input)
        };

        // Find the first existing ancestor and canonicalize it.
        let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
        let mut cursor: &Path = &candidate;
        let canon_existing = loop {
            match std::fs::canonicalize(cursor) {
                Ok(c) => break c,
                Err(_) => match cursor.parent() {
                    Some(parent) => {
                        if let Some(name) = cursor.file_name() {
                            tail.push(name);
                        }
                        cursor = parent;
                    }
                    None => {
                        // Hit the filesystem root with nothing canonicalizable.
                        return Err(FsToolError::OutsideRoot {
                            path: raw.to_string(),
                        });
                    }
                },
            }
        };

        // Reattach the missing tail in original order.
        let mut resolved = canon_existing;
        for name in tail.into_iter().rev() {
            resolved.push(name);
        }

        if !is_within(&resolved, root) {
            return Err(FsToolError::OutsideRoot {
                path: raw.to_string(),
            });
        }
        Ok(resolved)
    }
}

#[tool_handler]
impl ServerHandler for FsTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::default())
            .with_server_info(
                Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
                    .with_description(
                        "Savvagent filesystem tools (read_file, write_file, list_dir, glob)",
                    ),
            )
            .with_instructions(
                "Filesystem tool server for Savvagent. v0.1 has no sandbox; the host \
                 is expected to confirm destructive calls before routing them.",
            )
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// True if `path` contains any `..` component.
fn path_has_parent_dir(path: &Path) -> bool {
    use std::path::Component;
    path.components().any(|c| matches!(c, Component::ParentDir))
}

/// True if `path` is `root` itself or lies underneath it.
///
/// Both arguments are expected to be canonical (caller's responsibility).
fn is_within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

/// Render a path as a string using `/` as the component separator regardless
/// of host OS. Used at SPP wire boundaries (e.g. `glob` match strings, `list_dir`
/// entries) so cross-platform consumers see one canonical shape. On Unix the
/// backslash is a legal filename character, so we only rewrite when running on
/// Windows.
fn to_forward_slashes(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}

fn walk_dir(
    root: &Path,
    recursive: bool,
    limit: usize,
) -> Result<(Vec<DirEntry>, bool), FsToolError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut truncated = false;

    while let Some(dir) = stack.pop() {
        let read = std::fs::read_dir(&dir).map_err(|e| io_err("read_dir", e))?;
        for entry in read {
            if out.len() >= limit {
                truncated = true;
                return Ok((out, truncated));
            }
            let entry = entry.map_err(|e| io_err("read_dir entry", e))?;
            let meta = entry.metadata().map_err(|e| io_err("entry metadata", e))?;
            let is_dir = meta.is_dir();
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            out.push(DirEntry {
                name,
                path: path.to_string_lossy().into_owned(),
                is_dir,
                size_bytes: if is_dir { 0 } else { meta.len() },
            });
            if recursive && is_dir {
                stack.push(path);
            }
        }
    }
    Ok((out, truncated))
}

// ---------------------------------------------------------------------------
// Binary entry point
// ---------------------------------------------------------------------------

/// Serve [`FsTools`] over a stdio MCP transport. Shared between the
/// `savvagent-tool-fs` binary in this crate and the bundled shim in the
/// `savvagent` crate's release archive.
pub async fn run() -> anyhow::Result<()> {
    use rmcp::{ServiceExt, transport::stdio};

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    tracing::info!(
        "savvagent-tool-fs {} starting on stdio",
        env!("CARGO_PKG_VERSION")
    );

    let tools = build_tools_from_env();
    let service = tools.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// Build an [`FsTools`] honoring `SAVVAGENT_TOOL_FS_ROOT`. Falls back to the
/// process CWD; if both fail to canonicalize, returns the unrestricted
/// constructor so the binary still serves something useful.
fn build_tools_from_env() -> FsTools {
    let env_root = std::env::var("SAVVAGENT_TOOL_FS_ROOT")
        .ok()
        .filter(|s| !s.is_empty());
    if let Some(root) = env_root {
        match FsTools::with_root(&root) {
            Ok(t) => {
                tracing::info!(root = %root, "tool-fs containment enabled (env)");
                return t;
            }
            Err(e) => {
                tracing::warn!(
                    root = %root,
                    error = %e,
                    "SAVVAGENT_TOOL_FS_ROOT failed to canonicalize; falling back to CWD",
                );
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(t) = FsTools::with_root(&cwd)
    {
        tracing::info!(root = %cwd.display(), "tool-fs containment enabled (cwd)");
        return t;
    }
    tracing::warn!("tool-fs running without containment (no usable root)");
    FsTools::new()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn to_forward_slashes_emits_posix_separators() {
        // PathBuf::from_iter uses the host's native separator, so on Windows
        // this yields `a\b\c.rs`. The helper must collapse both shapes to
        // a single canonical forward-slash form.
        let p: PathBuf = ["a", "b", "c.rs"].iter().collect();
        assert_eq!(to_forward_slashes(&p), "a/b/c.rs");

        // A single-component path has no separator and is returned as-is.
        let p: PathBuf = ["solo.rs"].iter().collect();
        assert_eq!(to_forward_slashes(&p), "solo.rs");
    }

    #[tokio::test]
    async fn read_file_round_trips_utf8() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        tokio::fs::write(&path, b"hello world").await.unwrap();

        let tools = FsTools::new();
        let out = tools
            .read_file(Parameters(ReadFileInput {
                path: path.to_string_lossy().into_owned(),
                max_bytes: None,
            }))
            .await
            .unwrap();
        assert_eq!(out.0.bytes, 11);
        assert_eq!(out.0.content, "hello world");
    }

    #[tokio::test]
    async fn read_file_rejects_oversize() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("big.txt");
        tokio::fs::write(&path, vec![b'x'; 32]).await.unwrap();

        let tools = FsTools::new();
        let err = tools
            .read_file(Parameters(ReadFileInput {
                path: path.to_string_lossy().into_owned(),
                max_bytes: Some(8),
            }))
            .await
            .err()
            .expect("expected error");
        assert!(err.message.contains("too large"), "{}", err.message);
    }

    #[tokio::test]
    async fn read_file_rejects_non_utf8() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bin");
        tokio::fs::write(&path, [0xff, 0xfe, 0xfd]).await.unwrap();

        let tools = FsTools::new();
        let err = tools
            .read_file(Parameters(ReadFileInput {
                path: path.to_string_lossy().into_owned(),
                max_bytes: None,
            }))
            .await
            .err()
            .expect("expected error");
        assert!(err.message.contains("UTF-8"), "{}", err.message);
    }

    #[tokio::test]
    async fn write_file_creates_parents_when_requested() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a/b/c.txt");

        let tools = FsTools::new();
        let out = tools
            .write_file(Parameters(WriteFileInput {
                path: path.to_string_lossy().into_owned(),
                content: "hi".into(),
                create_dirs: true,
            }))
            .await
            .unwrap();
        assert_eq!(out.0.bytes_written, 2);
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "hi");
    }

    #[tokio::test]
    async fn write_file_overwrites() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("note.txt");
        tokio::fs::write(&path, b"old").await.unwrap();

        let tools = FsTools::new();
        tools
            .write_file(Parameters(WriteFileInput {
                path: path.to_string_lossy().into_owned(),
                content: "new!".into(),
                create_dirs: false,
            }))
            .await
            .unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "new!");
    }

    #[tokio::test]
    async fn list_dir_flat() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("a.txt"), b"a")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("b.txt"), b"bb")
            .await
            .unwrap();
        tokio::fs::create_dir(dir.path().join("nested"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("nested/c.txt"), b"ccc")
            .await
            .unwrap();

        let tools = FsTools::new();
        let out = tools
            .list_dir(Parameters(ListDirInput {
                path: dir.path().to_string_lossy().into_owned(),
                recursive: false,
                max_entries: None,
            }))
            .await
            .unwrap();
        let names: Vec<_> = out.0.entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names.len(), 3, "{:?}", names);
        assert!(names.contains(&"a.txt".to_string()));
        assert!(names.contains(&"nested".to_string()));
        assert!(out.0.entries.iter().any(|e| e.name == "nested" && e.is_dir));
    }

    #[tokio::test]
    async fn list_dir_recursive() {
        let dir = tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("sub")).await.unwrap();
        tokio::fs::write(dir.path().join("sub/x.txt"), b"x")
            .await
            .unwrap();

        let tools = FsTools::new();
        let out = tools
            .list_dir(Parameters(ListDirInput {
                path: dir.path().to_string_lossy().into_owned(),
                recursive: true,
                max_entries: None,
            }))
            .await
            .unwrap();
        assert!(out.0.entries.iter().any(|e| e.name == "x.txt"));
    }

    #[tokio::test]
    async fn list_dir_truncates() {
        let dir = tempdir().unwrap();
        for i in 0..5 {
            tokio::fs::write(dir.path().join(format!("f{i}.txt")), b"x")
                .await
                .unwrap();
        }

        let tools = FsTools::new();
        let out = tools
            .list_dir(Parameters(ListDirInput {
                path: dir.path().to_string_lossy().into_owned(),
                recursive: false,
                max_entries: Some(3),
            }))
            .await
            .unwrap();
        assert_eq!(out.0.entries.len(), 3);
        assert!(out.0.truncated);
    }

    #[tokio::test]
    async fn glob_matches_under_root() {
        let dir = tempdir().unwrap();
        tokio::fs::write(dir.path().join("keep.rs"), b"")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("skip.txt"), b"")
            .await
            .unwrap();
        tokio::fs::create_dir(dir.path().join("deep"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("deep/also.rs"), b"")
            .await
            .unwrap();

        let tools = FsTools::new();
        let out = tools
            .glob(Parameters(GlobInput {
                pattern: "**/*.rs".into(),
                root: Some(dir.path().to_string_lossy().into_owned()),
                max_matches: None,
                respect_gitignore: None,
            }))
            .await
            .unwrap();
        assert!(out.0.matches.iter().any(|m| m.ends_with("keep.rs")));
        assert!(out.0.matches.iter().any(|m| m.ends_with("also.rs")));
        assert!(!out.0.matches.iter().any(|m| m.ends_with("skip.txt")));
    }

    #[tokio::test]
    async fn glob_excludes_gitignored_by_default() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::write(dir.path().join("kept.rs"), b"").unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/skip.rs"), b"").unwrap();

        let tools = FsTools::with_root(dir.path()).unwrap();
        let out = tools
            .glob(Parameters(GlobInput {
                pattern: "**/*.rs".into(),
                root: Some(".".into()),
                max_matches: None,
                respect_gitignore: None,
            }))
            .await
            .unwrap();
        let files: Vec<_> = out.0.matches.iter().map(|s| s.as_str()).collect();
        assert_eq!(files, vec!["kept.rs"], "{:?}", out.0);
    }

    #[tokio::test]
    async fn glob_honors_nested_gitignore() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/.gitignore"), "private.rs\n").unwrap();
        std::fs::write(dir.path().join("sub/private.rs"), b"").unwrap();
        std::fs::write(dir.path().join("sub/public.rs"), b"").unwrap();

        let tools = FsTools::with_root(dir.path()).unwrap();
        let out = tools
            .glob(Parameters(GlobInput {
                pattern: "**/*.rs".into(),
                root: Some(".".into()),
                max_matches: None,
                respect_gitignore: None,
            }))
            .await
            .unwrap();
        let mut files: Vec<_> = out.0.matches.iter().map(|s| s.as_str()).collect();
        files.sort();
        assert_eq!(files, vec!["sub/public.rs"], "{:?}", out.0);
    }

    #[tokio::test]
    async fn glob_includes_gitignored_when_disabled() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::write(dir.path().join("kept.rs"), b"").unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/skip.rs"), b"").unwrap();

        let tools = FsTools::with_root(dir.path()).unwrap();
        let out = tools
            .glob(Parameters(GlobInput {
                pattern: "**/*.rs".into(),
                root: Some(".".into()),
                max_matches: None,
                respect_gitignore: Some(false),
            }))
            .await
            .unwrap();
        let mut files: Vec<_> = out.0.matches.iter().map(|s| s.as_str()).collect();
        files.sort();
        assert_eq!(files, vec!["kept.rs", "target/skip.rs"], "{:?}", out.0);
    }

    #[tokio::test]
    async fn glob_always_excludes_dot_git() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), b"ref: refs/heads/main").unwrap();
        std::fs::write(dir.path().join("a.rs"), b"").unwrap();

        let tools = FsTools::with_root(dir.path()).unwrap();
        let out = tools
            .glob(Parameters(GlobInput {
                pattern: "**/*".into(),
                root: Some(".".into()),
                max_matches: None,
                respect_gitignore: Some(false), // even when off
            }))
            .await
            .unwrap();
        assert!(
            !out.0.matches.iter().any(|m| m.contains(".git/")),
            ".git/ leaked into glob results: {:?}",
            out.0
        );
    }

    #[tokio::test]
    async fn glob_brace_pattern() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), b"").unwrap();
        std::fs::write(dir.path().join("b.toml"), b"").unwrap();
        std::fs::write(dir.path().join("c.txt"), b"").unwrap();

        let tools = FsTools::with_root(dir.path()).unwrap();
        let out = tools
            .glob(Parameters(GlobInput {
                pattern: "*.{rs,toml}".into(),
                root: Some(".".into()),
                max_matches: None,
                respect_gitignore: None,
            }))
            .await
            .unwrap();
        let mut files: Vec<_> = out.0.matches.iter().map(|s| s.as_str()).collect();
        files.sort();
        assert_eq!(files, vec!["a.rs", "b.toml"], "{:?}", out.0);
    }

    // ---- Layer 1 path containment (FsTools::with_root) -------------------

    #[tokio::test]
    async fn with_root_rejects_absolute_outside() {
        let inside = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("secret.txt");
        tokio::fs::write(&outside_file, b"shh").await.unwrap();

        let tools = FsTools::with_root(inside.path()).unwrap();
        let err = tools
            .read_file(Parameters(ReadFileInput {
                path: outside_file.to_string_lossy().into_owned(),
                max_bytes: None,
            }))
            .await
            .err()
            .expect("expected error");
        assert!(
            err.message.to_lowercase().contains("outside"),
            "{}",
            err.message
        );
    }

    #[tokio::test]
    async fn with_root_rejects_parent_traversal() {
        let inside = tempdir().unwrap();
        // Make a real file outside the root the traversal would target.
        let parent = inside.path().parent().unwrap();
        let escape = parent.join("escape-target.txt");
        let _ = tokio::fs::write(&escape, b"x").await; // best-effort

        let tools = FsTools::with_root(inside.path()).unwrap();
        let err = tools
            .read_file(Parameters(ReadFileInput {
                path: "../escape-target.txt".into(),
                max_bytes: None,
            }))
            .await
            .err()
            .expect("expected error");
        assert!(
            err.message.to_lowercase().contains("outside"),
            "{}",
            err.message
        );

        let _ = tokio::fs::remove_file(&escape).await;
    }

    #[tokio::test]
    async fn with_root_resolves_relative_under_root() {
        let inside = tempdir().unwrap();
        tokio::fs::create_dir(inside.path().join("sub"))
            .await
            .unwrap();
        tokio::fs::write(inside.path().join("sub/x.txt"), b"contents")
            .await
            .unwrap();

        let tools = FsTools::with_root(inside.path()).unwrap();
        let out = tools
            .read_file(Parameters(ReadFileInput {
                path: "sub/x.txt".into(),
                max_bytes: None,
            }))
            .await
            .unwrap();
        assert_eq!(out.0.content, "contents");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn with_root_rejects_symlink_escape() {
        let inside = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("target.txt");
        tokio::fs::write(&outside_file, b"shh").await.unwrap();

        let link = inside.path().join("link");
        std::os::unix::fs::symlink(&outside_file, &link).unwrap();

        let tools = FsTools::with_root(inside.path()).unwrap();
        let err = tools
            .read_file(Parameters(ReadFileInput {
                path: "link".into(),
                max_bytes: None,
            }))
            .await
            .err()
            .expect("expected error");
        assert!(
            err.message.to_lowercase().contains("outside"),
            "{}",
            err.message
        );
    }

    #[tokio::test]
    async fn with_root_write_rejects_outside() {
        let inside = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let target = outside.path().join("written.txt");

        let tools = FsTools::with_root(inside.path()).unwrap();
        let err = tools
            .write_file(Parameters(WriteFileInput {
                path: target.to_string_lossy().into_owned(),
                content: "nope".into(),
                create_dirs: false,
            }))
            .await
            .err()
            .expect("expected error");
        assert!(
            err.message.to_lowercase().contains("outside"),
            "{}",
            err.message
        );
        assert!(
            !target.exists(),
            "containment must not create files outside the root"
        );
    }

    #[tokio::test]
    async fn with_root_write_creates_dirs_inside_root() {
        let inside = tempdir().unwrap();

        let tools = FsTools::with_root(inside.path()).unwrap();
        let out = tools
            .write_file(Parameters(WriteFileInput {
                path: "a/b/c.txt".into(),
                content: "ok".into(),
                create_dirs: true,
            }))
            .await
            .unwrap();
        assert_eq!(out.0.bytes_written, 2);
        let written = tokio::fs::read_to_string(inside.path().join("a/b/c.txt"))
            .await
            .unwrap();
        assert_eq!(written, "ok");
    }

    #[test]
    fn is_within_filters_outside_paths() {
        let root = std::path::PathBuf::from("/root/project");
        assert!(is_within(&root, &root));
        assert!(is_within(
            &std::path::PathBuf::from("/root/project/sub/file.rs"),
            &root,
        ));
        assert!(!is_within(
            &std::path::PathBuf::from("/root/other/file.rs"),
            &root,
        ));
        assert!(!is_within(
            &std::path::PathBuf::from("/root/projectsibling/file.rs"),
            &root,
        ));
        assert!(!is_within(&std::path::PathBuf::from("/etc/passwd"), &root));
    }

    #[tokio::test]
    async fn with_root_glob_filters_outside_matches() {
        // Inside the root: keep.rs, deep/also.rs.
        // Outside the root: a sibling file the glob would otherwise match
        // through a symlink.
        //
        // NOTE: `follow_links(false)` on the `WalkBuilder` is the primary
        // defense here — the walker never descends through `escape-link`,
        // so the post-walk `is_within` canonicalize filter at the tail of
        // `glob` is not exercised by this test. That filter is covered by
        // the `is_within_filters_outside_paths` unit test above as
        // belt-and-suspenders.
        let inside = tempdir().unwrap();
        let outside = tempdir().unwrap();
        tokio::fs::write(inside.path().join("keep.rs"), b"")
            .await
            .unwrap();
        tokio::fs::create_dir(inside.path().join("deep"))
            .await
            .unwrap();
        tokio::fs::write(inside.path().join("deep/also.rs"), b"")
            .await
            .unwrap();
        tokio::fs::write(outside.path().join("escape.rs"), b"")
            .await
            .unwrap();

        // On unix, plant a symlink-to-outside inside the root so the glob
        // walker would otherwise traverse it.
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), inside.path().join("escape-link")).unwrap();

        let tools = FsTools::with_root(inside.path()).unwrap();
        let out = tools
            .glob(Parameters(GlobInput {
                pattern: "**/*.rs".into(),
                root: Some(".".into()),
                max_matches: None,
                respect_gitignore: None,
            }))
            .await
            .unwrap();

        assert!(
            out.0.matches.iter().any(|m| m.ends_with("keep.rs")),
            "{:?}",
            out.0.matches
        );
        assert!(
            out.0.matches.iter().any(|m| m.ends_with("also.rs")),
            "{:?}",
            out.0.matches
        );
        assert!(
            !out.0.matches.iter().any(|m| m.contains("escape.rs")),
            "containment must not surface matches outside the root: {:?}",
            out.0.matches
        );
    }

    #[tokio::test]
    async fn with_root_glob_rejects_parent_traversal() {
        let inside = tempdir().unwrap();
        let tools = FsTools::with_root(inside.path()).unwrap();
        let err = tools
            .glob(Parameters(GlobInput {
                pattern: "../*.rs".into(),
                root: Some(".".into()),
                max_matches: None,
                respect_gitignore: None,
            }))
            .await
            .err()
            .expect("expected error");
        assert!(
            err.message.to_lowercase().contains("outside"),
            "{}",
            err.message
        );
    }

    use crate::edit::{InsertInput, MultiEdit, MultiEditInput, ReplaceInput};

    #[tokio::test]
    async fn replace_tool_unique_match() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        tokio::fs::write(&p, b"foo bar baz").await.unwrap();

        let tools = FsTools::with_root(dir.path()).unwrap();
        let out = tools
            .replace(Parameters(ReplaceInput {
                path: "a.txt".into(),
                old: "bar".into(),
                new: "BAR".into(),
                count: None,
            }))
            .await
            .unwrap();
        assert_eq!(out.0.replacements, 1);
        assert_eq!(tokio::fs::read_to_string(&p).await.unwrap(), "foo BAR baz");
    }

    #[tokio::test]
    async fn replace_tool_rejects_env_path() {
        let dir = tempdir().unwrap();
        let p = dir.path().join(".env");
        tokio::fs::write(&p, b"SECRET=abc").await.unwrap();

        let tools = FsTools::with_root(dir.path()).unwrap();
        let err = tools
            .replace(Parameters(ReplaceInput {
                path: ".env".into(),
                old: "abc".into(),
                new: "xyz".into(),
                count: None,
            }))
            .await
            .err()
            .expect("deny-floor must reject .env");
        assert!(
            err.message.to_lowercase().contains("outside"),
            "{}",
            err.message
        );
        assert_eq!(tokio::fs::read_to_string(&p).await.unwrap(), "SECRET=abc");
    }

    #[tokio::test]
    async fn insert_tool_prepends() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        tokio::fs::write(&p, b"second\n").await.unwrap();

        let tools = FsTools::with_root(dir.path()).unwrap();
        let out = tools
            .insert(Parameters(InsertInput {
                path: "a.txt".into(),
                after_line: 0,
                text: "first".into(),
            }))
            .await
            .unwrap();
        assert_eq!(out.0.lines_inserted, 1);
        assert_eq!(
            tokio::fs::read_to_string(&p).await.unwrap(),
            "first\nsecond\n"
        );
    }

    #[tokio::test]
    async fn multi_edit_tool_atomic_on_failure() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        tokio::fs::write(&p, b"foo bar baz").await.unwrap();

        let tools = FsTools::with_root(dir.path()).unwrap();
        let err = tools
            .multi_edit(Parameters(MultiEditInput {
                path: "a.txt".into(),
                edits: vec![
                    MultiEdit::Replace {
                        old: "foo".into(),
                        new: "FOO".into(),
                        count: None,
                    },
                    MultiEdit::Replace {
                        old: "missing".into(),
                        new: "X".into(),
                        count: None,
                    },
                ],
            }))
            .await
            .err()
            .expect("second edit must fail");
        // Error must identify the failing step so the agent can fix just that
        // one (FIX 4) AND retain the underlying cause for diagnosis.
        assert!(
            err.message.contains("edit #2 (replace)"),
            "missing step prefix: {}",
            err.message
        );
        assert!(err.message.contains("not found"), "{}", err.message);
        assert_eq!(
            tokio::fs::read_to_string(&p).await.unwrap(),
            "foo bar baz",
            "atomicity broken — original file modified"
        );

        // No leftover tmp files in parent.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".savvagent-tmp.")
            })
            .collect();
        assert!(leftovers.is_empty(), "leftover tmp file: {leftovers:?}");
    }

    #[tokio::test]
    async fn multi_edit_tool_round_trip() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        tokio::fs::write(&p, b"line1\nline2\nline3\n")
            .await
            .unwrap();

        let tools = FsTools::with_root(dir.path()).unwrap();
        let out = tools
            .multi_edit(Parameters(MultiEditInput {
                path: "a.txt".into(),
                edits: vec![
                    MultiEdit::Replace {
                        old: "line2".into(),
                        new: "LINE_TWO".into(),
                        count: None,
                    },
                    MultiEdit::Insert {
                        after_line: 1,
                        text: "inserted".into(),
                    },
                ],
            }))
            .await
            .unwrap();
        assert_eq!(out.0.path, "a.txt");
        assert_eq!(
            tokio::fs::read_to_string(&p).await.unwrap(),
            "line1\ninserted\nLINE_TWO\nline3\n"
        );
    }

    #[tokio::test]
    async fn edit_tools_handle_workspace_path_containing_credential() {
        // Regression for the deny-floor relative-path fix: a workspace whose
        // absolute path itself contains "credential" must not false-positive
        // and reject every legitimate edit inside it.
        let parent = tempdir().unwrap();
        let workspace = parent.path().join("my-credentials-app");
        std::fs::create_dir(&workspace).unwrap();
        let target = workspace.join("source.rs");
        tokio::fs::write(&target, b"let x = 1;").await.unwrap();

        let tools = FsTools::with_root(&workspace).unwrap();
        let out = tools
            .replace(Parameters(ReplaceInput {
                path: "source.rs".into(),
                old: "x".into(),
                new: "y".into(),
                count: None,
            }))
            .await
            .expect("workspace path containing 'credential' must not false-positive");
        assert_eq!(out.0.replacements, 1);
        assert_eq!(
            tokio::fs::read_to_string(&target).await.unwrap(),
            "let y = 1;"
        );
    }

    #[tokio::test]
    async fn edit_tools_reject_absolute_path_outside_root() {
        // Containment: absolute paths outside the project root must be
        // rejected by every edit handler, and the target file must not be
        // touched.
        let inside = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let escape = outside.path().join("target.txt");
        tokio::fs::write(&escape, b"foo").await.unwrap();

        let tools = FsTools::with_root(inside.path()).unwrap();

        // replace
        let err = tools
            .replace(Parameters(ReplaceInput {
                path: escape.to_string_lossy().into_owned(),
                old: "foo".into(),
                new: "bar".into(),
                count: None,
            }))
            .await
            .err()
            .expect("replace must reject absolute paths outside the root");
        assert!(
            err.message.to_lowercase().contains("outside"),
            "{}",
            err.message
        );
        assert_eq!(tokio::fs::read_to_string(&escape).await.unwrap(), "foo");

        // insert
        let err = tools
            .insert(Parameters(InsertInput {
                path: escape.to_string_lossy().into_owned(),
                after_line: 0,
                text: "x".into(),
            }))
            .await
            .err()
            .expect("insert must reject absolute paths outside the root");
        assert!(
            err.message.to_lowercase().contains("outside"),
            "{}",
            err.message
        );
        assert_eq!(tokio::fs::read_to_string(&escape).await.unwrap(), "foo");

        // multi_edit
        let err = tools
            .multi_edit(Parameters(MultiEditInput {
                path: escape.to_string_lossy().into_owned(),
                edits: vec![MultiEdit::Replace {
                    old: "foo".into(),
                    new: "bar".into(),
                    count: None,
                }],
            }))
            .await
            .err()
            .expect("multi_edit must reject absolute paths outside the root");
        assert!(
            err.message.to_lowercase().contains("outside"),
            "{}",
            err.message
        );
        assert_eq!(tokio::fs::read_to_string(&escape).await.unwrap(), "foo");
    }

    #[tokio::test]
    async fn rejects_empty_path() {
        let tools = FsTools::new();
        assert!(
            tools
                .read_file(Parameters(ReadFileInput {
                    path: String::new(),
                    max_bytes: None
                }))
                .await
                .is_err()
        );
        assert!(
            tools
                .write_file(Parameters(WriteFileInput {
                    path: String::new(),
                    content: String::new(),
                    create_dirs: false
                }))
                .await
                .is_err()
        );
        assert!(
            tools
                .list_dir(Parameters(ListDirInput {
                    path: String::new(),
                    recursive: false,
                    max_entries: None,
                }))
                .await
                .is_err()
        );
        assert!(
            tools
                .glob(Parameters(GlobInput {
                    pattern: String::new(),
                    root: None,
                    max_matches: None,
                    respect_gitignore: None,
                }))
                .await
                .is_err()
        );
    }
}
