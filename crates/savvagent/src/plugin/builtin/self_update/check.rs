//! Version-check logic for `internal:self-update`.
//!
//! Scope of v0.11.0 PR 2: query the GitHub Releases API for the latest
//! tag, compare against the running binary's compiled-in version, and
//! produce an [`UpdateState`]. The UI surface (banner slot) and the
//! `/update` apply path are wired in later PRs — PR 2 only mutates
//! plugin state in response to [`savvagent_plugin::HostEvent::HostStarting`].
//!
//! The trait-injected [`ReleasesFetcher`] keeps the production reqwest
//! call out of unit tests; tests substitute a synchronous stub so the
//! state-transition logic is covered without touching the network.

use async_trait::async_trait;
use semver::Version;

use super::InstallMethod;

/// GitHub repo to query for releases. Hardcoded — the plugin only
/// targets this project's official release feed.
const RELEASES_API_URL: &str =
    "https://api.github.com/repos/savvagent/savvagent-cli/releases/latest";

/// User-Agent value GitHub requires on the releases endpoint. Includes
/// the running binary version so request logs at GitHub identify the
/// caller cohort.
const USER_AGENT: &str = concat!("savvagent-rs/", env!("CARGO_PKG_VERSION"), " (self-update)");

/// The plugin's view of "is there a newer release?". Stored in plugin
/// state and read by [`crate::plugin::builtin::self_update::SelfUpdatePlugin`]'s
/// `render_slot` and `handle_slash`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateState {
    /// Initial state before the `HostStarting` task has produced a result.
    Unknown,
    /// The plugin is short-circuited and will not check or apply updates.
    /// Set for dev builds and when opt-out flags are set.
    Disabled,
    /// The running binary matches or exceeds the latest release.
    UpToDate,
    /// A newer release is available; carries both versions so the banner
    /// can render the transition. Transient — the `HostStarting` task
    /// flips to [`UpdateState::Installing`] immediately after producing
    /// this state, so users typically only see it on the first frame
    /// after detection.
    Available {
        /// Running binary version (from `CARGO_PKG_VERSION`).
        current: Version,
        /// Latest release version (parsed from the tag, with any leading
        /// `v` stripped).
        latest: Version,
    },
    /// Background install in progress (downloading + running the cargo-dist
    /// installer script). Set by the `HostStarting` task right before it
    /// invokes the installer; replaced with [`UpdateState::Updated`] or
    /// [`UpdateState::InstallFailed`] when the installer exits.
    Installing {
        /// Running binary version.
        current: Version,
        /// Version being installed.
        latest: Version,
    },
    /// Background install attempted and failed. The user can retry via
    /// `/update`, which re-runs the installer.
    InstallFailed {
        /// Running binary version.
        current: Version,
        /// Version that failed to install.
        latest: Version,
        /// Combined stdout/stderr from the installer subprocess (or the
        /// download error message). Surfaced verbatim in the banner and
        /// retry note.
        error: String,
    },
    /// Install succeeded; the on-disk binaries are now `to` but the
    /// running process is still `from`. The banner row reflects this so
    /// the user is prompted to restart.
    Updated {
        /// Version the running process was when the install started.
        from: Version,
        /// Version of the newly-installed on-disk binaries.
        to: Version,
    },
    /// Network / parse / IO error during the check. Logged at `debug` only;
    /// no UI surface.
    CheckFailed,
}

/// Outcome of comparing two version strings. Splits out from
/// [`UpdateState`] so the pure helper [`compare_versions`] is reusable
/// in both production code and tests without the `current` / `latest`
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    /// Running version is strictly behind the latest release — show banner.
    Behind,
    /// Running version equals the latest release — no banner.
    Equal,
    /// Running version is ahead (developer running a future build, or the
    /// latest tag rolled back). No banner — treat as up-to-date.
    Ahead,
    /// Either side failed to parse as SemVer.
    Unparseable,
}

