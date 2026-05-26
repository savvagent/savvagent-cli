//! Host-side implementations of the capabilities every WIT world imports.
//!
//! Each capability lives in its own submodule and is consumed by the
//! corresponding adapter in `crate::adapter` via the bindgen-emitted
//! `add_to_linker` trait. The host-import surface in v0.18.0:
//!
//! - [`log`] — `log(level, msg)`: structured-logging passthrough used by
//!   all three worlds.
//! - [`theme`] — `current-theme()`: snapshot of the active palette's
//!   (name, color) tuples; used by static and interactive worlds.
//! - [`http`] — `http-capability.fetch`: buffered HTTPS request with
//!   manifest-driven allow-list. Provider world only (Task 6).
//! - [`keyring`] — `keyring-capability.get`: OS keyring read against the
//!   fixed `"savvagent"` service. Provider world only (Task 6).
//! - [`progress`] — `progress-capability.emit-stream-event`: forwards a
//!   WIT `stream-event` to the active host channel. Provider world only
//!   (Task 6).
//!
//! Capabilities specific to the interactive world that *were* in the
//! original spec (`draw-text`, `draw-block`, …) were rejected during
//! Task 5 review; the interactive world stays content-only.

pub mod http;
pub mod keyring;
pub mod log;
pub mod progress;
pub mod theme;
