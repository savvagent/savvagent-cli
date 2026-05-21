//! Per-entry installer: binary download/verify/extract or npm i -g.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

use super::catalog::{CatalogEntry, InstallMethod, Target};

/// Per-stage progress observation emitted via the install path's
/// `notify` callback. Each variant marks the start of one stage; the
/// install path returns `Result<InstallOutcome, InstallError>` to
/// communicate terminal status, so there is no `Failed` variant here.
///
/// Today the only consumer is a `tracing::info!` sink in
/// [`super::LspInstallerPlugin::handle_install`] — these events are
/// **not** routed to the conversation log. Wiring them through the
/// `PushNote` channel for live progress is a planned follow-up
/// (requires either a `HostEvent` plumbing or an mpsc seam back into
/// the plugin event loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallProgress {
    /// Install task started for `entry_id`. Fired once per entry.
    Started {
        /// Catalog id (matches `CatalogEntry::id`).
        entry_id: String,
    },
    /// HTTP download in flight. The current implementation fires this
    /// twice — once before the request with `(0, None)` and once after
    /// the body has been buffered with `(final_size, Some(final_size))`.
    /// The shape supports per-chunk streaming if a future consumer
    /// wants it.
    Downloading {
        /// Catalog id.
        entry_id: String,
        /// Bytes received so far.
        bytes_so_far: u64,
        /// Total size in bytes, if the server reported it.
        total: Option<u64>,
    },
    /// SHA256 verification in progress.
    Verifying {
        /// Catalog id.
        entry_id: String,
    },
    /// Archive extraction in progress.
    Extracting {
        /// Catalog id.
        entry_id: String,
    },
    /// `npm i -g` running; `line` is one line of npm's combined
    /// stdout/stderr.
    RunningNpm {
        /// Catalog id.
        entry_id: String,
        /// One line of npm's output.
        line: String,
    },
    /// Install succeeded. `installed_at` is the absolute path to the
    /// binary that should be referenced in `lsp.toml`.
    Done {
        /// Catalog id.
        entry_id: String,
        /// Absolute path to the installed binary.
        installed_at: PathBuf,
    },
}

/// Returned by the install path on success — carries the data
/// [`super::config_writer`] needs to upsert the entry into
/// `~/.savvagent/lsp.toml`.
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    /// Catalog id (matches `CatalogEntry::id`).
    pub entry_id: String,
    /// Absolute path to the installed binary; used as the `command`
    /// value in the `lsp.toml` entry when
    /// [`super::catalog::CommandTemplate::Installed`] is used.
    pub installed_at: PathBuf,
}

/// Reasons the install path can return `Err`.
#[derive(Debug, Error)]
pub enum InstallError {
    /// The host's `(OS, arch)` doesn't match any supported `Target`.
    #[error("unsupported host target — {0}")]
    UnsupportedTarget(String),
    /// HTTP download failed.
    #[error("download failed: {0}")]
    Download(String),
    /// SHA256 of the downloaded payload didn't match the catalog's
    /// pinned value.
    #[error("checksum mismatch for {entry_id}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Catalog id.
        entry_id: String,
        /// Pinned SHA256 from the catalog (hex).
        expected: String,
        /// Computed SHA256 from the download (hex).
        actual: String,
    },
    /// Archive extraction failed.
    #[error("extract failed for {entry_id}: {reason}")]
    Extract {
        /// Catalog id.
        entry_id: String,
        /// Reason from the extractor.
        reason: String,
    },
    /// `npm i -g` exited non-zero or couldn't be invoked.
    #[error("npm install failed for {entry_id}: {reason}")]
    Npm {
        /// Catalog id.
        entry_id: String,
        /// Reason (npm's exit message or our own framing).
        reason: String,
    },
    /// Filesystem error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Thin abstraction over an HTTP client so tests can substitute a
/// fixture without spinning up a real server.
#[async_trait::async_trait]
pub trait Downloader: Send + Sync {
    /// Fetch `url` and return its body bytes.
    async fn fetch(&self, url: &str) -> Result<bytes::Bytes, InstallError>;
}

