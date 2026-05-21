//! LSP-server bridge as a stdio MCP server.
//!
//! Wraps user-configured LSP servers (rust-analyzer, typescript-language-server,
//! pyright, gopls, …) behind a small MCP tool surface and publishes diagnostics
//! as MCP resources (`lsp://diagnostics/<absolute-path>`).
//!
//! Language servers are configured in `~/.savvagent/lsp.toml` (global) and
//! optionally overridden per repo at `<repo>/.savvagent/lsp.toml`. No
//! languages are hardcoded; see the README for example entries.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod config;
pub use config::{LanguageEntry, LspConfig};

mod language;
pub use language::{LanguageId, extension_of, workspace_root_for};

mod session;
pub use session::LspSession;

mod pool;
pub use pool::{IDLE_TIMEOUT, LspPool};

mod convert;
pub use convert::{DiagnosticOut, FileEditOut, LocationOut, PositionOut, RangeOut, TextEditOut};

mod resources;
pub mod tools;

use std::sync::Arc;

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::{
        router::tool::ToolRouter,
        wrapper::{Json, Parameters},
    },
    model::{
        Implementation, InitializeRequestParams, InitializeResult, ProtocolVersion,
        ReadResourceRequestParams, ReadResourceResult, ResourceUpdatedNotificationParam,
        ServerCapabilities, ServerInfo,
    },
    service::{Peer, RequestContext},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use tokio::sync::OnceCell;

/// Entrypoint used by the `savvagent-tool-lsp` shim binary. Reads the
/// configured `lsp.toml` files, starts an rmcp stdio server, and serves
/// until stdin closes. While the server runs we spin a background task
/// that calls [`LspPool::evict_idle`] every `IDLE_TIMEOUT / 2`; on EOF
/// we drive [`LspPool::shutdown_all`] so every active LSP child is
/// shut down gracefully instead of being orphaned.
pub async fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let server = LspServer::new()?;
    // Capture pool handle BEFORE `server.serve(...)` consumes `server`.
    let pool_for_eviction = Arc::clone(&server.pool);
    let pool_for_shutdown = Arc::clone(&server.pool);
    let service = server.serve(stdio()).await?;

    // Fire-and-forget eviction loop. The handle is dropped at function
    // return, which cancels the task; that's the intended shutdown.
    let _eviction_task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(pool::IDLE_TIMEOUT / 2);
        // First tick fires immediately; skip it so we don't churn the
        // pool right after construction (sessions are spawned lazily,
        // so it would be a no-op, but the log noise is wasteful).
        tick.tick().await;
        loop {
            tick.tick().await;
            pool_for_eviction.evict_idle().await;
        }
    });

    let result = service.waiting().await;
    pool_for_shutdown.shutdown_all().await;
    result?;
    Ok(())
}

/// rmcp `ServerHandler` for tool-lsp. Owns the shared configuration,
/// session pool, root, diagnostics callback, and the macro-generated
/// tool router that dispatches to per-tool modules in `tools/`.
pub struct LspServer {
    #[allow(dead_code)] // Read by tool dispatch handlers.
    config: Arc<config::LspConfig>,
    #[allow(dead_code)] // Read by tool dispatch handlers.
    pool: Arc<pool::LspPool>,
    #[allow(dead_code)] // Read by tool dispatch handlers.
    root: Arc<std::path::PathBuf>,
    /// Callback that fires after every publishDiagnostics arrives.
    /// Forwards a `notifications/resources/updated` upstream once the
    /// rmcp peer is captured via the `initialize` handshake.
    on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
    /// rmcp peer handle captured from the first `initialize` request.
    /// Used by `on_diagnostics` to fire `notify_resource_updated`. The
    /// `OnceCell` is required because the peer doesn't exist at
    /// construction time — it's bound in by the service loop after the
    /// MCP handshake.
    peer: Arc<OnceCell<Peer<RoleServer>>>,
    #[allow(dead_code)] // Read by the `#[tool_handler]` macro expansion.
    tool_router: ToolRouter<Self>,
}

impl LspServer {
    /// Construct a new server: loads global + per-repo `lsp.toml`,
    /// pins the SAVVAGENT_TOOL_LSP_ROOT (defaulting to the process CWD),
    /// and initializes an empty session pool.
    pub fn new() -> anyhow::Result<Self> {
        let home = std::env::var("HOME").map(std::path::PathBuf::from).ok();
        let global = home
            .map(|h| h.join(".savvagent/lsp.toml"))
            .unwrap_or_else(|| std::path::PathBuf::from("/dev/null"));
        let cwd = std::env::current_dir()?;
        let repo = cwd.join(".savvagent/lsp.toml");
        let config = config::LspConfig::load(&global, Some(&repo))?;
        let root = std::env::var("SAVVAGENT_TOOL_LSP_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or(cwd);
        let peer: Arc<OnceCell<Peer<RoleServer>>> = Arc::new(OnceCell::new());
        let on_diagnostics = Self::make_on_diagnostics(Arc::clone(&peer));
        Ok(Self {
            config: Arc::new(config),
            pool: Arc::new(pool::LspPool::default()),
            root: Arc::new(root),
            on_diagnostics,
            peer,
            tool_router: Self::tool_router(),
        })
    }

    /// Build the per-session `on_diagnostics` callback. On every
    /// `publishDiagnostics` arriving from any child LSP we (a) compute
    /// the matching `lsp://diagnostics/<path>` URI, then (b) fire
    /// `notifications/resources/updated` upstream — but ONLY if the
    /// rmcp peer has been captured. Pre-handshake fires are silently
    /// dropped (the host will pick up the diagnostics on the next
    /// `resources/read`).
    fn make_on_diagnostics(
        peer: Arc<OnceCell<Peer<RoleServer>>>,
    ) -> Arc<dyn Fn(&str) + Send + Sync> {
        Arc::new(move |file_uri: &str| {
            let uri = resources::diagnostics::diagnostics_uri_for(file_uri);
            let peer = Arc::clone(&peer);
            tokio::spawn(async move {
                if let Some(p) = peer.get() {
                    let _ = p
                        .notify_resource_updated(ResourceUpdatedNotificationParam { uri })
                        .await;
                }
            });
        })
    }
}

#[tool_router]
impl LspServer {
    /// Jump to the definition of the symbol at the given position.
    #[tool(description = "Jump to the definition of the symbol at the given position.")]
    pub async fn lsp_definition(
        &self,
        Parameters(input): Parameters<tools::definition::LspDefinitionInput>,
    ) -> Result<Json<tools::definition::LspDefinitionOutput>, ErrorData> {
        tools::definition::dispatch(
            input,
            &self.config,
            &self.pool,
            &self.root,
            Arc::clone(&self.on_diagnostics),
        )
        .await
        .map(Json)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }

