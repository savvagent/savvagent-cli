//! `InProcessToolHandler` — savvagent-internal trait for tools whose
//! implementation runs on the calling tokio runtime (no stdio child).
//!
//! Used by built-in plugins that need direct access to host state
//! (e.g. the `task` tool needs to construct a SubHost from the
//! parent's `Host`). The concrete context type is opaque here so this
//! crate does not depend on `savvagent-host`; handlers downcast the
//! `Arc<dyn Any>` to `savvagent_host::ToolCallContext`.

use async_trait::async_trait;
use serde_json::Value;
use std::any::Any;
use std::sync::Arc;

/// Trait implemented by in-process tool handlers (tools whose
/// implementation lives in the parent process rather than a stdio
/// child). The host stores [`InProcessToolHandlerArc`] in its
/// `ToolRegistry` alongside stdio-backed tools.
#[async_trait]
pub trait InProcessToolHandler: Send + Sync + 'static {
    /// Invoke the tool. `input` is the JSON argument object. `ctx` is
    /// an opaque host-owned value the host provides; handlers
    /// downcast to the concrete type they expect.
    async fn call(&self, input: Value, ctx: Arc<dyn Any + Send + Sync>) -> Result<Value, String>;
}

/// Newtype wrapper around `Arc<dyn InProcessToolHandler>` that supplies
/// the `PartialEq` + opaque `Debug` impls the [`crate::Effect`] enum
/// needs in order to keep its derived `PartialEq` and so its hand-rolled
/// `Debug` can format the variant without exposing the dyn target.
///
/// `PartialEq` is pointer-equality via [`Arc::ptr_eq`]; two clones of the
/// same handler compare equal, two independently-constructed instances
/// of the same concrete type do not. This matches the semantics callers
/// of [`crate::Effect`] need (they only ever check `==` to detect
/// "same handler re-emitted").
#[derive(Clone)]
pub struct InProcessToolHandlerArc(Arc<dyn InProcessToolHandler>);

impl InProcessToolHandlerArc {
    /// Construct from any concrete `InProcessToolHandler`.
    pub fn new<H: InProcessToolHandler>(handler: H) -> Self {
        Self(Arc::new(handler))
    }

    /// Borrow the underlying `Arc<dyn InProcessToolHandler>`.
    pub fn as_arc(&self) -> &Arc<dyn InProcessToolHandler> {
        &self.0
    }
}

impl std::fmt::Debug for InProcessToolHandlerArc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InProcessToolHandlerArc")
            .field("handler", &"<dyn InProcessToolHandler>")
            .finish()
    }
}

impl PartialEq for InProcessToolHandlerArc {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl From<Arc<dyn InProcessToolHandler>> for InProcessToolHandlerArc {
    fn from(arc: Arc<dyn InProcessToolHandler>) -> Self {
        Self(arc)
    }
}