/// Production [`Downloader`] backed by `reqwest`. Sets the
/// `User-Agent: savvagent/<version>` header so GitHub's asset CDN logs
/// us as a known client.
pub struct ReqwestDownloader {
    /// The underlying client. Reused across fetches.
    pub client: reqwest::Client,
}

impl ReqwestDownloader {
    /// Build a default client. Returns `None` if `reqwest::Client::builder().build()`
    /// fails (network stack misconfigured); callers fall back to a
    /// PushNote error.
    pub fn new() -> Option<Self> {
        reqwest::Client::builder()
            .build()
            .ok()
            .map(|client| Self { client })
    }
}

#[async_trait::async_trait]
impl Downloader for ReqwestDownloader {
    async fn fetch(&self, url: &str) -> Result<bytes::Bytes, InstallError> {
        let resp = self
            .client
            .get(url)
            .header(
                reqwest::header::USER_AGENT,
                concat!("savvagent/", env!("CARGO_PKG_VERSION")),
            )
            .send()
            .await
            .map_err(|e| InstallError::Download(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(InstallError::Download(format!(
                "HTTP {}: {}",
                resp.status(),
                url
            )));
        }
        resp.bytes()
            .await
            .map_err(|e| InstallError::Download(e.to_string()))
    }
}

/// Install a single `BinaryDownload` catalog entry: download from the
/// pinned URL, verify SHA256, extract into
/// `<lsp_bin_root>/<entry.id>/`, set the executable bit on Unix.
///
/// `notify` receives one `InstallProgress` per stage; the wrapping
/// plugin pumps these into the conversation log.
pub async fn install_binary_entry(
    entry: &CatalogEntry,
    target: Target,
    lsp_bin_root: &Path,
    downloader: &dyn Downloader,
    notify: impl Fn(InstallProgress) + Send + Sync,
) -> Result<InstallOutcome, InstallError> {
    let InstallMethod::BinaryDownload { urls, binary_path } = entry.method else {
        return Err(InstallError::Download(format!(
            "{}: install_binary_entry called on a non-Binary entry",
            entry.id
        )));
    };

    let (_, url, expected_sha) = urls
        .iter()
        .find(|(t, _, _)| *t == target)
        .ok_or_else(|| InstallError::UnsupportedTarget(format!("{target:?}")))?;

    notify(InstallProgress::Started {
        entry_id: entry.id.into(),
    });

    notify(InstallProgress::Downloading {
        entry_id: entry.id.into(),
        bytes_so_far: 0,
        total: None,
    });
    let bytes = downloader.fetch(url).await?;
    notify(InstallProgress::Downloading {
        entry_id: entry.id.into(),
        bytes_so_far: bytes.len() as u64,
        total: Some(bytes.len() as u64),
    });

    notify(InstallProgress::Verifying {
        entry_id: entry.id.into(),
    });
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = hex::encode(hasher.finalize());
    if actual != *expected_sha {
        return Err(InstallError::ChecksumMismatch {
            entry_id: entry.id.into(),
            expected: (*expected_sha).into(),
            actual,
        });
    }

    notify(InstallProgress::Extracting {
        entry_id: entry.id.into(),
    });
    let install_dir = lsp_bin_root.join(entry.id);
    if install_dir.exists() {
        tokio::fs::remove_dir_all(&install_dir).await?;
    }
    tokio::fs::create_dir_all(&install_dir).await?;
    let installed_at = extract_one(&bytes, url, binary_path, &install_dir, entry.id).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&installed_at)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&installed_at, perms)?;
    }

    notify(InstallProgress::Done {
        entry_id: entry.id.into(),
        installed_at: installed_at.clone(),
    });
    Ok(InstallOutcome {
        entry_id: entry.id.into(),
        installed_at,
    })
}

