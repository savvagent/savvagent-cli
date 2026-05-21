//! Stub library target so Cargo accepts this fixture as a path
//! dev-dependency from `savvagent-host`. Adding a `[lib]` target lets
//! Cargo include the package in the dependency graph (binary-only path
//! deps are silently dropped — see the "ignoring invalid dependency"
//! warning).
//!
//! Including the package in the graph still does NOT cause Cargo to
//! export `CARGO_BIN_EXE_resource-tool` to the test harness — that env
//! var is only set for binaries owned by the crate-under-test, not for
//! binaries owned by path dev-deps. The integration test in
//! `tests/resources_integration.rs` shells out to `cargo build` for
//! this fixture's manifest and probes the workspace's `target/debug/`
//! directory to find the freshly-built binary instead.
//!
//! No real code lives here.
