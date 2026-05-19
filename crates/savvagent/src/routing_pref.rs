//! TUI-side helper that resolves `~/.savvagent/routing.toml` and reads
//! just the `default = "..."` field for the model-resolution chain.
//!
//! The full `RoutingRules` parser lives in `savvagent-host` and runs at
//! `Host::start`; this helper exists so `main.rs` can consult the
//! file's `default` during startup without depending on the full type.

use std::path::PathBuf;

use savvagent_host::{DefaultPick, RoutingRules};

/// Where the user's `routing.toml` lives, or `None` when no `$HOME`.
pub fn routing_toml_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let home = PathBuf::from(home);
    Some(home.join(".savvagent").join("routing.toml"))
}

/// Load `routing.toml`'s `default` field. Missing file → `None`. Parse
/// errors are logged at `warn!` and treated as `None` — the caller falls
/// back to the next layer in the precedence chain.
pub fn load_default_pick() -> Option<DefaultPick> {
    let path = routing_toml_path()?;
    match RoutingRules::load_from_path(&path) {
        Ok(rules) => rules.default,
        Err(e) => {
            tracing::warn!(error = %e, "could not load routing.toml#default at startup; ignoring");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_path_under_home() {
        let original_home = std::env::var_os("HOME");
        // SAFETY: single-threaded test
        unsafe {
            std::env::set_var("HOME", "/tmp/savvagent-test-home");
        }
        let p = routing_toml_path().expect("HOME set");
        assert!(p.ends_with(".savvagent/routing.toml"));
        match original_home {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}