/// Extract `bytes` into `install_dir`, choosing the extractor by `url`'s
/// suffix (`.gz` / `.tar.gz` / `.zip`). Returns the absolute path to
/// the binary inside the install dir (with `.exe` appended on Windows
/// if the catalog template omitted it).
///
/// All sync I/O (gzip decode, tar unpack, zip extract, write to disk)
/// runs inside `tokio::task::spawn_blocking` so a 30+ MB rust-analyzer
/// archive doesn't stall the runtime worker.
async fn extract_one(
    bytes: &bytes::Bytes,
    url: &str,
    binary_path: &str,
    install_dir: &Path,
    entry_id: &str,
) -> Result<PathBuf, InstallError> {
    enum ExtractKind {
        TarGz,
        Zip,
        GzipOnly,
    }
    let kind = if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
        ExtractKind::TarGz
    } else if url.ends_with(".zip") {
        ExtractKind::Zip
    } else if url.ends_with(".gz") {
        ExtractKind::GzipOnly
    } else {
        return Err(InstallError::Extract {
            entry_id: entry_id.into(),
            reason: format!("unrecognised archive suffix in {url}"),
        });
    };

    let bin_in_dir = install_dir.join(resolve_binary_path(binary_path));
    let bytes = bytes.clone();
    let install_dir = install_dir.to_path_buf();
    let entry_id_owned = entry_id.to_string();
    let bin_for_blocking = bin_in_dir.clone();

    tokio::task::spawn_blocking(move || -> Result<(), InstallError> {
        match kind {
            ExtractKind::TarGz => {
                let dec = flate2::read::GzDecoder::new(&bytes[..]);
                let mut ar = tar::Archive::new(dec);
                ar.unpack(&install_dir).map_err(|e| InstallError::Extract {
                    entry_id: entry_id_owned.clone(),
                    reason: e.to_string(),
                })?;
            }
            ExtractKind::Zip => {
                let reader = std::io::Cursor::new(&bytes[..]);
                let mut zip = zip::ZipArchive::new(reader).map_err(|e| InstallError::Extract {
                    entry_id: entry_id_owned.clone(),
                    reason: e.to_string(),
                })?;
                zip.extract(&install_dir)
                    .map_err(|e| InstallError::Extract {
                        entry_id: entry_id_owned.clone(),
                        reason: e.to_string(),
                    })?;
            }
            ExtractKind::GzipOnly => {
                use std::io::{Read, Write};
                let mut dec = flate2::read::GzDecoder::new(&bytes[..]);
                let mut out = std::fs::File::create(&bin_for_blocking)?;
                let mut buf = [0u8; 64 * 1024];
                loop {
                    let n = dec.read(&mut buf).map_err(InstallError::Io)?;
                    if n == 0 {
                        break;
                    }
                    out.write_all(&buf[..n])?;
                }
                out.flush()?;
            }
        }
        Ok(())
    })
    .await
    .map_err(|join_err| InstallError::Extract {
        entry_id: entry_id.into(),
        reason: format!("extractor task panicked: {join_err}"),
    })??;

    if !bin_in_dir.exists() {
        return Err(InstallError::Extract {
            entry_id: entry_id.into(),
            reason: format!("binary not found at {} after extract", bin_in_dir.display()),
        });
    }
    Ok(bin_in_dir)
}

/// Append `.exe` to `binary_path` on Windows when the catalog template
/// omitted it. The catalog deliberately keeps `binary_path` Unix-style
/// so a single literal works across targets.
fn resolve_binary_path(binary_path: &str) -> PathBuf {
    #[cfg(windows)]
    {
        if !binary_path.ends_with(".exe") {
            return PathBuf::from(format!("{binary_path}.exe"));
        }
    }
    PathBuf::from(binary_path)
}

