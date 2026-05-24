//! `ScopedToolRegistry` — wraps `Arc<ToolRegistry>` and rejects calls
//! whose tool name is not in a per-subagent allowlist. Used by `SubHost`
//! to enforce the `tools:` frontmatter scoping at runtime, defending
//! against a model that fabricates a tool name from training data.

use crate::tools::ToolRegistry;
use std::collections::HashSet;
use std::sync::Arc;

/// Per-subagent view over a shared [`ToolRegistry`] that only permits
/// dispatch for tool names appearing in `allowed`. Actual dispatch goes
/// through the wrapped registry via [`Self::inner`]; this type's only job
/// is name filtering.
#[derive(Clone)]
pub struct ScopedToolRegistry {
    inner: Arc<ToolRegistry>,
    allowed: Arc<HashSet<String>>,
}

impl ScopedToolRegistry {
    /// Build a scoped view over `inner` that admits exactly the names in
    /// `allowed`. Names should be the fully-qualified `server:tool` form
    /// that the model sees in the provider's tool list.
    ///
    /// Currently unused outside tests; consumed by SubHost in Task 7.
    #[allow(dead_code)]
    pub(crate) fn new(inner: Arc<ToolRegistry>, allowed: HashSet<String>) -> Self {
        Self {
            inner,
            allowed: Arc::new(allowed),
        }
    }

    /// Returns `true` iff `name` is in the per-subagent allowlist.
    ///
    /// Currently unused outside tests; consumed by SubHost in Task 7.
    #[allow(dead_code)]
    pub(crate) fn allows(&self, name: &str) -> bool {
        self.allowed.contains(name)
    }

    /// Access the wrapped registry for actual dispatch. Callers must
    /// gate calls on [`Self::allows`] first.
    ///
    /// Currently unused outside tests; consumed by SubHost in Task 7.
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> &Arc<ToolRegistry> {
        &self.inner
    }

    /// Read-only access to the allowlist, e.g. for filtering the
    /// provider-facing tool list before a turn.
    ///
    /// Currently unused outside tests; consumed by SubHost in Task 7.
    #[allow(dead_code)]
    pub(crate) fn allowed(&self) -> &HashSet<String> {
        &self.allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_known_name() {
        let inner = ToolRegistry::empty_for_test();
        let mut allowed = HashSet::new();
        allowed.insert("tool-fs:read_file".to_string());
        let scoped = ScopedToolRegistry::new(inner, allowed);
        assert!(scoped.allows("tool-fs:read_file"));
        assert!(!scoped.allows("tool-fs:write_file"));
    }
}
