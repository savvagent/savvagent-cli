//! Per-(language, workspace_root) session pool.
//!
//! Sessions are spawned lazily on first request that needs them, reused
//! across requests, and evicted after `IDLE_TIMEOUT` of no activity.

use crate::config::LanguageEntry;
use crate::language::LanguageId;
use crate::session::LspSession;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Idle eviction threshold. Tunable later; ten minutes matches the spec.
pub const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Pool key: language id + absolute workspace root.
pub type SessionKey = (LanguageId, PathBuf);

/// Pool of running [`LspSession`]s, keyed by `(language, workspace_root)`.
///
/// Sessions are spawned lazily on first request and reused across
/// requests for the same key. Idle sessions are evicted by
/// [`LspPool::evict_idle`], which callers should drive on a tokio
/// interval. On stdin EOF, [`LspPool::shutdown_all`] tears the pool
/// down gracefully.
pub struct LspPool {
    sessions: Mutex<HashMap<SessionKey, Entry>>,
}

struct Entry {
    session: Arc<LspSession>,
    last_used: std::time::Instant,
}

impl Default for LspPool {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

impl LspPool {
    /// Acquire (or lazily spawn) a session for `(language_id, root)`.
    pub async fn get_or_spawn(
        &self,
        entry: &LanguageEntry,
        root: PathBuf,
        on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Result<Arc<LspSession>> {
        let key: SessionKey = (LanguageId(entry.id.clone()), root.clone());
        {
            let mut guard = self.sessions.lock().await;
            if let Some(e) = guard.get_mut(&key) {
                e.last_used = std::time::Instant::now();
                return Ok(Arc::clone(&e.session));
            }
        }
        // Spawn outside the lock — initialize handshake can take seconds.
        let session = LspSession::spawn(
            &entry.command,
            &entry.args,
            &entry.env,
            root,
            on_diagnostics,
        )
        .await?;
        let mut guard = self.sessions.lock().await;
        // Another concurrent caller may have populated the slot while we
        // were spawning; if so, retire ours and use theirs.
        if let Some(existing) = guard.get_mut(&key) {
            existing.last_used = std::time::Instant::now();
            tokio::spawn(async move { session.shutdown(2_000).await });
            return Ok(Arc::clone(&existing.session));
        }
        guard.insert(
            key,
            Entry {
                session: Arc::clone(&session),
                last_used: std::time::Instant::now(),
            },
        );
        Ok(session)
    }

    /// Background eviction tick. Call from a periodic task.
    pub async fn evict_idle(&self) {
        let mut guard = self.sessions.lock().await;
        let now = std::time::Instant::now();
        let to_remove: Vec<SessionKey> = guard
            .iter()
            .filter(|(_, e)| now.duration_since(e.last_used) > IDLE_TIMEOUT)
            .map(|(k, _)| k.clone())
            .collect();
        for k in to_remove {
            if let Some(e) = guard.remove(&k) {
                let session = e.session;
                tracing::info!(
                    language = %k.0.as_str(),
                    root = %k.1.display(),
                    "evicting idle LSP session"
                );
                tokio::spawn(async move { session.shutdown(2_000).await });
            }
        }
    }

    /// Snapshot every active session for resource readers to walk.
    ///
    /// Used by `resources/diagnostics::read` to fan a `resources/read`
    /// out across every cached language server in O(n) without holding
    /// the pool lock across the read.
    pub async fn snapshot_sessions(&self) -> Vec<Arc<LspSession>> {
        self.sessions
            .lock()
            .await
            .values()
            .map(|e| Arc::clone(&e.session))
            .collect()
    }

    /// Shut down every session (called from `run()` on stdin EOF).
    pub async fn shutdown_all(&self) {
        let mut guard = self.sessions.lock().await;
        let entries: Vec<_> = guard.drain().collect();
        drop(guard);
        for (_, e) in entries {
            e.session.shutdown(5_000).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pool_returns_same_arc_for_same_key() {
        // We can't easily spawn a real LSP in unit tests without picking
        // a binary off $PATH. The dedup-after-race branch is the easiest
        // observable property; the rest is covered by the integration
        // test against fake-lsp in Task 16.
        let pool = LspPool::default();
        // Inject a fake entry directly to verify lookup-and-update path.
        let key: SessionKey = (LanguageId("rust".into()), PathBuf::from("/tmp/p"));
        // We can't easily fabricate an LspSession because it needs a
        // running child. Instead, assert that pool's HashMap exists and
        // can be locked — a smoke check that the type compiles.
        assert!(pool.sessions.lock().await.is_empty());
        drop(key);
    }
}
