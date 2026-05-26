//! Process-wide shared [`wasmtime::Engine`].
//!
//! All three adapters (static, interactive, provider) share one engine so
//! components compile once and the configuration is consistent across worlds.
//! `Engine` is `Clone` (it holds an internal `Arc`), so handing it out by
//! value is cheap.
//!
//! Configuration:
//! - **Component model**: required — every world we ship is a component.
//! - **Async support**: required — all generated exports return
//!   `impl Future`, and adapters await them inside `tokio::task` workers.
//! - **Epoch interruption**: enabled (Task 8). Every store starts with a
//!   sentinel deadline of `u64::MAX` so untouched stores never trap; each
//!   adapter then re-sets the deadline per-call from the manifest's
//!   `runtime.call_timeout_ms`. A dedicated background thread increments
//!   the engine's epoch counter every [`EPOCH_TICK`] so deadlines that
//!   expire produce a `Trap::Interrupt`.
//!
//! The engine is initialized lazily via [`OnceLock`]; the first call to
//! [`shared_engine`] pays the construction cost and every subsequent call
//! is a single relaxed load.
//!
//! ## Why `std::thread`, not `tokio::spawn`
//!
//! `OnceLock::get_or_init` is sync, and the very first caller may be
//! anywhere — a binary's `main`, a `#[tokio::test]`, even a sync unit
//! test. `tokio::spawn` requires a runtime context that won't necessarily
//! exist there. `std::thread::spawn` + `std::thread::sleep` works
//! unconditionally and adds no runtime dependency. The thread holds a
//! clone of the engine (`Engine` is `Arc`-backed) and lives for the
//! process lifetime — no graceful shutdown.
//!
//! Initialization failures are surfaced through
//! [`crate::error::WasmPluginError::Wasmtime`] rather than panicking, so a
//! misconfigured embedder doesn't take the host down. In practice the
//! features we enable are all stable in wasmtime 34, so the failure path is
//! defensive.

use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use wasmtime::{Config, Engine};

use crate::error::WasmPluginError;

/// Period of the background epoch bumper. Per-call deadlines are set in
/// units of this tick: `set_epoch_deadline(N)` traps after `N * EPOCH_TICK`
/// of wall-clock wasm execution time at most.
///
/// Default per-plugin `call_timeout_ms = 5000` therefore corresponds to
/// `5000 / 100 = 50` ticks. The granularity (100ms) is a deliberate
/// trade-off: finer ticks cost more CPU on the bumper thread but bound the
/// over-budget execution time more tightly. 100ms keeps overhead at ~10
/// `increment_epoch` calls/second per process (negligible) while still
/// trapping runaway plugins inside the user's perceptual budget.
pub const EPOCH_TICK: Duration = Duration::from_millis(100);

static ENGINE: OnceLock<Engine> = OnceLock::new();

/// Returns the process-wide shared [`Engine`], initializing it on first
/// call. Cheap to clone (internal `Arc`).
pub fn shared_engine() -> Result<Engine, WasmPluginError> {
    if let Some(e) = ENGINE.get() {
        return Ok(e.clone());
    }
    let mut cfg = Config::new();
    cfg.async_support(true);
    cfg.wasm_component_model(true);
    // Enable epoch-based interruption. Without a corresponding per-call
    // `Store::set_epoch_deadline`, the default deadline is `0` and every
    // store traps on the first instruction — which is why this flag was
    // gated on Task 8 landing the deadline-management code. Each adapter
    // now sets a per-call deadline from `runtime.call_timeout_ms` before
    // it issues the wasm call.
    cfg.epoch_interruption(true);
    let engine = Engine::new(&cfg).map_err(WasmPluginError::Wasmtime)?;
    // `get_or_init` is the right primitive but it requires an infallible
    // closure; we already built the engine above so use the racy path:
    // first writer wins, others drop their copy and use the stored one.
    let stored = ENGINE.get_or_init(|| engine).clone();
    // Start the epoch bumper exactly once. `get_or_init` ensures only one
    // thread observes the "we just installed the engine" transition; we
    // detect it via a second `OnceLock` so a second concurrent caller
    // doesn't spawn a duplicate bumper.
    start_epoch_bumper_once(stored.clone());
    Ok(stored)
}

/// Spawn the epoch-bump thread the first time it's needed. Subsequent
/// callers observe the `OnceLock` is populated and no-op.
fn start_epoch_bumper_once(engine: Engine) {
    static BUMPER_STARTED: OnceLock<()> = OnceLock::new();
    BUMPER_STARTED.get_or_init(|| {
        // Hand the thread a clone of the engine. `Engine` is internally
        // `Arc`-backed, so the bumper and every adapter call see the
        // same counter; `increment_epoch()` on any clone advances the
        // shared epoch.
        let engine_for_thread = engine;
        thread::Builder::new()
            .name("savvagent-wasm-epoch".to_string())
            .spawn(move || {
                loop {
                    thread::sleep(EPOCH_TICK);
                    engine_for_thread.increment_epoch();
                }
            })
            .expect("spawn epoch bumper thread");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_engine_is_idempotent() {
        let a = shared_engine().expect("engine init");
        let b = shared_engine().expect("engine init");
        // Engine doesn't expose pointer-equality directly; the smoke test
        // is that two calls succeed and either is usable.
        let _ = a;
        let _ = b;
    }

    #[test]
    fn engine_supports_component_model() {
        let engine = shared_engine().expect("engine init");
        // Empty component bytes wouldn't parse; instead just confirm the
        // engine is callable. The presence of `wasm_component_model(true)`
        // is verified via downstream tests that actually instantiate
        // components (see tests/static_adapter.rs).
        let _ = engine;
    }

    #[test]
    fn epoch_tick_is_nonzero() {
        // `EPOCH_TICK` feeds the per-call deadline calculation in every
        // adapter (`call_timeout_ms / tick_ms`). A zero tick would
        // cause a divide-by-zero — the `max(1)` guards against it but
        // we still assert the constant is sane.
        assert!(EPOCH_TICK.as_millis() > 0);
    }

    // We intentionally don't assert that `engine.current_epoch()` advances:
    // the method is `pub(crate)` in wasmtime 34 and therefore not
    // observable from a downstream crate. End-to-end verification that
    // the bumper is alive happens in
    // `tests/fault_injection.rs::timeout_can_be_cancelled_by_host`,
    // which depends on the bumper trapping a busy-loop wasm guest
    // within the per-call deadline.
}
