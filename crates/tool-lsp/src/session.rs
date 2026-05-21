//! One LSP child process plus a hand-rolled JSON-RPC client over its stdio.
//!
//! We don't pull in `async-lsp` or `tower-lsp-server`'s client adapter
//! because the framing is simple (Content-Length-prefixed UTF-8 JSON) and
//! we want full control over notification dispatch. ~150 lines is less
//! than the API surface we'd consume from either library.

use anyhow::{Context, Result, anyhow};
use lsp_types::{
    ClientCapabilities, InitializeParams, InitializeResult, PublishDiagnosticsParams, Uri,
    WorkspaceFolder, notification::Notification as LspNotification, request::Request as LspRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{Mutex, oneshot};

/// One running LSP child + its JSON-RPC client.
pub struct LspSession {
    /// Workspace root the child was initialized against.
    pub root: PathBuf,
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    next_id: std::sync::atomic::AtomicI64,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<serde_json::Value>>>>,
    /// Latest `publishDiagnostics` per URI. The notification handler
    /// task updates this; resource consumers read it.
    diagnostics: Arc<Mutex<HashMap<String, Vec<lsp_types::Diagnostic>>>>,
    /// Callback invoked on every `textDocument/publishDiagnostics`. Used
    /// by `resources/diagnostics.rs` to fire `notifications/resources/updated`
    /// upstream.
    #[allow(dead_code)]
    on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
    /// URIs we've already sent `textDocument/didOpen` for. Real LSP
    /// servers (rust-analyzer, tsserver, pyright, gopls) only return
    /// meaningful results for OPENED documents, so we lazily open every
    /// file before its first per-tool request and dedupe on subsequent
    /// hits to avoid spamming the server with redundant didOpens.
    open_files: Mutex<HashSet<String>>,
}

#[derive(Serialize, Deserialize)]
struct Request<P> {
    jsonrpc: &'static str,
    id: i64,
    method: &'static str,
    params: P,
}

#[derive(Serialize, Deserialize)]
struct Notification<P> {
    jsonrpc: &'static str,
    method: &'static str,
    params: P,
}

#[derive(Serialize, Deserialize, Debug)]
struct Response {
    #[allow(dead_code)]
    jsonrpc: String,
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<serde_json::Value>,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

impl LspSession {
    /// Spawn a child, perform `initialize` + `initialized`, return the
    /// ready session. `on_diagnostics` is called for every URI that
    /// receives a `publishDiagnostics` notification.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        root: PathBuf,
        on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Result<Arc<Self>> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args);
        cmd.env_clear();
        cmd.envs(std::env::vars()); // inherit user env first
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::null());
        // Without this, a panic/abort path that drops the `Child`
        // without calling `kill().await` would leak the child process.
        cmd.kill_on_drop(true);
        let mut child = cmd.spawn().with_context(|| format!("spawn {command}"))?;
        let stdin = child.stdin.take().context("child stdin missing")?;
        let stdout = child.stdout.take().context("child stdout missing")?;

        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<serde_json::Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let diagnostics: Arc<Mutex<HashMap<String, Vec<lsp_types::Diagnostic>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Spawn the reader task. It demultiplexes responses (by id) and
        // notifications (by method).
        let pending_clone = Arc::clone(&pending);
        let diagnostics_clone = Arc::clone(&diagnostics);
        let on_diag_clone: Arc<dyn Fn(&str) + Send + Sync> = Arc::clone(&on_diagnostics);
        tokio::spawn(async move {
            if let Err(e) = read_loop(stdout, pending_clone, diagnostics_clone, on_diag_clone).await
            {
                tracing::error!("LSP read loop ended: {e}");
            }
        });

        let session = Arc::new(LspSession {
            root: root.clone(),
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            next_id: std::sync::atomic::AtomicI64::new(1),
            pending,
            diagnostics,
            on_diagnostics,
            open_files: Mutex::new(HashSet::new()),
        });

        session.initialize(&root).await?;
        Ok(session)
    }

    async fn initialize(&self, root: &Path) -> Result<()> {
        let uri = path_to_uri(root)?;
        // `root_uri` is `#[deprecated]` in lsp-types but still required by
        // many servers (e.g. rust-analyzer) that haven't migrated to
        // `workspace_folders`. We populate both deliberately.
        #[allow(deprecated)]
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: uri.clone(),
                name: root
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("workspace")
                    .to_string(),
            }]),
            root_uri: Some(uri),
            capabilities: ClientCapabilities::default(),
            ..Default::default()
        };
        let _init_result: InitializeResult = self
            .request::<lsp_types::request::Initialize>(params)
            .await?;
        self.notify::<lsp_types::notification::Initialized>(lsp_types::InitializedParams {})
            .await?;
        Ok(())
    }

    /// Issue an LSP request and await the response, deserialized as `R::Result`.
    pub async fn request<R: LspRequest>(&self, params: R::Params) -> Result<R::Result>
    where
        R::Params: Serialize,
        R::Result: for<'de> Deserialize<'de>,
    {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let req = Request {
            jsonrpc: "2.0",
            id,
            method: R::METHOD,
            params,
        };
        let body = serde_json::to_vec(&req)?;

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        self.write_message(&body).await?;
        let value = rx
            .await
            .map_err(|_| anyhow!("response channel closed for id={id}"))?;
        if let Some(err) = value.get("lsp_error") {
            return Err(anyhow!("LSP error: {err}"));
        }
        Ok(serde_json::from_value::<R::Result>(value)?)
    }

    /// Send an LSP notification (no response expected).
    pub async fn notify<N: LspNotification>(&self, params: N::Params) -> Result<()>
    where
        N::Params: Serialize,
    {
        let n = Notification {
            jsonrpc: "2.0",
            method: N::METHOD,
            params,
        };
        let body = serde_json::to_vec(&n)?;
        self.write_message(&body).await
    }

    /// Ensure the file at `path` has been sent `textDocument/didOpen`.
    /// Reads the file content from disk on first open, picks the
    /// language id from the file extension, and tracks the URI so
    /// subsequent calls are a no-op.
    ///
    /// Real LSP servers (rust-analyzer, tsserver, pyright, gopls)
    /// return empty results for unopened documents; the fake-lsp
    /// fixture doesn't enforce that, so the gap was test-invisible
    /// until this hook landed.
    pub async fn ensure_did_open(&self, path: &Path) -> Result<()> {
        let uri = path_to_uri(path)?;
        let uri_str = uri.as_str().to_string();
        {
            let guard = self.open_files.lock().await;
            if guard.contains(&uri_str) {
                return Ok(());
            }
        }
        let text = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("read file for didOpen: {}", path.display()))?;
        let language_id = path
            .extension()
            .and_then(|os| os.to_str())
            .map(str::to_string)
            .unwrap_or_default();
        let params = lsp_types::DidOpenTextDocumentParams {
            text_document: lsp_types::TextDocumentItem {
                uri,
                language_id,
                version: 1,
                text,
            },
        };
        self.notify::<lsp_types::notification::DidOpenTextDocument>(params)
            .await?;
        self.open_files.lock().await.insert(uri_str);
        Ok(())
    }

    async fn write_message(&self, body: &[u8]) -> Result<()> {
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut guard = self.stdin.lock().await;
        guard.write_all(header.as_bytes()).await?;
        guard.write_all(body).await?;
        guard.flush().await?;
        Ok(())
    }

    /// Read-only access to the latest diagnostics for a URI.
    pub async fn diagnostics_for(&self, uri: &str) -> Vec<lsp_types::Diagnostic> {
        self.diagnostics
            .lock()
            .await
            .get(uri)
            .cloned()
            .unwrap_or_default()
    }

    /// Shut down gracefully: send `shutdown`, then `exit`, then wait
    /// up to `grace_ms` for the child to exit before killing it.
    pub async fn shutdown(&self, grace_ms: u64) {
        // Best-effort; we never propagate errors here because we're tearing down.
        let _ = self.request::<lsp_types::request::Shutdown>(()).await;
        let _ = self.notify::<lsp_types::notification::Exit>(()).await;
        let mut child = self.child.lock().await;
        let wait = tokio::time::timeout(std::time::Duration::from_millis(grace_ms), child.wait());
        if wait.await.is_err() {
            let _ = child.kill().await;
        }
    }
}

