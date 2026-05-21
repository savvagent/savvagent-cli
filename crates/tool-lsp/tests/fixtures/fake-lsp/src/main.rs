//! Minimal LSP server for tool-lsp integration tests.
//!
//! Speaks JSON-RPC over stdio with Content-Length framing. Implements:
//! - initialize / initialized (handshake)
//! - textDocument/definition (returns a canned Location at line 0, char 0)
//! - textDocument/references (returns one Location for the queried URI)
//! - publishes one publishDiagnostics on initialize completion
//! - shutdown / exit

use anyhow::Result;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut initialized_root: Option<String> = None;

    loop {
        // Header parse.
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                return Ok(());
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
                content_length = Some(rest.trim().parse()?);
            }
        }
        let len = content_length.ok_or_else(|| anyhow::anyhow!("no Content-Length"))?;
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).await?;
        let msg: Value = serde_json::from_slice(&buf)?;

        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        match method.as_deref() {
            Some("initialize") => {
                if let Some(root_uri) = params.get("rootUri").and_then(Value::as_str) {
                    initialized_root = Some(root_uri.to_string());
                }
                let body = serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "capabilities": {
                            "textDocumentSync": 1,
                            "definitionProvider": true,
                            "referencesProvider": true,
                            "hoverProvider": true,
                            "documentSymbolProvider": true,
                            "workspaceSymbolProvider": true,
                            "renameProvider": true,
                            "codeActionProvider": true
                        }
                    }
                }))?;
                write_message(&mut stdout, &body).await?;
            }
            Some("initialized") => {
                // Fire one publishDiagnostics so tool-lsp tests can verify
                // the notification path round-trips into MCP resources.
                if let Some(root) = initialized_root.clone() {
                    let body = serde_json::to_vec(&json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/publishDiagnostics",
                        "params": {
                            "uri": format!("{root}/src/lib.rs"),
                            "diagnostics": [{
                                "range": {"start":{"line":1,"character":0},"end":{"line":1,"character":1}},
                                "severity": 1,
                                "source": "fake-lsp",
                                "code": "FAKE001",
                                "message": "diagnostic emitted by fixture"
                            }]
                        }
                    }))?;
                    write_message(&mut stdout, &body).await?;
                }
            }
            Some("textDocument/definition") => {
                let uri = params
                    .get("textDocument")
                    .and_then(|t| t.get("uri"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let body = serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "uri": uri,
                        "range": {"start":{"line":0,"character":0},"end":{"line":0,"character":3}}
                    }
                }))?;
                write_message(&mut stdout, &body).await?;
            }
            Some("textDocument/references") => {
                let uri = params
                    .get("textDocument")
                    .and_then(|t| t.get("uri"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let body = serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": [{
                        "uri": uri,
                        "range": {"start":{"line":2,"character":0},"end":{"line":2,"character":3}}
                    }]
                }))?;
                write_message(&mut stdout, &body).await?;
            }
            Some("shutdown") => {
                let body = serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": null
                }))?;
                write_message(&mut stdout, &body).await?;
            }
            Some("exit") => return Ok(()),
            _ => {
                // Reply with method-not-found for any request we didn't model.
                if id.is_some() {
                    let body = serde_json::to_vec(&json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32601, "message": "method not found"}
                    }))?;
                    write_message(&mut stdout, &body).await?;
                }
            }
        }
    }
}

async fn write_message<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, body: &[u8]) -> Result<()> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    w.write_all(header.as_bytes()).await?;
    w.write_all(body).await?;
    w.flush().await?;
    Ok(())
}
