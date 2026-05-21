//! Bundled `savvagent-tool-lsp` binary. Delegates to [`tool_lsp::run`] so
//! the release archive ships LSP tooling alongside the TUI under one
//! installer.

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tool_lsp::run().await
}
