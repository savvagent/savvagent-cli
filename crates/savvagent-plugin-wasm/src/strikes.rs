//! Rolling 10-minute trap counter. Three strikes within the window =
//! disable the offending plugin.
//!
//! ## Why a rolling window
//!
//! A plugin that traps once a month is buggy but not malicious; auto-
//! disabling on a single trap punishes users for transient faults
//! (out-of-memory at process startup, a dependency's edge case, …).
//! Conversely, three traps inside ten minutes is a stuck failure mode —
//! the plugin is either crashing on every input or in an unrecoverable
//! state, and continuing to call it just amplifies the host-side noise.
//!
//! The window is intentionally process-local. We do NOT persist strikes
//! across restarts: that would require a host-managed counter file, which
//! complicates the trust ledger without buying much (a restart clears the
//! transient bad state more often than not). Persistent disablement
//! happens in [`crate::trust`] via `disabled_reason`, written by the
//! `internal:plugins-manager` plugin once it observes the disable signal
//! from the registry (auto-disable signal wiring is a v0.18.1 follow-up).
//!
//! ## Semantics
//!
//! - `record()` is called once per wasm trap. It prunes entries older
//!   than [`WINDOW`] from the front of the queue, pushes the current
//!   instant onto the back, and reports the new length.
//! - Three or more entries inside the window returns
//!   [`StrikeOutcome::Disable`]; fewer returns
//!   [`StrikeOutcome::Continue`] with the current count.
//! - Successful calls do NOT reset the counter — the window is rolling
//!   time-based, not "consecutive". A plugin that traps twice, succeeds
//!   once, then traps again still has three strikes inside the window.
//!
//! ## Thread safety
//!
//! `Mutex<VecDeque<Instant>>` is sufficient: the critical section is
//! `prune + push + len`, all cheap, with no `.await` inside. Contention
//! is bounded by the rate of wasm traps on a single plugin (in practice:
//! near zero).

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Rolling window over which strikes count toward auto-disable.
pub const WINDOW: Duration = Duration::from_secs(600);

/// Strike count that triggers auto-disable.
pub const LIMIT: usize = 3;

/// In-memory three-strikes counter for one plugin instance.
///
/// One counter per adapter; each adapter constructs its own at load
/// time. Cloning is via `Arc` at the call site (the counter itself is
/// not `Clone`).
#[derive(Default, Debug)]
pub struct StrikeCounter {
    inner: Mutex<VecDeque<Instant>>,
}

impl StrikeCounter {
    /// Record a strike at `Instant::now()` and report whether the plugin
    /// should be disabled.
    ///
    /// Prunes entries older than [`WINDOW`] before deciding, so a slow
    /// drip of traps spaced more than ten minutes apart never disables.
    pub fn record(&self) -> StrikeOutcome {
        self.record_at(Instant::now())
    }

    /// Test-only variant that lets unit tests inject a clock value. We
    /// keep the entry point `pub(crate)` so production paths always go
    /// through [`StrikeCounter::record`] (and the wall-clock).
    pub(crate) fn record_at(&self, now: Instant) -> StrikeOutcome {
        let mut q = self.inner.lock().expect("strike mutex poisoned");
        // Drop expired entries — `front()` is the oldest. `now -
        // *t > WINDOW` is equivalent to "outside the rolling window";
        // strict `>` so a tick that lands exactly on the boundary still
        // counts (closed-interval lower bound).
        while q.front().is_some_and(|t| now.duration_since(*t) > WINDOW) {
            q.pop_front();
        }
        q.push_back(now);
        if q.len() >= LIMIT {
            StrikeOutcome::Disable
        } else {
            StrikeOutcome::Continue {
                count: q.len(),
                window: WINDOW,
            }
        }
    }

    /// Forget every recorded strike. Currently unused by the adapters —
    /// kept available for callers (e.g. `/plugins reset-strikes` in a
    /// later release) that want to clear the counter explicitly.
    pub fn reset(&self) {
        self.inner.lock().expect("strike mutex poisoned").clear();
    }
}

/// Outcome of recording one strike.
#[derive(Debug, PartialEq, Eq)]
pub enum StrikeOutcome {
    /// Below the limit. `count` is the post-record length; `window` is
    /// the rolling window so callers can log "2/3 in the last 600s".
    Continue {
        /// Number of strikes inside the rolling window after this one.
        count: usize,
        /// Rolling-window duration the count is computed over.
        window: Duration,
    },
    /// At or above the limit. The adapter should mark itself disabled
    /// and short-circuit every subsequent call.
    Disable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_strikes_disable() {
        let c = StrikeCounter::default();
        assert!(matches!(
            c.record(),
            StrikeOutcome::Continue { count: 1, .. }
        ));
        assert!(matches!(
            c.record(),
            StrikeOutcome::Continue { count: 2, .. }
        ));
        assert_eq!(c.record(), StrikeOutcome::Disable);
    }

    #[test]
    fn reset_clears() {
        let c = StrikeCounter::default();
        c.record();
        c.record();
        c.reset();
        assert!(matches!(
            c.record(),
            StrikeOutcome::Continue { count: 1, .. }
        ));
    }

    #[test]
    fn outside_window_does_not_count() {
        // Two strikes far enough apart that the older one falls outside
        // the rolling window by the time the third one lands. The third
        // record should leave us with a count of TWO (the recent one +
        // the new push), not Disable. Pruning is the load-bearing
        // piece — without it a long-running plugin would accumulate
        // strikes forever.
        let c = StrikeCounter::default();
        let t0 = Instant::now();
        assert!(matches!(
            c.record_at(t0),
            StrikeOutcome::Continue { count: 1, .. }
        ));
        // 500s after t0: still inside window relative to t0 (500 < 600),
        // so post-record count is 2.
        assert!(matches!(
            c.record_at(t0 + Duration::from_secs(500)),
            StrikeOutcome::Continue { count: 2, .. }
        ));
        // 700s after t0: t0 entry is pruned (700 > 600); t0+500 is
        // still inside (700-500 = 200 < 600); push the new entry.
        // Post-record count is 2 (t0+500 + the new one), NOT Disable.
        assert!(matches!(
            c.record_at(t0 + Duration::from_secs(700)),
            StrikeOutcome::Continue { count: 2, .. }
        ));
    }

    #[test]
    fn pruning_then_disable_works() {
        // Three strikes that all land inside the rolling window after
        // pruning still triggers Disable. Ensures the prune-then-push
        // order doesn't double-count or skip the limit check after
        // eviction.
        let c = StrikeCounter::default();
        let t0 = Instant::now();
        c.record_at(t0); // pruned at t0+650
        c.record_at(t0 + Duration::from_secs(200));
        c.record_at(t0 + Duration::from_secs(400));
        // At t0+650: t0 is pruned (650 > 600); t0+200 stays (450 <
        // 600); t0+400 stays (250 < 600); push the new entry.
        // Three inside the window → Disable.
        assert_eq!(
            c.record_at(t0 + Duration::from_secs(650)),
            StrikeOutcome::Disable
        );
    }
}