/// Thin abstraction over the `npm` subprocess so tests can stub it.
#[async_trait::async_trait]
pub trait NpmRunner: Send + Sync {
    /// Run `npm i -g <package>@<version>`. Each line of npm's combined
    /// stdout/stderr is forwarded via `on_line`. Returns `Ok` on a
    /// zero exit code, `Err(message)` otherwise.
    async fn install_global(
        &self,
        package: &str,
        version: &str,
        on_line: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(), String>;
    /// Return `npm root -g` — the directory npm installs globals into.
    async fn root_global(&self) -> Result<PathBuf, String>;
}

/// Production [`NpmRunner`] backed by `tokio::process::Command`.
pub struct SystemNpmRunner;

#[async_trait::async_trait]
impl NpmRunner for SystemNpmRunner {
    async fn install_global(
        &self,
        package: &str,
        version: &str,
        on_line: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(), String> {
        use std::collections::VecDeque;
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::process::Command;

        let mut child = Command::new("npm")
            .args(["i", "-g", &format!("{package}@{version}")])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn npm: {e}"))?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let mut out_lines = BufReader::new(stdout).lines();
        let mut err_lines = BufReader::new(stderr).lines();

        // Drain both streams independently; one of them closing first
        // must not abandon the other (npm's stderr typically carries the
        // failure summary AFTER stdout has already EOF'd). Also keep a
        // small rolling tail so a non-zero exit can surface the actual
        // diagnostic rather than just "exit status 1".
        const TAIL_CAP: usize = 20;
        let mut tail: VecDeque<String> = VecDeque::with_capacity(TAIL_CAP);
        let mut out_done = false;
        let mut err_done = false;
        loop {
            tokio::select! {
                line = out_lines.next_line(), if !out_done => match line {
                    Ok(Some(l)) => {
                        if tail.len() == TAIL_CAP { tail.pop_front(); }
                        tail.push_back(l.clone());
                        on_line(l);
                    }
                    _ => out_done = true,
                },
                line = err_lines.next_line(), if !err_done => match line {
                    Ok(Some(l)) => {
                        if tail.len() == TAIL_CAP { tail.pop_front(); }
                        tail.push_back(l.clone());
                        on_line(l);
                    }
                    _ => err_done = true,
                },
                else => break,
            }
            if out_done && err_done {
                break;
            }
        }

        let status = child.wait().await.map_err(|e| format!("wait npm: {e}"))?;
        if !status.success() {
            let suffix = if tail.is_empty() {
                String::new()
            } else {
                format!(
                    " — last output:\n{}",
                    tail.into_iter().collect::<Vec<_>>().join("\n")
                )
            };
            return Err(format!("npm exited with status {status}{suffix}"));
        }
        Ok(())
    }

    async fn root_global(&self) -> Result<PathBuf, String> {
        let out = tokio::process::Command::new("npm")
            .args(["root", "-g"])
            .output()
            .await
            .map_err(|e| format!("spawn `npm root -g`: {e}"))?;
        if !out.status.success() {
            return Err(format!("npm root -g failed: status {}", out.status));
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(PathBuf::from(path))
    }
}

/// `Some(path)` if `npm` is on `$PATH`, `None` otherwise. The wrapping
/// plugin uses this to skip npm-based entries with a clear note rather
/// than blowing up mid-install.
pub fn detect_npm() -> Option<PathBuf> {
    which::which("npm").ok()
}

/// Install a single `NpmGlobal` catalog entry by shelling out to the
/// host's `npm`. Returns the absolute path npm placed the binary at;
/// the wrapping plugin writes that path into `lsp.toml` only when the
/// catalog's `lsp_entry.command` is `"{{BIN}}"` — usually npm entries
/// pin a literal `command` and the installed path is informational.
pub async fn install_npm_entry(
    entry: &CatalogEntry,
    runner: &dyn NpmRunner,
    notify: impl Fn(InstallProgress) + Send + Sync,
) -> Result<InstallOutcome, InstallError> {
    let InstallMethod::NpmGlobal { package, binary } = entry.method else {
        return Err(InstallError::Npm {
            entry_id: entry.id.into(),
            reason: "install_npm_entry called on a non-Npm entry".into(),
        });
    };

    notify(InstallProgress::Started {
        entry_id: entry.id.into(),
    });

    let entry_id_for_notify = entry.id.to_string();
    let notify_for_npm = &notify;
    runner
        .install_global(package, entry.version, &move |line| {
            notify_for_npm(InstallProgress::RunningNpm {
                entry_id: entry_id_for_notify.clone(),
                line,
            });
        })
        .await
        .map_err(|reason| InstallError::Npm {
            entry_id: entry.id.into(),
            reason,
        })?;

    let root = runner
        .root_global()
        .await
        .map_err(|reason| InstallError::Npm {
            entry_id: entry.id.into(),
            reason,
        })?;
    // `npm root -g` returns `<prefix>/lib/node_modules`. Bins live at
    // `<prefix>/bin/<binary>` on Unix; the installer surfaces that
    // path back to the caller. On Windows npm normally puts the bin on
    // `$PATH` directly, so the literal `command` in the catalog's
    // `lsp.toml` entry resolves without depending on the path we
    // compute here — this derivation is best-effort.
    let installed_at = root
        .parent()
        .and_then(|p| p.parent())
        .map(|prefix| prefix.join("bin").join(binary))
        .ok_or_else(|| InstallError::Npm {
            entry_id: entry.id.into(),
            reason: format!("could not derive bin path from npm root {}", root.display()),
        })?;

    notify(InstallProgress::Done {
        entry_id: entry.id.into(),
        installed_at: installed_at.clone(),
    });
    Ok(InstallOutcome {
        entry_id: entry.id.into(),
        installed_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::builtin::lsp_installer::catalog::{
        CatalogEntry, CommandTemplate, InstallMethod, LspEntryTemplate, Target,
    };

    fn fake_entry(urls: &'static [(Target, &'static str, &'static str)]) -> CatalogEntry {
        CatalogEntry {
            id: "fakelsp",
            display_name: "fakelsp",
            language_label: "fake",
            version: "0.0.0",
            method: InstallMethod::BinaryDownload {
                urls,
                binary_path: "fakelsp",
            },
            lsp_entry: LspEntryTemplate {
                id: "fake",
                extensions: &["fake"],
                root_markers: &["fake.toml"],
                command: CommandTemplate::Installed,
                args: &[],
            },
        }
    }

    struct StubDownloader {
        payload: bytes::Bytes,
    }

    #[async_trait::async_trait]
    impl Downloader for StubDownloader {
        async fn fetch(&self, _url: &str) -> Result<bytes::Bytes, InstallError> {
            Ok(self.payload.clone())
        }
    }

    fn gzipped(plain: &[u8]) -> Vec<u8> {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(plain).unwrap();
        enc.finish().unwrap()
    }

    #[tokio::test]
    async fn binary_download_happy_path_writes_executable() {
        let plain = b"#!/bin/sh\necho fakelsp\n";
        let archive = gzipped(plain);
        let sha = hex::encode(Sha256::digest(&archive));
        let url_static: &'static str = Box::leak(
            "https://example.test/fakelsp.gz"
                .to_string()
                .into_boxed_str(),
        );
        let sha_static: &'static str = Box::leak(sha.into_boxed_str());
        let urls: &'static [(Target, &'static str, &'static str)] =
            Box::leak(Box::new([(Target::LinuxX86_64Gnu, url_static, sha_static)]));

        let entry = fake_entry(urls);
        let tmp = tempfile::tempdir().unwrap();
        let dl = StubDownloader {
            payload: bytes::Bytes::from(archive),
        };
        let outcome = install_binary_entry(&entry, Target::LinuxX86_64Gnu, tmp.path(), &dl, |_| {})
            .await
            .unwrap();
        assert!(outcome.installed_at.exists(), "binary must exist on disk");
        let written = std::fs::read(&outcome.installed_at).unwrap();
        assert_eq!(written, plain, "binary contents must match");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&outcome.installed_at)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o111,
                0o111,
                "binary must be executable, got {mode:o}"
            );
        }
    }

