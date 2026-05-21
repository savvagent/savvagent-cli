//! Per-host cache of MCP resources advertised by connected tool servers.
//!
//! The cache stores **ownership + sequence**, not bodies. Resource bodies are
//! pulled on demand by the model via the `read_resource` synthetic tool;
//! caching them would force us to invalidate on every server update and gain
//! nothing — tools are local children, the read round-trip is cheap.
//!
//! The `dirty` set tracks URIs that received an `updated` notification
//! since the host last drained the set at the tool-use-loop boundary.
//! [`Host`] reads + clears the set inside the loop and uses it to inject
//! `[resource updated: <uri>]` user-text blocks.

use std::collections::{HashMap, HashSet};

/// One entry in the cache. The `seq` field is monotonically increasing
/// across all updates the host has observed for any URI — useful for
/// telemetry and for detecting "did anything change since I last looked."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceSnapshot {
    /// Tool server label (matches `ToolServer.label`) that owns this URI.
    pub owner: String,
    /// Monotonic sequence number; higher means more recent.
    pub seq: u64,
}

/// Cache of every resource any connected tool has notified us about.
#[derive(Debug, Default)]
pub struct ResourceCache {
    entries: HashMap<String, ResourceSnapshot>,
    dirty: HashSet<String>,
    next_seq: u64,
}

impl ResourceCache {
    /// Record an `updated` notification from `owner` for `uri`. Sets the
    /// URI as dirty until the next [`Self::drain_dirty`] call.
    pub fn mark_updated(&mut self, uri: impl Into<String>, owner: impl Into<String>) {
        let uri = uri.into();
        self.next_seq = self.next_seq.saturating_add(1);
        let snapshot = ResourceSnapshot {
            owner: owner.into(),
            seq: self.next_seq,
        };
        self.entries.insert(uri.clone(), snapshot);
        self.dirty.insert(uri);
    }

    /// Look up the owner of a URI. Returns `None` if no notification has
    /// ever arrived for it.
    pub fn owner(&self, uri: &str) -> Option<&str> {
        self.entries.get(uri).map(|s| s.owner.as_str())
    }

    /// Drain the dirty set, returning each URI in sorted order for
    /// stability — same set of URIs always produces the same drain
    /// sequence, so injected conversation blocks land in a deterministic
    /// order across hosts.
    pub fn drain_dirty(&mut self) -> Vec<String> {
        let mut out: Vec<String> = self.dirty.drain().collect();
        out.sort();
        out
    }

    /// Number of distinct URIs ever observed. Test/telemetry helper.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache has ever observed any URI. Paired with [`Self::len`]
    /// to satisfy clippy's `len_without_is_empty` lint under `-D warnings`.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_updated_records_owner_and_dirties_uri() {
        let mut cache = ResourceCache::default();
        cache.mark_updated("lsp://diagnostics/a.rs", "tool-lsp");
        assert_eq!(cache.owner("lsp://diagnostics/a.rs"), Some("tool-lsp"));
        assert_eq!(cache.drain_dirty(), vec!["lsp://diagnostics/a.rs"]);
    }

    #[test]
    fn drain_dirty_is_idempotent_after_first_call() {
        let mut cache = ResourceCache::default();
        cache.mark_updated("a", "t");
        let _ = cache.drain_dirty();
        assert!(
            cache.drain_dirty().is_empty(),
            "drain_dirty must clear the set; second call returns empty"
        );
    }

    #[test]
    fn second_update_for_same_uri_still_dirties() {
        let mut cache = ResourceCache::default();
        cache.mark_updated("a", "t");
        let _ = cache.drain_dirty();
        cache.mark_updated("a", "t");
        assert_eq!(cache.drain_dirty(), vec!["a"]);
    }

    #[test]
    fn drain_dirty_returns_sorted_uris_for_determinism() {
        // Insertion order is HashSet-defined and therefore arbitrary;
        // we sort so callers (the conversation-injection step) see a
        // stable order regardless of host platform / hash randomization.
        let mut cache = ResourceCache::default();
        cache.mark_updated("zzz", "t");
        cache.mark_updated("aaa", "t");
        cache.mark_updated("mmm", "t");
        assert_eq!(cache.drain_dirty(), vec!["aaa", "mmm", "zzz"]);
    }

    #[test]
    fn seq_is_monotonic_across_updates() {
        let mut cache = ResourceCache::default();
        cache.mark_updated("a", "t");
        cache.mark_updated("b", "t");
        let seq_a = cache.entries.get("a").unwrap().seq;
        let seq_b = cache.entries.get("b").unwrap().seq;
        assert!(seq_b > seq_a, "later updates must have higher seq");
    }

    #[test]
    fn owner_updates_when_a_different_tool_republishes() {
        // Reasonable: if tool-A publishes URI X and later tool-B publishes
        // the same URI, the latest owner wins. Multi-publisher cases are
        // unlikely in practice but we shouldn't silently keep the stale
        // owner.
        let mut cache = ResourceCache::default();
        cache.mark_updated("x", "tool-a");
        cache.mark_updated("x", "tool-b");
        assert_eq!(cache.owner("x"), Some("tool-b"));
    }
}
