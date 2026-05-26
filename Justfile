# Build the wasm fixtures used by `cargo test -p savvagent-plugin-wasm`.
#
# Requirements:
#   - rustup target add wasm32-unknown-unknown
#   - cargo install cargo-component --locked
#
# Each fixture is its own component-model Rust crate that targets one of
# the three WIT worlds. Built fixtures are copied to
# `crates/savvagent-plugin-wasm/tests/fixtures/*.wasm` and committed to the
# repo so day-to-day `cargo test` doesn't need the wasm toolchain.

# Build all wasm fixtures and copy them into the test fixtures dir.
build-fixtures: build-fixture-static build-fixture-interactive build-fixture-provider build-fixture-trap build-fixture-timeout build-fixture-denied-host build-fixture-denied-account

# Build the `plugin-static` world fixture and copy to tests/fixtures/static.wasm.
#
# We build for `wasm32-unknown-unknown` rather than `wasm32-wasip1` /
# `wasm32-wasip2` to avoid pulling in the WASI preview1-adapter import
# stubs (`wasi:cli/environment`, …). Our static-world plugin only needs
# `log` + `current-theme` host imports; the WASI stubs would force the
# host adapter to wire up a wasi-cli backend that pure-data plugins don't
# actually need.
build-fixture-static:
    cd crates/savvagent-plugin-wasm/tests/fixtures-src/static && \
        cargo component build --target wasm32-unknown-unknown --release
    cp crates/savvagent-plugin-wasm/tests/fixtures-src/static/target/wasm32-unknown-unknown/release/fixture_static.wasm \
       crates/savvagent-plugin-wasm/tests/fixtures/static.wasm

# Build the `plugin-interactive` world fixture and copy to
# tests/fixtures/interactive.wasm. Same `wasm32-unknown-unknown` target as
# the static fixture for the same WASI-import-avoidance reasons.
build-fixture-interactive:
    cd crates/savvagent-plugin-wasm/tests/fixtures-src/interactive && \
        cargo component build --target wasm32-unknown-unknown --release
    cp crates/savvagent-plugin-wasm/tests/fixtures-src/interactive/target/wasm32-unknown-unknown/release/fixture_interactive.wasm \
       crates/savvagent-plugin-wasm/tests/fixtures/interactive.wasm

# Build the `plugin-provider` world fixture and copy to
# tests/fixtures/provider.wasm. Same `wasm32-unknown-unknown` target as the
# static / interactive fixtures.
build-fixture-provider:
    cd crates/savvagent-plugin-wasm/tests/fixtures-src/provider && \
        cargo component build --target wasm32-unknown-unknown --release
    cp crates/savvagent-plugin-wasm/tests/fixtures-src/provider/target/wasm32-unknown-unknown/release/fixture_provider.wasm \
       crates/savvagent-plugin-wasm/tests/fixtures/provider.wasm

# ---- Task 7 fault-injection fixtures --------------------------------
#
# Four wasm components that exercise the host adapters' failure paths:
#
#   trap.wasm           — emits a `unreachable` instruction from
#                         `handle_slash("boom", ..)`; the static
#                         adapter must surface it as PluginError::Internal.
#   timeout.wasm        — busy-loops forever in `handle_slash("forever", ..)`;
#                         the integration test wraps the call in
#                         `tokio::time::timeout` to prove the host can
#                         bound its own awaiting. Epoch-based wasm
#                         interruption lands in Task 8.
#   denied-host.wasm    — provider plugin that calls
#                         http.fetch("https://evil.example/...") which
#                         is outside the manifest's allow-list. Host
#                         returns HttpError::DeniedHost.
#   denied-account.wasm — provider plugin that calls
#                         keyring.get("not-listed") which is outside the
#                         manifest's keyring-accounts allow-list. Host
#                         returns KeyringError::Denied without touching
#                         the OS keyring backend.

# Build the trap-fault fixture and copy to tests/fixtures/trap.wasm.
build-fixture-trap:
    cd crates/savvagent-plugin-wasm/tests/fixtures-src/trap && \
        cargo component build --target wasm32-unknown-unknown --release
    cp crates/savvagent-plugin-wasm/tests/fixtures-src/trap/target/wasm32-unknown-unknown/release/fixture_trap.wasm \
       crates/savvagent-plugin-wasm/tests/fixtures/trap.wasm

# Build the timeout-fault fixture and copy to tests/fixtures/timeout.wasm.
build-fixture-timeout:
    cd crates/savvagent-plugin-wasm/tests/fixtures-src/timeout && \
        cargo component build --target wasm32-unknown-unknown --release
    cp crates/savvagent-plugin-wasm/tests/fixtures-src/timeout/target/wasm32-unknown-unknown/release/fixture_timeout.wasm \
       crates/savvagent-plugin-wasm/tests/fixtures/timeout.wasm

# Build the denied-host fault fixture and copy to
# tests/fixtures/denied-host.wasm.
build-fixture-denied-host:
    cd crates/savvagent-plugin-wasm/tests/fixtures-src/denied-host && \
        cargo component build --target wasm32-unknown-unknown --release
    cp crates/savvagent-plugin-wasm/tests/fixtures-src/denied-host/target/wasm32-unknown-unknown/release/fixture_denied_host.wasm \
       crates/savvagent-plugin-wasm/tests/fixtures/denied-host.wasm

# Build the denied-account fault fixture and copy to
# tests/fixtures/denied-account.wasm.
build-fixture-denied-account:
    cd crates/savvagent-plugin-wasm/tests/fixtures-src/denied-account && \
        cargo component build --target wasm32-unknown-unknown --release
    cp crates/savvagent-plugin-wasm/tests/fixtures-src/denied-account/target/wasm32-unknown-unknown/release/fixture_denied_account.wasm \
       crates/savvagent-plugin-wasm/tests/fixtures/denied-account.wasm