    /// Build a `.tar.gz` archive in memory containing a single file at
    /// `binary_path` with the given contents.
    fn targz_with(binary_path: &str, contents: &[u8]) -> Vec<u8> {
        use flate2::{Compression, write::GzEncoder};
        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, binary_path, contents)
                .unwrap();
            builder.finish().unwrap();
        }
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut gz, &tar_buf).unwrap();
        gz.finish().unwrap()
    }

    /// Build a `.zip` archive in memory containing a single file at
    /// `binary_path` with the given contents.
    fn zipped_with(binary_path: &str, contents: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut writer = zip::ZipWriter::new(cursor);
            let options: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o755);
            zip::ZipWriter::start_file::<&str, ()>(&mut writer, binary_path, options).unwrap();
            std::io::Write::write_all(&mut writer, contents).unwrap();
            writer.finish().unwrap();
        }
        buf
    }

    // Real-world Windows archives ship binaries with explicit `.exe`
    // suffixes (e.g. rust-analyzer's `.zip` and lua-language-server's
    // Windows `.zip` both already contain `<name>.exe`); these tests
    // build Unix-flavored fixtures, so on Windows `resolve_binary_path`
    // looks for `<name>.exe` while the archive actually wrote `<name>`.
    // Windows coverage of the gzip-only path comes from
    // `smoke_local_http_install` and `reinstall_wipes_existing_install_dir`.
    #[cfg(unix)]
    #[tokio::test]
    async fn targz_extract_writes_binary_at_nested_path() {
        let payload = b"#!/bin/sh\necho lua\n";
        let archive = targz_with("bin/fakelsp", payload);
        let sha = hex::encode(Sha256::digest(&archive));
        let url_static: &'static str = Box::leak(
            "https://example.test/fakelsp.tar.gz"
                .to_string()
                .into_boxed_str(),
        );
        let sha_static: &'static str = Box::leak(sha.into_boxed_str());
        let urls: &'static [(Target, &'static str, &'static str)] =
            Box::leak(Box::new([(Target::LinuxX86_64Gnu, url_static, sha_static)]));
        let mut entry = fake_entry(urls);
        if let InstallMethod::BinaryDownload {
            ref mut binary_path,
            ..
        } = entry.method
        {
            *binary_path = "bin/fakelsp";
        }
        let tmp = tempfile::tempdir().unwrap();
        let dl = StubDownloader {
            payload: bytes::Bytes::from(archive),
        };
        let outcome = install_binary_entry(&entry, Target::LinuxX86_64Gnu, tmp.path(), &dl, |_| {})
            .await
            .unwrap();
        assert_eq!(outcome.installed_at, tmp.path().join("fakelsp/bin/fakelsp"));
        assert_eq!(std::fs::read(&outcome.installed_at).unwrap(), payload);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn zip_extract_writes_binary_at_top_level() {
        let payload = b"binary-bytes";
        let archive = zipped_with("fakelsp", payload);
        let sha = hex::encode(Sha256::digest(&archive));
        let url_static: &'static str = Box::leak(
            "https://example.test/fakelsp.zip"
                .to_string()
                .into_boxed_str(),
        );
        let sha_static: &'static str = Box::leak(sha.into_boxed_str());
        let urls: &'static [(Target, &'static str, &'static str)] =
            Box::leak(Box::new([(Target::LinuxX86_64Gnu, url_static, sha_static)]));
        let entry = fake_entry(urls);
        let tmp = tempfile::tempdir().unwrap();
        let dl = StubDownloader {
            payload: bytes::Bytes::from(archive),
        };
        let outcome = install_binary_entry(&entry, Target::LinuxX86_64Gnu, tmp.path(), &dl, |_| {})
            .await
            .unwrap();
        assert_eq!(outcome.installed_at, tmp.path().join("fakelsp/fakelsp"));
        assert_eq!(std::fs::read(&outcome.installed_at).unwrap(), payload);
    }

    #[tokio::test]
    async fn reinstall_wipes_existing_install_dir() {
        let payload = b"#!/bin/sh\necho v2\n";
        let archive = gzipped(payload);
        let sha = hex::encode(Sha256::digest(&archive));
        let url_static: &'static str = Box::leak(
            "https://example.test/fakelsp.gz"
                .to_string()
                .into_boxed_str(),
        );
        let sha_static: &'static str = Box::leak(sha.into_boxed_str());
        let urls: &'static [(Target, &'static str, &'static str)] =
            Box::leak(Box::new([(Target::LinuxX86_64Gnu, url_static, sha_static)]));
        let entry = fake_entry(urls);

        // Pre-populate the install dir with a sentinel from a "previous
        // install" — different version's auxiliary files that must be
        // cleared on reinstall.
        let tmp = tempfile::tempdir().unwrap();
        let install_dir = tmp.path().join("fakelsp");
        std::fs::create_dir_all(&install_dir).unwrap();
        let sentinel = install_dir.join("leftover-from-v1.txt");
        std::fs::write(&sentinel, b"stale state from a previous install").unwrap();
        assert!(sentinel.exists());

        let dl = StubDownloader {
            payload: bytes::Bytes::from(archive),
        };
        let outcome = install_binary_entry(&entry, Target::LinuxX86_64Gnu, tmp.path(), &dl, |_| {})
            .await
            .unwrap();
        assert!(outcome.installed_at.exists(), "fresh binary present");
        assert!(
            !sentinel.exists(),
            "previous install must be wiped on reinstall"
        );
    }

    #[tokio::test]
    async fn checksum_mismatch_returns_error() {
        let archive = gzipped(b"not-the-payload-we-expected");
        let urls: &'static [(Target, &'static str, &'static str)] = &[(
            Target::LinuxX86_64Gnu,
            "https://example.test/fakelsp.gz",
            "0000000000000000000000000000000000000000000000000000000000000000",
        )];
        let entry = fake_entry(urls);
        let tmp = tempfile::tempdir().unwrap();
        let dl = StubDownloader {
            payload: bytes::Bytes::from(archive),
        };
        let err = install_binary_entry(&entry, Target::LinuxX86_64Gnu, tmp.path(), &dl, |_| {})
            .await
            .unwrap_err();
        match err {
            InstallError::ChecksumMismatch { .. } => (),
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unsupported_target_returns_error() {
        let urls: &'static [(Target, &'static str, &'static str)] = &[(
            Target::LinuxX86_64Gnu,
            "https://example.test/fakelsp.gz",
            "0",
        )];
        let entry = fake_entry(urls);
        let tmp = tempfile::tempdir().unwrap();
        let dl = StubDownloader {
            payload: bytes::Bytes::new(),
        };
        let err = install_binary_entry(&entry, Target::MacosAarch64, tmp.path(), &dl, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, InstallError::UnsupportedTarget(_)));
    }

    struct StubNpm {
        install_result: Result<(), String>,
        root: PathBuf,
    }

    #[async_trait::async_trait]
    impl NpmRunner for StubNpm {
        async fn install_global(
            &self,
            _package: &str,
            _version: &str,
            on_line: &(dyn Fn(String) + Send + Sync),
        ) -> Result<(), String> {
            on_line("added 1 package".into());
            self.install_result.clone()
        }
        async fn root_global(&self) -> Result<PathBuf, String> {
            Ok(self.root.clone())
        }
    }

    fn npm_entry() -> CatalogEntry {
        CatalogEntry {
            id: "fake-npm-lsp",
            display_name: "fake-npm-lsp",
            language_label: "fake",
            version: "1.2.3",
            method: InstallMethod::NpmGlobal {
                package: "fake-npm-lsp",
                binary: "fake-npm-lsp",
            },
            lsp_entry: LspEntryTemplate {
                id: "fake",
                extensions: &["fake"],
                root_markers: &["fake.toml"],
                command: CommandTemplate::Literal("fake-npm-lsp"),
                args: &[],
            },
        }
    }

    #[tokio::test]
    async fn npm_happy_path_derives_bin_from_root() {
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path();
        let root = prefix.join("lib").join("node_modules");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::write(prefix.join("bin").join("fake-npm-lsp"), b"#!/bin/sh\n").unwrap();
        let runner = StubNpm {
            install_result: Ok(()),
            root,
        };
        let outcome = install_npm_entry(&npm_entry(), &runner, |_| {})
            .await
            .unwrap();
        assert_eq!(
            outcome.installed_at,
            prefix.join("bin").join("fake-npm-lsp")
        );
    }

    #[tokio::test]
    async fn npm_install_failure_returns_npm_error() {
        let runner = StubNpm {
            install_result: Err("network down".into()),
            root: PathBuf::from("/tmp/unused"),
        };
        let err = install_npm_entry(&npm_entry(), &runner, |_| {})
            .await
            .unwrap_err();
        match err {
            InstallError::Npm { reason, .. } => assert!(reason.contains("network down")),
            other => panic!("expected InstallError::Npm, got {other:?}"),
        }
    }

    #[test]
    fn checksum_mismatch_display_includes_both_hashes() {
        let e = InstallError::ChecksumMismatch {
            entry_id: "rust-analyzer".into(),
            expected: "abc".into(),
            actual: "def".into(),
        };
        let msg = format!("{e}");
        assert!(msg.contains("rust-analyzer"));
        assert!(msg.contains("abc"));
        assert!(msg.contains("def"));
    }

    /// End-to-end smoke test: serve a gzipped fixture over loopback,
    /// run `install_binary_entry` against the production
    /// [`ReqwestDownloader`], assert the binary was extracted with the
    /// executable bit set. Verifies the wiring between download → SHA
    /// verify → gzip extract that the stubbed-Downloader tests can't
    /// exercise.
    #[tokio::test]
    async fn smoke_local_http_install() {
        use std::sync::Arc;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;

        let payload = b"#!/bin/sh\necho hello-from-fakelsp\n";
        let archive = gzipped(payload);
        let sha = hex::encode(Sha256::digest(&archive));
        let archive = Arc::new(archive);

        // Single-shot HTTP/1.1 server. Listens on 127.0.0.1 with an
        // OS-assigned port, serves the gzipped archive, then exits.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/fakelsp.gz");

        let archive_for_server = Arc::clone(&archive);
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (read, mut write) = sock.split();
            let mut reader = BufReader::new(read);
            // Read + discard the request headers until an empty line.
            let mut buf = String::new();
            loop {
                buf.clear();
                let n = reader.read_line(&mut buf).await.unwrap_or(0);
                if n == 0 || buf == "\r\n" || buf == "\n" {
                    break;
                }
            }
            let body = &*archive_for_server;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n",
                body.len()
            );
            write.write_all(header.as_bytes()).await.unwrap();
            write.write_all(body).await.unwrap();
            write.flush().await.unwrap();
        });

        // Leak the URL + SHA + url-array into 'static so they can sit
        // in CatalogEntry's static-only fields. Acceptable in a test.
        let url_static: &'static str = Box::leak(url.into_boxed_str());
        let sha_static: &'static str = Box::leak(sha.into_boxed_str());
        let urls: &'static [(Target, &'static str, &'static str)] = Box::leak(Box::new([(
            Target::current().expect("supported host target"),
            url_static,
            sha_static,
        )]));

        let entry = CatalogEntry {
            id: "fakelsp-smoke",
            display_name: "fakelsp-smoke",
            language_label: "fake",
            version: "0.0.0",
            method: InstallMethod::BinaryDownload {
                urls,
                binary_path: "fakelsp-smoke",
            },
            lsp_entry: LspEntryTemplate {
                id: "fake",
                extensions: &["fake"],
                root_markers: &["fake.toml"],
                command: CommandTemplate::Installed,
                args: &[],
            },
        };

        let tmp = tempfile::tempdir().unwrap();
        let dl = ReqwestDownloader::new().expect("reqwest builds");
        let outcome =
            install_binary_entry(&entry, Target::current().unwrap(), tmp.path(), &dl, |_| {})
                .await
                .expect("install must succeed end-to-end");

        assert!(outcome.installed_at.exists());
        let written = std::fs::read(&outcome.installed_at).unwrap();
        assert_eq!(written, payload, "binary contents must match the fixture");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&outcome.installed_at)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "binary must be executable");
        }

        let _ = server.await;
    }
}
