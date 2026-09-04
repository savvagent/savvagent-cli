//! Network seam for `internal:changelog`.
//!
//! The screen invokes [`ChangelogFetcher::fetch`] to pull the latest
//! `CHANGELOG.md` from GitHub. The trait keeps the reqwest call out of
//! unit tests; production uses [`GithubChangelogFetcher`] and tests
//! substitute a stub.

use async_trait::async_trait;
use std::time::Duration;

/// URL of the canonical CHANGELOG.md. Streams from `main` so the
/// viewer always reflects the most recent release — including entries
/// for versions the user hasn't installed yet.
pub const CHANGELOG_URL: &str =
    "https://raw.githubusercontent.com/savvagent/savvagent-cli/main/CHANGELOG.md";

/// User-Agent value sent with the request. Includes the running binary
/// version so request logs identify the caller cohort, mirroring the
/// pattern used in [`crate::plugin::builtin::self_update::check`].
const USER_AGENT: &str = concat!("savvagent-rs/", env!("CARGO_PKG_VERSION"), " (changelog)");

/// Hard upper bound on the network call. The viewer is user-facing and
/// opens on-demand; without a timeout, an unresponsive GitHub would
/// leave the screen stuck at "Fetching changelog…" until the user
/// pressed Esc. 10s is comfortably longer than the typical fetch
/// (~100–500ms) and short enough that a stalled connection surfaces
/// the `Failed` state quickly so the user can retry with `r`.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

#[async_trait]
pub trait ChangelogFetcher: Send + Sync {
    /// Return the raw markdown content of CHANGELOG.md, or an
    /// [`anyhow::Error`] on any failure (network, non-2xx, parse).
    async fn fetch(&self) -> anyhow::Result<String>;
}

/// Production [`ChangelogFetcher`] backed by `reqwest`.
#[derive(Debug, Default)]
pub struct GithubChangelogFetcher;

#[async_trait]
impl ChangelogFetcher for GithubChangelogFetcher {
    async fn fetch(&self) -> anyhow::Result<String> {
        let resp = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(FETCH_TIMEOUT)
            .build()?
            .get(CHANGELOG_URL)
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.text().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_targets_raw_main_branch() {
        // Regression guard: pinning to a tag would freeze the viewer at
        // the build's commit and stop users from seeing entries for
        // versions they haven't installed yet.
        assert!(
            CHANGELOG_URL.starts_with("https://raw.githubusercontent.com/savvagent/savvagent-cli/"),
            "URL must hit raw.githubusercontent.com: {CHANGELOG_URL}"
        );
        assert!(
            CHANGELOG_URL.ends_with("/main/CHANGELOG.md"),
            "URL must reference main/CHANGELOG.md: {CHANGELOG_URL}"
        );
    }

    #[test]
    fn user_agent_identifies_savvagent() {
        assert!(USER_AGENT.contains("savvagent"));
        assert!(USER_AGENT.contains("changelog"));
    }
}
