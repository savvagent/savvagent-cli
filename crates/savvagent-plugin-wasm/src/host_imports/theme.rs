//! `current-theme()` host capability.
//!
//! Exposes a snapshot of the active theme's `(name, color)` map to wasm
//! guests. The map is held behind a [`tokio::sync::RwLock`] so the TUI can
//! swap it atomically whenever the active theme changes (via
//! `Effect::SetActiveTheme`), without needing to rebuild every plugin's
//! `Store`.
//!
//! In v0.18.0 the host resolves every semantic theme slot
//! (`ThemeColor::Fg`, `::Bg`, `::Accent`, …) against the active palette
//! *before* publishing the snapshot, so guest plugins see only literal
//! ANSI/indexed/RGB colors. That keeps the WIT surface tiny: no semantic
//! slot ever crosses the boundary.

use std::sync::Arc;

use tokio::sync::RwLock;

use savvagent_plugin as sp;

/// Thread-safe handle to the active theme map. Cheap to clone (internal
/// `Arc`); read paths take a non-blocking `read().await`.
pub type ThemeProvider = Arc<RwLock<Vec<(String, sp::ThemeColor)>>>;

/// Construct a `ThemeProvider` populated with the given initial snapshot.
///
/// Callers (the TUI's app state) own the provider and update it via
/// `provider.write().await.clone_from(&new_map)` whenever a `SetActiveTheme`
/// effect is applied; reads via the host-import are non-blocking so multiple
/// plugins can fan out the same snapshot in parallel.
pub fn provider(initial: Vec<(String, sp::ThemeColor)>) -> ThemeProvider {
    Arc::new(RwLock::new(initial))
}

/// Build a fresh snapshot for handing to wasm. Pulls under `read().await`
/// and clones — the wasm caller must not hold a lock across its own awaits.
///
/// Returned vec is in the same insertion order as `provider` (no implicit
/// sort) so deterministic guests can rely on stable ordering.
pub async fn snapshot(provider: &ThemeProvider) -> Vec<(String, sp::ThemeColor)> {
    provider.read().await.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::ThemeColor;

    #[tokio::test]
    async fn provider_default_is_empty() {
        let p = provider(Vec::new());
        let snap = snapshot(&p).await;
        assert!(snap.is_empty());
    }

    #[tokio::test]
    async fn provider_round_trips_initial_snapshot() {
        let initial = vec![
            ("bg".into(), ThemeColor::Black),
            ("fg".into(), ThemeColor::White),
            ("accent".into(), ThemeColor::Blue),
        ];
        let p = provider(initial.clone());
        let snap = snapshot(&p).await;
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].0, "bg");
        assert_eq!(snap[2].1, ThemeColor::Blue);
    }

    #[tokio::test]
    async fn provider_supports_write_then_read() {
        let p = provider(Vec::new());
        {
            let mut w = p.write().await;
            w.push(("fg".into(), ThemeColor::White));
        }
        let snap = snapshot(&p).await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].0, "fg");
    }
}
