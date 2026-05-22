//! Test-only utilities shared across the savvagent crate's unit test modules.
//!
//! `HOME_LOCK` serialises access to the global home-directory env variables,
//! so any test that uses [`HomeGuard`] (which rewrites them to a temp dir)
//! holds the lock for its lifetime. Both `app::tests` and
//! `theme_command_tests` must import this single mutex — keeping per-module
//! copies would let tests in one module race tests in the other on the
//! process-wide env.
//!
//! On Windows, `dirs::home_dir()` reads `USERPROFILE`, not `HOME`. To keep
//! production code that calls `dirs::home_dir()` inside a test sandbox, the
//! guard redirects BOTH variables on Windows. On Unix, only `HOME` is
//! touched.

#![cfg(test)]

use std::sync::Mutex;

/// Process-wide lock serialising every test that mutates the home-directory
/// env variables (`HOME` everywhere; `USERPROFILE` on Windows).
pub static HOME_LOCK: Mutex<()> = Mutex::new(());

/// RAII guard: on construction, redirects the platform's home-dir env
/// variables to a fresh tmpdir; on drop, restores the previous values (or
/// unsets them). Must be held while the test touches home-rooted paths
/// (e.g. `~/.savvagent/theme.toml`, `~/.savvagent/trusted-projects.json`).
pub struct HomeGuard {
    _td: tempfile::TempDir,
    prev_home: Option<std::ffi::OsString>,
    #[cfg(windows)]
    prev_userprofile: Option<std::ffi::OsString>,
}

impl HomeGuard {
    pub fn new() -> Self {
        let td = tempfile::TempDir::new().expect("tempdir");
        let prev_home = std::env::var_os("HOME");
        #[cfg(windows)]
        let prev_userprofile = std::env::var_os("USERPROFILE");
        // SAFETY: setting these env vars is unsafe in Rust 2024 because it
        // mutates process-global state. We hold HOME_LOCK for the lifetime
        // of the guard, so no other test reads them concurrently.
        unsafe {
            std::env::set_var("HOME", td.path());
            #[cfg(windows)]
            std::env::set_var("USERPROFILE", td.path());
        }
        Self {
            _td: td,
            prev_home,
            #[cfg(windows)]
            prev_userprofile,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: see HomeGuard::new — we still hold HOME_LOCK here.
        unsafe {
            match &self.prev_home {
                Some(p) => std::env::set_var("HOME", p),
                None => std::env::remove_var("HOME"),
            }
            #[cfg(windows)]
            match &self.prev_userprofile {
                Some(p) => std::env::set_var("USERPROFILE", p),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }
}