/// Compare two version strings. Strips a single leading `v`/`V` from the
/// `latest` argument (GitHub release tags use the `vX.Y.Z` convention)
/// and parses both with [`semver::Version`]. Returns
/// [`Comparison::Unparseable`] on any parse error rather than panicking.
pub fn compare_versions(current: &str, latest: &str) -> Comparison {
    let latest_str = latest.strip_prefix(['v', 'V']).unwrap_or(latest);
    let Ok(current_v) = Version::parse(current) else {
        return Comparison::Unparseable;
    };
    let Ok(latest_v) = Version::parse(latest_str) else {
        return Comparison::Unparseable;
    };
    match current_v.cmp(&latest_v) {
        std::cmp::Ordering::Less => Comparison::Behind,
        std::cmp::Ordering::Equal => Comparison::Equal,
        std::cmp::Ordering::Greater => Comparison::Ahead,
    }
}

/// Trait abstracting the GitHub Releases API call. Production uses
/// [`GithubReleasesFetcher`]; tests substitute a stub so the rest of the
/// check logic exercises without touching the network.
#[async_trait]
pub trait ReleasesFetcher: Send + Sync {
    /// Return the latest release tag (`tag_name` field of GitHub's
    /// `/releases/latest` JSON), or an [`anyhow::Error`] on any failure
    /// (network, parse, non-2xx status, etc.).
    async fn latest_tag(&self) -> anyhow::Result<String>;
}

/// Default [`ReleasesFetcher`] backed by `reqwest`. Uses the workspace's
/// rustls-enabled reqwest client and sets the GitHub-required User-Agent.
#[derive(Debug, Default)]
pub struct GithubReleasesFetcher;

#[async_trait]
impl ReleasesFetcher for GithubReleasesFetcher {
    async fn latest_tag(&self) -> anyhow::Result<String> {
        #[derive(serde::Deserialize)]
        struct LatestRelease {
            tag_name: String,
        }

        let client = reqwest::Client::builder().user_agent(USER_AGENT).build()?;
        let resp = client
            .get(RELEASES_API_URL)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?
            .error_for_status()?;
        let parsed: LatestRelease = resp.json().await?;
        Ok(parsed.tag_name)
    }
}

/// Drive the full version-check flow: short-circuit on dev builds,
/// otherwise call the fetcher and compare. Returns [`UpdateState::CheckFailed`]
/// on any fetch error (logged at `debug` upstream).
///
/// The function is intentionally cache-free — the plugin's `on_event`
/// handler wraps this call with cache read/write so unit tests of the
/// compare/fetch logic can run without touching the filesystem.
pub async fn check_for_update<F: ReleasesFetcher + ?Sized>(
    current_version: &str,
    install_method: InstallMethod,
    fetcher: &F,
) -> UpdateState {
    if matches!(install_method, InstallMethod::Dev) {
        return UpdateState::Disabled;
    }

    let tag = match fetcher.latest_tag().await {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!(error = %e, "self-update: GitHub Releases query failed");
            return UpdateState::CheckFailed;
        }
    };

    classify_tag(current_version, &tag)
}

