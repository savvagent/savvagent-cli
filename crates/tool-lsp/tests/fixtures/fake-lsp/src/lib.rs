//! Stub library target so Cargo accepts this fixture as a path
//! dev-dependency from `tool-lsp`. Adding a `[lib]` target lets Cargo
//! include the package in the dependency graph (binary-only path deps
//! are silently dropped — see the "ignoring invalid dependency"
//! warning).
//!
//! Wiring `fake-lsp = { path = "tests/fixtures/fake-lsp" }` as a
//! dev-dep in Task 17 also exports `CARGO_BIN_EXE_fake-lsp` to the
//! integration test harness, so tests can locate the freshly-built
//! fixture binary without shelling out to `cargo build`.
//!
//! No real code lives here.