async fn read_loop(
    stdout: ChildStdout,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<serde_json::Value>>>>,
    diagnostics: Arc<Mutex<HashMap<String, Vec<lsp_types::Diagnostic>>>>,
    on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<()> {
    let exit = inner_read_loop(stdout, &pending, diagnostics, on_diagnostics).await;
    drain_pending(&pending, &exit).await;
    exit
}

async fn inner_read_loop(
    stdout: ChildStdout,
    pending: &Arc<Mutex<HashMap<i64, oneshot::Sender<serde_json::Value>>>>,
    diagnostics: Arc<Mutex<HashMap<String, Vec<lsp_types::Diagnostic>>>>,
    on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<()> {
    let mut reader = BufReader::new(stdout);
    loop {
        // Header lines, terminated by \r\n\r\n.
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                return Ok(());
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break; // end of headers
            }
            if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
                content_length = Some(rest.trim().parse()?);
            }
            // Ignore any other header (Content-Type, etc.).
        }
        let len = content_length.context("missing Content-Length header")?;
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).await?;
        let resp: Response = serde_json::from_slice(&buf)
            .with_context(|| format!("decoding LSP message of {len} bytes"))?;

        // Response (has an id and either result or error).
        if let Some(id) = resp.id {
            let mut pending = pending.lock().await;
            if let Some(tx) = pending.remove(&id) {
                if let Some(err) = resp.error {
                    let _ = tx.send(serde_json::json!({ "lsp_error": err }));
                } else {
                    let _ = tx.send(resp.result.unwrap_or(serde_json::Value::Null));
                }
            } else {
                tracing::warn!(id, "no pending request for response id");
            }
            continue;
        }

        // Notification.
        match resp.method.as_deref() {
            Some("textDocument/publishDiagnostics") => {
                let params: PublishDiagnosticsParams =
                    serde_json::from_value(resp.params.unwrap_or(serde_json::Value::Null))?;
                let uri_str = params.uri.as_str().to_string();
                diagnostics
                    .lock()
                    .await
                    .insert(uri_str.clone(), params.diagnostics);
                on_diagnostics(&uri_str);
            }
            Some(other) => {
                tracing::trace!(method = other, "LSP notification ignored");
            }
            None => {
                tracing::warn!(?resp, "LSP message had no id and no method");
            }
        }
    }
}