/// Pure helper: turn a (`current_version`, `tag`) pair into an
/// [`UpdateState`]. Used by [`check_for_update`] on the fresh-fetch path
/// and by the plugin's cached path in `on_event`.
pub fn classify_tag(current_version: &str, tag: &str) -> UpdateState {
    match compare_versions(current_version, tag) {
        Comparison::Behind => {
            let tag_str = tag.strip_prefix(['v', 'V']).unwrap_or(tag);
            match (Version::parse(current_version), Version::parse(tag_str)) {
                (Ok(current), Ok(latest)) => UpdateState::Available { current, latest },
                _ => UpdateState::CheckFailed,
            }
        }
        Comparison::Equal | Comparison::Ahead => UpdateState::UpToDate,
        Comparison::Unparseable => {
            tracing::debug!(
                current = current_version,
                latest = tag,
                "self-update: could not parse version strings"
            );
            UpdateState::CheckFailed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // --- compare_versions ---

    #[test]
    fn compare_strips_v_prefix() {
        assert_eq!(compare_versions("0.10.0", "v0.11.0"), Comparison::Behind);
        assert_eq!(compare_versions("0.10.0", "V0.11.0"), Comparison::Behind);
        assert_eq!(compare_versions("0.10.0", "0.11.0"), Comparison::Behind);
    }

    #[test]
    fn compare_recognises_equal_versions() {
        assert_eq!(compare_versions("0.10.0", "v0.10.0"), Comparison::Equal);
    }

    #[test]
    fn compare_recognises_ahead_when_current_is_newer() {
        assert_eq!(compare_versions("0.11.0", "v0.10.0"), Comparison::Ahead);
    }

    #[test]
    fn compare_handles_patch_level_difference() {
        assert_eq!(compare_versions("0.10.0", "v0.10.1"), Comparison::Behind);
        assert_eq!(compare_versions("0.10.2", "v0.10.1"), Comparison::Ahead);
    }

    #[test]
    fn compare_handles_prerelease_lower_than_release() {
        // 0.11.0-alpha.1 < 0.11.0 per SemVer.
        assert_eq!(
            compare_versions("0.11.0-alpha.1", "v0.11.0"),
            Comparison::Behind
        );
    }

    #[test]
    fn compare_returns_unparseable_for_garbage_tag() {
        assert_eq!(
            compare_versions("0.10.0", "not-a-version"),
            Comparison::Unparseable
        );
    }

    #[test]
    fn compare_returns_unparseable_for_garbage_current() {
        assert_eq!(
            compare_versions("garbage", "v0.10.0"),
            Comparison::Unparseable
        );
    }

    // --- check_for_update with stub fetcher ---

    /// Test stub: returns a canned tag string or a canned error.
    struct StubFetcher {
        /// `Ok(tag)` returns the tag; `Err(message)` produces an
        /// `anyhow::Error` with that message. Wrapped in a Mutex so the
        /// fixture can be configured per test without `mut self`.
        result: Mutex<Result<String, String>>,
    }

    impl StubFetcher {
        fn ok(tag: &str) -> Self {
            Self {
                result: Mutex::new(Ok(tag.to_string())),
            }
        }
        fn err(msg: &str) -> Self {
            Self {
                result: Mutex::new(Err(msg.to_string())),
            }
        }
    }

    #[async_trait]
    impl ReleasesFetcher for StubFetcher {
        async fn latest_tag(&self) -> anyhow::Result<String> {
            match &*self.result.lock().unwrap() {
                Ok(t) => Ok(t.clone()),
                Err(e) => Err(anyhow::anyhow!(e.clone())),
            }
        }
    }

    #[tokio::test]
    async fn check_returns_disabled_for_dev_builds() {
        let stub = StubFetcher::ok("v99.99.99");
        let state = check_for_update("0.10.0", InstallMethod::Dev, &stub).await;
        assert_eq!(state, UpdateState::Disabled);
    }

    #[tokio::test]
    async fn check_returns_available_when_remote_is_newer() {
        let stub = StubFetcher::ok("v0.11.0");
        let state = check_for_update("0.10.0", InstallMethod::Installed, &stub).await;
        match state {
            UpdateState::Available { current, latest } => {
                assert_eq!(current.to_string(), "0.10.0");
                assert_eq!(latest.to_string(), "0.11.0");
            }
            other => panic!("expected Available, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn check_returns_up_to_date_on_equal() {
        let stub = StubFetcher::ok("v0.10.0");
        let state = check_for_update("0.10.0", InstallMethod::Installed, &stub).await;
        assert_eq!(state, UpdateState::UpToDate);
    }

    #[tokio::test]
    async fn check_returns_up_to_date_when_running_ahead() {
        let stub = StubFetcher::ok("v0.9.0");
        let state = check_for_update("0.10.0", InstallMethod::Installed, &stub).await;
        assert_eq!(state, UpdateState::UpToDate);
    }

    #[tokio::test]
    async fn check_returns_check_failed_on_fetch_error() {
        let stub = StubFetcher::err("network down");
        let state = check_for_update("0.10.0", InstallMethod::Installed, &stub).await;
        assert_eq!(state, UpdateState::CheckFailed);
    }

    #[tokio::test]
    async fn check_returns_check_failed_on_unparseable_tag() {
        let stub = StubFetcher::ok("not-a-version");
        let state = check_for_update("0.10.0", InstallMethod::Installed, &stub).await;
        assert_eq!(state, UpdateState::CheckFailed);
    }
}