    /// Find all references to the symbol at the given position.
    #[tool(description = "Find all references to the symbol at the given position.")]
    pub async fn lsp_references(
        &self,
        Parameters(input): Parameters<tools::references::LspReferencesInput>,
    ) -> Result<Json<tools::references::LspReferencesOutput>, ErrorData> {
        tools::references::dispatch(
            input,
            &self.config,
            &self.pool,
            &self.root,
            Arc::clone(&self.on_diagnostics),
        )
        .await
        .map(Json)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }

    /// Get hover information for the symbol at the given position.
    #[tool(description = "Get hover information for the symbol at the given position.")]
    pub async fn lsp_hover(
        &self,
        Parameters(input): Parameters<tools::hover::LspHoverInput>,
    ) -> Result<Json<tools::hover::LspHoverOutput>, ErrorData> {
        tools::hover::dispatch(
            input,
            &self.config,
            &self.pool,
            &self.root,
            Arc::clone(&self.on_diagnostics),
        )
        .await
        .map(Json)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }

    /// List the symbols defined in a single document, with nesting.
    #[tool(description = "List the symbols defined in a single document, with nesting.")]
    pub async fn lsp_document_symbols(
        &self,
        Parameters(input): Parameters<tools::document_symbols::LspDocumentSymbolsInput>,
    ) -> Result<Json<tools::document_symbols::LspDocumentSymbolsOutput>, ErrorData> {
        tools::document_symbols::dispatch(
            input,
            &self.config,
            &self.pool,
            &self.root,
            Arc::clone(&self.on_diagnostics),
        )
        .await
        .map(Json)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }

    /// Search for symbols across the workspace by query string.
    #[tool(description = "Search for symbols across the workspace by query string.")]
    pub async fn lsp_workspace_symbols(
        &self,
        Parameters(input): Parameters<tools::workspace_symbols::LspWorkspaceSymbolsInput>,
    ) -> Result<Json<tools::workspace_symbols::LspWorkspaceSymbolsOutput>, ErrorData> {
        tools::workspace_symbols::dispatch(
            input,
            &self.config,
            &self.pool,
            &self.root,
            Arc::clone(&self.on_diagnostics),
        )
        .await
        .map(Json)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }

    /// Compute the edits needed to rename a symbol. Does NOT apply them —
    /// the caller drives the resulting `FileEditOut`s through
    /// `tool-fs::write_file`.
    #[tool(
        description = "Compute the edits required to rename the symbol at the given position. Does NOT apply the edits — the caller must do that via tool-fs::write_file."
    )]
    pub async fn lsp_rename(
        &self,
        Parameters(input): Parameters<tools::rename::LspRenameInput>,
    ) -> Result<Json<tools::rename::LspRenameOutput>, ErrorData> {
        tools::rename::dispatch(
            input,
            &self.config,
            &self.pool,
            &self.root,
            Arc::clone(&self.on_diagnostics),
        )
        .await
        .map(Json)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }

    /// List code actions available for a range. Actions return edit
    /// descriptors; the caller applies them via `tool-fs::write_file`.
    #[tool(
        description = "List code actions (quickfixes, refactors) available for a range. Actions return edit descriptors; the caller applies them via tool-fs::write_file."
    )]
    pub async fn lsp_code_actions(
        &self,
        Parameters(input): Parameters<tools::code_actions::LspCodeActionsInput>,
    ) -> Result<Json<tools::code_actions::LspCodeActionsOutput>, ErrorData> {
        tools::code_actions::dispatch(
            input,
            &self.config,
            &self.pool,
            &self.root,
            Arc::clone(&self.on_diagnostics),
        )
        .await
        .map(Json)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }
}

#[tool_handler]
impl ServerHandler for LspServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::default())
        .with_server_info(Implementation::new(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        ))
    }

    /// Capture the rmcp peer on the post-handshake `initialize` request
    /// so subsequent `publishDiagnostics` callbacks can fire
    /// `notifications/resources/updated`. We mirror the default
    /// implementation's `set_peer_info` call so client info is still
    /// available to `peer.peer_info()`.
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        if context.peer.peer_info().is_none() {
            context.peer.set_peer_info(request);
        }
        // First handshake wins; later attempts (which shouldn't happen
        // on a stdio server) are ignored. We deliberately ignore the
        // `Err` from `set` — it just means the cell was already filled.
        let _ = self.peer.set(context.peer.clone());
        Ok(self.get_info())
    }

    /// Serve `resources/read` for `lsp://diagnostics/*` URIs. Anything
    /// else falls through to the trait default (which yields
    /// `MethodNotFound`); we don't currently publish other resources.
    async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        resources::diagnostics::read(&params.uri, &self.pool).await
    }
}
