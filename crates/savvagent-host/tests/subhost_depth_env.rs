//! Env-var tests for `max_depth_from_env`. Lives as an integration test
//! (not a unit test) because `savvagent-host` declares
//! `#![forbid(unsafe_code)]` at the crate root, and the Rust 2024 edition
//! requires `unsafe` blocks around `std::env::set_var` /
//! `std::env::remove_var`. Integration tests are separate binaries and
//! do not inherit the lib crate's lint level.
//!
//! `set_var` / `remove_var` are process-global, so the three tests in
//! this binary serialize on `ENV_LOCK` to keep each other's mutations
//! from racing under the default parallel test runner. Each test still
//! saves and restores the previous value of `SAVVAGENT_AGENT_MAX_DEPTH`
//! so it does not leak outside the locked section.

use std::sync::Mutex;

use savvagent_host::max_depth_from_env;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn depth_limit_env_default_is_three() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: this integration test binary is the only writer of
    // SAVVAGENT_AGENT_MAX_DEPTH, sibling tests serialize on ENV_LOCK,
    // and the previous value is restored before the guard drops.
    let prev = std::env::var("SAVVAGENT_AGENT_MAX_DEPTH").ok();
    unsafe {
        std::env::remove_var("SAVVAGENT_AGENT_MAX_DEPTH");
    }
    assert_eq!(max_depth_from_env(), 3);
    if let Some(v) = prev {
        unsafe {
            std::env::set_var("SAVVAGENT_AGENT_MAX_DEPTH", v);
        }
    }
}

#[test]
fn depth_limit_env_override_parses() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: see `depth_limit_env_default_is_three`.
    let prev = std::env::var("SAVVAGENT_AGENT_MAX_DEPTH").ok();
    unsafe {
        std::env::set_var("SAVVAGENT_AGENT_MAX_DEPTH", "5");
    }
    assert_eq!(max_depth_from_env(), 5);
    unsafe {
        match prev {
            Some(v) => std::env::set_var("SAVVAGENT_AGENT_MAX_DEPTH", v),
            None => std::env::remove_var("SAVVAGENT_AGENT_MAX_DEPTH"),
        }
    }
}

#[test]
fn depth_limit_env_invalid_falls_back() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: see `depth_limit_env_default_is_three`.
    let prev = std::env::var("SAVVAGENT_AGENT_MAX_DEPTH").ok();
    unsafe {
        std::env::set_var("SAVVAGENT_AGENT_MAX_DEPTH", "not-a-number");
    }
    assert_eq!(max_depth_from_env(), 3);
    unsafe {
        match prev {
            Some(v) => std::env::set_var("SAVVAGENT_AGENT_MAX_DEPTH", v),
            None => std::env::remove_var("SAVVAGENT_AGENT_MAX_DEPTH"),
        }
    }
}
