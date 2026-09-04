//! Install path for `internal:self-update`.
//!
//! Invokes the cargo-dist installer script that ships with each GitHub
//! Release. Unlike the previous implementation (which wrapped
//! `self_update::backends::github::Update` and only swapped the main
//! `savvagent` binary), this driver replaces every binary in the release
//! archive — `savvagent` plus the six helper binaries
//! (`savvagent-anthropic`, `savvagent-gemini`, `savvagent-openai`,
//! `savvagent-tool-fs`, `savvagent-tool-bash`, `savvagent-tool-grep`).
//!
//! Mechanism: download the per-release installer (`savvagent-installer.sh`
//! on Unix, `savvagent-installer.ps1` on Windows), then exec it through
//! `sh` / `powershell`. The installer is the same script users invoke for
//! a fresh install, so it already knows how to fetch the right archive
//! for the host triple, extract every binary, and place them into the
//! original install location. Inheriting the installer means future
//! cargo-dist fixes (mirror fallback, signature checks, new helpers)
//! reach existing installs automatically.
//!
//! The [`Installer`] trait keeps the network + subprocess machinery out
//! of unit tests; production uses [`CargoDistInstaller`] and tests
//! substitute a stub.

use std::process::Stdio;

use async_trait::async_trait;
use semver::Version;

use super::UpdateState;

const REPO_OWNER: &str = "savvagent";
const REPO_NAME: &str = "savvagent-cli";

#[cfg(target_os = "windows")]
const INSTALLER_FILENAME: &str = "savvagent-installer.ps1";
#[cfg(not(target_os = "windows"))]
const INSTALLER_FILENAME: &str = "savvagent-installer.sh";

fn installer_url(latest: &Version) -> String {
    format!(
        "https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/download/v{latest}/{INSTALLER_FILENAME}"
    )
}

/// Runs the per-release cargo-dist installer for a given target version.
/// Production downloads + execs the script; tests substitute a stub that
/// records the requested version and returns a canned outcome.
#[async_trait]
pub trait Installer: Send + Sync {
    /// Install the release with version `latest`. Returns `Ok` only when
    /// the installer subprocess exits 0; on any other outcome (download
    /// failure, non-zero exit, IO error) the error message contains
    /// enough context to render in the failure banner.
    async fn install(&self, latest: &Version) -> anyhow::Result<()>;
}

/// Production [`Installer`] backed by the cargo-dist installer scripts
/// shipped with each GitHub release.
#[derive(Debug, Default)]
pub struct CargoDistInstaller;

#[async_trait]
impl Installer for CargoDistInstaller {
    async fn install(&self, latest: &Version) -> anyhow::Result<()> {
        let url = installer_url(latest);
        let script = download_script(&url).await?;
        run_installer(&script).await
    }
}

async fn download_script(url: &str) -> anyhow::Result<String> {
    let resp = reqwest::Client::builder()
        .user_agent(concat!(
            "savvagent-rs/",
            env!("CARGO_PKG_VERSION"),
            " (self-update)"
        ))
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?;
    Ok(resp.text().await?)
}

#[cfg(not(target_os = "windows"))]
async fn run_installer(script: &str) -> anyhow::Result<()> {
    spawn_and_wait("sh", &[], script).await
}

#[cfg(target_os = "windows")]
async fn run_installer(script: &str) -> anyhow::Result<()> {
    spawn_and_wait(
        "powershell",
        &["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", "-"],
        script,
    )
    .await
}

/// Pipe `script` into the named interpreter via stdin, capture both
/// streams, and bubble up the combined output on non-zero exit. Keeps
/// the script off disk so a partially-downloaded installer can't be
/// rerun later by accident.
async fn spawn_and_wait(program: &str, args: &[&str], script: &str) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(script.as_bytes()).await?;
        stdin.shutdown().await?;
    }

    let output = child.wait_with_output().await?;
    if output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = combine_streams(&stdout, &stderr);
    anyhow::bail!(
        "installer exited with {}: {combined}",
        output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into())
    );
}

fn combine_streams(stdout: &str, stderr: &str) -> String {
    match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (true, true) => "<no output>".into(),
        (false, true) => stdout.trim().to_string(),
        (true, false) => stderr.trim().to_string(),
        (false, false) => format!("{} | {}", stdout.trim(), stderr.trim()),
    }
}

/// Drive the install path: invoke the installer and, on success, return
/// [`UpdateState::Updated`]. On failure the caller is expected to
/// transition to [`UpdateState::InstallFailed`] with the error string.
pub async fn apply_update<I: Installer + ?Sized>(
    installer: &I,
    current: Version,
    latest: Version,
) -> anyhow::Result<UpdateState> {
    installer.install(&latest).await?;
    Ok(UpdateState::Updated {
        from: current,
        to: latest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct StubInstaller {
        result: Mutex<Result<(), String>>,
        last_call: Mutex<Option<Version>>,
    }

    impl StubInstaller {
        fn ok() -> Self {
            Self {
                result: Mutex::new(Ok(())),
                last_call: Mutex::new(None),
            }
        }
        fn err(msg: &str) -> Self {
            Self {
                result: Mutex::new(Err(msg.into())),
                last_call: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl Installer for StubInstaller {
        async fn install(&self, latest: &Version) -> anyhow::Result<()> {
            *self.last_call.lock().unwrap() = Some(latest.clone());
            match &*self.result.lock().unwrap() {
                Ok(()) => Ok(()),
                Err(msg) => Err(anyhow::anyhow!(msg.clone())),
            }
        }
    }

    #[tokio::test]
    async fn apply_returns_updated_state_on_success() {
        let installer = StubInstaller::ok();
        let current = Version::parse("0.10.0").unwrap();
        let latest = Version::parse("0.11.0").unwrap();
        let state = apply_update(&installer, current.clone(), latest.clone())
            .await
            .unwrap();
        match state {
            UpdateState::Updated { from, to } => {
                assert_eq!(from, current);
                assert_eq!(to, latest);
            }
            other => panic!("expected Updated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_propagates_installer_error() {
        let installer = StubInstaller::err("download failed");
        let current = Version::parse("0.10.0").unwrap();
        let latest = Version::parse("0.11.0").unwrap();
        let err = apply_update(&installer, current, latest).await.unwrap_err();
        assert!(err.to_string().contains("download failed"), "got: {err}");
    }

    #[tokio::test]
    async fn apply_forwards_latest_version_to_installer() {
        let installer = StubInstaller::ok();
        let current = Version::parse("0.10.0").unwrap();
        let latest = Version::parse("0.11.2").unwrap();
        apply_update(&installer, current, latest.clone())
            .await
            .unwrap();
        let last = installer.last_call.lock().unwrap().clone().unwrap();
        assert_eq!(last, latest);
    }

    #[test]
    fn installer_url_targets_the_correct_release_asset() {
        let url = installer_url(&Version::parse("0.13.0").unwrap());
        assert!(
            url.starts_with(
                "https://github.com/savvagent/savvagent-cli/releases/download/v0.13.0/"
            ),
            "url should target the savvagent/savvagent-cli release for the requested tag: {url}"
        );
        assert!(
            url.ends_with(INSTALLER_FILENAME),
            "url should fetch the platform installer script: {url}"
        );
    }

    #[test]
    fn combine_streams_handles_empty_inputs() {
        assert_eq!(combine_streams("", ""), "<no output>");
        assert_eq!(combine_streams("out", ""), "out");
        assert_eq!(combine_streams("", "err"), "err");
        assert_eq!(combine_streams("out", "err"), "out | err");
    }
}
