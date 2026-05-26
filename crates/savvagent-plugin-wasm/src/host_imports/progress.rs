//! `progress-capability` host import for provider plugins.
//!
//! The WIT contract is one operation: `emit-stream-event(event)`. The
//! plugin calls it inside `complete` to surface streaming deltas while
//! the call is in flight; the host forwards the event to whatever
//! `mpsc::Sender<StreamEvent>` was attached for this turn.
//!
//! ## Wiring
//!
//! `WasmProviderClient::complete` is the only caller that constructs a
//! `ProgressState` with `active_emitter = Some(...)`. The other two
//! provider-world entry points (`list_models`, `count_tokens`) construct
//! `ProgressState` with `active_emitter = None`, so a misbehaving
//! plugin that tries to emit stream events from inside `list-models`
//! gets a silent drop instead of a wasm trap or a misdirected forward.
//!
//! ## Send/Sync
//!
//! `mpsc::Sender` is `Send + Clone`. We hold it directly (no extra
//! `Arc`) so cloning into a per-call `Store` is cheap.
//!
//! ## Backpressure
//!
//! If the receiver has gone away (host dropped the channel mid-turn),
//! `Sender::send` returns `Err(SendError)`. We swallow that error: the
//! plugin can't react to it meaningfully and the host's intent ("I'm
//! not listening anymore") is already expressed by the drop.

use tokio::sync::mpsc;

use crate::provider_world::savvagent::plugin::spp as wit;
use savvagent_protocol::StreamEvent;

/// Per-store progress state. Carries the optional channel forward to
/// the host's stream collector.
///
/// `Clone` is intentionally not derived: each per-call Store owns its
/// own `ProgressState`, and the `Sender` is cheap to clone manually
/// when constructing one.
pub struct ProgressState {
    /// Receiver for the in-flight turn's stream events. `None` when the
    /// containing call is `list_models` or `count_tokens` (no streaming
    /// surface) or when the caller deliberately passed `events: None`.
    pub active_emitter: Option<mpsc::Sender<StreamEvent>>,
}

impl ProgressState {
    /// Construct a state with no emitter — for non-streaming calls.
    pub fn disabled() -> Self {
        Self {
            active_emitter: None,
        }
    }

    /// Construct a state wired to the given sender.
    pub fn enabled(tx: mpsc::Sender<StreamEvent>) -> Self {
        Self {
            active_emitter: Some(tx),
        }
    }

    /// Forward one WIT stream-event to the active sender.
    ///
    /// Returns `()` for every input: a `None` emitter, a closed
    /// receiver, and a successful send all collapse to the same
    /// outcome from the wasm guest's perspective. WIT's
    /// `emit-stream-event` is itself a `-> ()` function for this
    /// reason.
    pub async fn emit(&self, event: wit::StreamEvent) {
        let Some(tx) = self.active_emitter.as_ref() else {
            return;
        };
        let spp_event: StreamEvent = event.into();
        // Fire-and-forget per WIT contract. `try_send` drops the event
        // if the channel is full (slow consumer) or closed (turn
        // cancelled / panicked) rather than blocking the wasm guest's
        // execution context. Plugin can't act on the failure either way.
        let _ = tx.try_send(spp_event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_protocol::StreamEvent;

    #[tokio::test]
    async fn disabled_drops_event_silently() {
        let s = ProgressState::disabled();
        // No sender, no panic: emit is a no-op.
        s.emit(wit::StreamEvent::MessageStop).await;
    }

    #[tokio::test]
    async fn enabled_forwards_event_to_receiver() {
        let (tx, mut rx) = mpsc::channel(4);
        let s = ProgressState::enabled(tx);
        s.emit(wit::StreamEvent::MessageStop).await;
        let received = rx.recv().await.expect("MessageStop forwarded");
        assert!(matches!(received, StreamEvent::MessageStop));
    }

    #[tokio::test]
    async fn dropped_receiver_does_not_panic() {
        let (tx, rx) = mpsc::channel(4);
        let s = ProgressState::enabled(tx);
        drop(rx);
        // No assertion: we're verifying this *doesn't* panic.
        s.emit(wit::StreamEvent::MessageStop).await;
    }
}