/// On read_loop exit (EOF or error), drain every still-pending request
/// waiter and fire a synthetic `lsp_error` envelope at it. `request<R>`
/// translates that envelope into `Err(anyhow!(...))`, so each hung
/// waiter becomes a clean error to the caller instead of hanging on
/// the oneshot receiver forever.
async fn drain_pending(
    pending: &Arc<Mutex<HashMap<i64, oneshot::Sender<serde_json::Value>>>>,
    exit_reason: &Result<()>,
) {
    let drained: Vec<_> = {
        let mut guard = pending.lock().await;
        guard.drain().collect()
    };
    if drained.is_empty() {
        return;
    }
    let reason_str = match exit_reason {
        Ok(()) => "LSP connection closed (EOF)".to_string(),
        Err(e) => format!("LSP connection error: {e}"),
    };
    for (_id, tx) in drained {
        let _ = tx.send(serde_json::json!({
            "lsp_error": { "message": reason_str.clone() }
        }));
    }
}

/// Convert a filesystem path to a `file://` URI.
pub fn path_to_uri(p: &std::path::Path) -> Result<Uri> {
    let url =
        url::Url::from_file_path(p).map_err(|_| anyhow!("path {} is not absolute", p.display()))?;
    Ok(url.as_str().parse()?)
}

/// Convert an LSP `file://` URI back to a filesystem path.
pub fn uri_to_path(uri: &Uri) -> Result<PathBuf> {
    let url: url::Url = uri.as_str().parse()?;
    url.to_file_path()
        .map_err(|_| anyhow!("URI {} is not a file:// URL", uri.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn path_to_uri_and_back_round_trips() {
        let p = std::env::current_dir().unwrap();
        let uri = path_to_uri(&p).unwrap();
        let back = uri_to_path(&uri).unwrap();
        // canonicalize both sides because URI normalization can resolve symlinks
        assert_eq!(back.canonicalize().unwrap(), p.canonicalize().unwrap());
    }

    #[test]
    fn path_to_uri_rejects_relative_path() {
        assert!(path_to_uri(Path::new("relative/file.rs")).is_err());
    }
}
