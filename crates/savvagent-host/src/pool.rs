//! Provider pool primitives. The pool stores per-provider entries keyed
//! by `ProviderId`. Each entry holds an `Arc<dyn ProviderClient>` plus
//! an active-turn counter that drives drain-mode disconnects.
//!
//! Locking discipline: the pool sits behind a single `tokio::sync::RwLock`
//! owned by `Host`. Read guards are released before any `.await` on the
//! provider client.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use savvagent_mcp::ProviderClient;
use savvagent_protocol::ProviderId;
use thiserror::Error;

use crate::capabilities::ProviderCapabilities;

/// Errors returned by pool operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PoolError {
    /// Returned when attempting to register a provider that is already in the pool.
    #[error("provider {0} is already registered")]
    AlreadyRegistered(ProviderId),
    /// Returned when referencing a provider that is not in the pool.
    #[error("provider {0} is not registered")]
    NotRegistered(ProviderId),
}

/// Controls how an active provider entry is removed from the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DisconnectMode {
    /// Wait for all outstanding [`ProviderLease`]s to drop before completing
    /// the disconnect. Implemented via `Arc` reference counting on the
    /// `active_turns` counter.
    Drain,
    /// Immediately cancel in-flight turns (3-stage: cooperative cancel →
    /// grace period → `JoinHandle::abort`). The grace duration is
    /// [`crate::config::HostConfig::force_disconnect_grace_ms`].
    /// Branching lives in [`Host::remove_provider`]; see that method for the
    /// 3-stage protocol.
    Force,
}

/// A live entry in the provider pool.
pub struct PoolEntry {
    client: Arc<dyn ProviderClient + Send + Sync>,
    capabilities: ProviderCapabilities,
    aliases: Vec<crate::capabilities::ModelAlias>,
    display_name: String,
    /// Number of [`ProviderLease`]s currently outstanding for this entry.
    /// Used by drain-disconnect to wait until in-flight turns finish.
    active_turns: Arc<AtomicUsize>,
}

impl PoolEntry {
    /// Construct a new entry with no active leases.
    pub fn new(
        client: Arc<dyn ProviderClient + Send + Sync>,
        capabilities: ProviderCapabilities,
        aliases: Vec<crate::capabilities::ModelAlias>,
        display_name: String,
    ) -> Self {
        Self {
            client,
            capabilities,
            aliases,
            display_name,
            active_turns: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The capability metadata advertised by this provider.
    pub fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    /// The model aliases advertised by this provider.
    pub fn aliases(&self) -> &[crate::capabilities::ModelAlias] {
        &self.aliases
    }

    /// Human-readable name for display in the UI.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Acquire a lease on this entry. The lease keeps the inner
    /// `Arc<dyn ProviderClient>` alive until dropped, even if the entry is
    /// removed from the pool in between.
    pub fn lease(&self) -> ProviderLease {
        self.active_turns.fetch_add(1, Ordering::SeqCst);
        ProviderLease {
            client: Arc::clone(&self.client),
            active_turns: Arc::clone(&self.active_turns),
        }
    }

    /// Number of leases currently outstanding for this entry.
    pub fn active_turn_count(&self) -> usize {
        self.active_turns.load(Ordering::SeqCst)
    }
}

/// RAII lease handle returned by [`PoolEntry::lease`].
///
/// Dropping a lease decrements the entry's active-turns counter, which is the
/// mechanism that lets drain-mode disconnect know when all in-flight turns
/// have finished.
#[must_use = "ProviderLease must be held for the duration of the turn"]
pub struct ProviderLease {
    client: Arc<dyn ProviderClient + Send + Sync>,
    active_turns: Arc<AtomicUsize>,
}

impl ProviderLease {
    /// The underlying provider client, usable for the duration of the lease.
    pub fn client(&self) -> &Arc<dyn ProviderClient + Send + Sync> {
        &self.client
    }
}

impl Drop for ProviderLease {
    fn drop(&mut self) {
        self.active_turns.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{CostTier, ModelCapabilities};
    use async_trait::async_trait;
    use savvagent_protocol::{
        CompleteRequest, CompleteResponse, ListModelsResponse, ProviderError, StreamEvent,
    };
    use tokio::sync::mpsc;

    struct StubClient;

    #[async_trait]
    impl ProviderClient for StubClient {
        async fn complete(
            &self,
            _: CompleteRequest,
            _: Option<mpsc::Sender<StreamEvent>>,
        ) -> Result<CompleteResponse, ProviderError> {
            unreachable!()
        }

        async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
            unreachable!()
        }
    }

    fn entry() -> PoolEntry {
        let caps = ProviderCapabilities::new(
            vec![ModelCapabilities {
                id: "m".into(),
                display_name: "M".into(),
                supports_vision: false,
                supports_audio: false,
                context_window: 1000,
                cost_tier: CostTier::Standard,
            }],
            "m".into(),
        )
        .expect("valid test caps");
        PoolEntry::new(Arc::new(StubClient), caps, Vec::new(), "Stub".into())
    }

    #[test]
    fn lease_increments_and_drop_decrements() {
        let e = entry();
        assert_eq!(e.active_turn_count(), 0);
        let l1 = e.lease();
        assert_eq!(e.active_turn_count(), 1);
        let l2 = e.lease();
        assert_eq!(e.active_turn_count(), 2);
        drop(l1);
        assert_eq!(e.active_turn_count(), 1);
        drop(l2);
        assert_eq!(e.active_turn_count(), 0);
    }

    #[test]
    fn lease_keeps_client_alive_after_entry_drop() {
        let e = entry();
        let lease = e.lease();
        let arc_weak = Arc::downgrade(lease.client());
        drop(e);
        // Lease still holds the Arc, so weak upgrade must succeed.
        assert!(arc_weak.upgrade().is_some());
        drop(lease);
        // Now both strong refs are gone.
        assert!(arc_weak.upgrade().is_none());
    }
}
