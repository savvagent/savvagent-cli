//! Bundled `savvagent-tool-web` binary. Delegates to [`tool_web::run`]
//! so the release archive ships the web-access tools alongside the TUI
//! under one installer.

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tool_web::run().await
}
