//! `/plugins install <url>` — fetch a `plugin.toml`, validate it, fetch the
//! referenced `plugin.wasm`, hash the staged tree, and ask the user to
//! confirm trust via a [`ScreenArgs::PluginsTrustModal`] modal.
//!
//! All work up to (but not including) writing the trust record is performed
//! into a tempdir owned by this function. The handle is converted via
//! [`tempfile::TempDir::into_path`] so it survives past the function return:
//! the trust modal then either moves the staged directory into the
//! user-tier plugin path on confirm, or `std::fs::remove_dir_all`s it on
//! cancel.
//!
//! ## Size caps (defence-in-depth)
//!
//! - `plugin.toml` is capped at **64 KiB** — far larger than any plausible
//!   manifest and still cheap to fully buffer.
//! - `plugin.wasm` is capped at **32 MiB** — comfortably larger than the
//!   committed fixtures (~1.5 MiB) but small enough that a hostile mirror
//!   can't OOM the agent by serving a multi-gigabyte body.
//!
//! Both caps are applied **after** the body is downloaded (`reqwest::bytes`
//! returns the whole body), so a malicious server can still force us to
//! buffer up to its `Content-Length`. A future hardening pass should swap
//! to a streamed read with an early-abort once the cap is exceeded; for
//! the v0.18.0 cut the post-hoc check is adequate.

use std::path::Path;

use reqwest::Client;
use savvagent_plugin::{Effect, PluginError, ScreenArgs, StyledLine};
use savvagent_plugin_wasm::manifest::PluginManifest;
use savvagent_plugin_wasm::trust::tree_hash;

const MAX_TOML_BYTES: usize = 64 * 1024;
const MAX_WASM_BYTES: usize = 32 * 1024 * 1024;

/// Download + validate the plugin pointed to by `toml_url` and return a
/// single `Effect::OpenScreen { id: "plugins.trust-modal", … }` carrying
/// the staging tempdir's path. Errors surface as `PluginError::Internal`
/// with a user-facing string; the caller already wraps these into a
/// `PushNote` so no panic-style messages leak to the UI.
pub async fn install(home_dir: &Path, toml_url: &str) -> Result<Vec<Effect>, PluginError> {
    let client = Client::builder()
        .use_rustls_tls()
        // Top-level request timeout. The body-size caps below catch
        // oversize responses post-receive; this timeout catches the
        // slow-loris case where the server holds the TCP connection
        // open without sending bytes.
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| PluginError::Internal(format!("reqwest: {e}")))?;

    let toml_text = fetch_capped_text(&client, toml_url, MAX_TOML_BYTES).await?;

    // Stage in a tempdir. We use `into_path()` after a successful build
    // so the directory survives past this function and the trust modal
    // can either move it or delete it.
    let staging =
        tempfile::tempdir().map_err(|e| PluginError::Internal(format!("tempdir: {e}")))?;
    let staging_path = staging.path().to_path_buf();
    std::fs::write(staging_path.join("plugin.toml"), &toml_text)
        .map_err(|e| PluginError::Internal(format!("write plugin.toml: {e}")))?;

    // Cross-check the manifest's `[plugin] id` matches the on-disk
    // directory name we'd install into (`<id>/`), then run the full
    // PluginManifest::load() validation.
    let parsed_id = extract_id(&toml_text)?;
    let manifest = PluginManifest::load(&staging_path.join("plugin.toml"), &parsed_id)
        .map_err(|e| PluginError::Internal(format!("manifest: {e}")))?;

    let wasm_url = manifest
        .plugin
        .wasm
        .as_deref()
        .ok_or_else(|| PluginError::Internal("plugin.toml missing wasm = URL".into()))?;
    let wasm_bytes = fetch_capped_bytes(&client, wasm_url, MAX_WASM_BYTES).await?;
    std::fs::write(staging_path.join("plugin.wasm"), &wasm_bytes)
        .map_err(|e| PluginError::Internal(format!("write plugin.wasm: {e}")))?;

    let hash =
        tree_hash(&staging_path).map_err(|e| PluginError::Internal(format!("tree_hash: {e}")))?;

    // Disable auto-cleanup; the trust modal owns the staging dir from
    // here on. `TempDir::keep` returns the on-disk path and stops the
    // drop-time cleanup; the modal is responsible for moving or
    // deleting the directory.
    let staging_dir = staging.keep();

    Ok(vec![Effect::OpenScreen {
        id: "plugins.trust-modal".into(),
        args: ScreenArgs::PluginsTrustModal {
            id: parsed_id,
            name: manifest.plugin.name,
            version: manifest.plugin.version,
            source_url: toml_url.to_string(),
            hash,
            staging_dir,
            home_dir: home_dir.to_path_buf(),
        },
    }])
}

/// Helper that returns a fresh `Effect::PushNote` carrying a plain line.
/// Used by `/plugins` subcommand handlers; lives here so `mod.rs` can
/// stay terse.
pub(super) fn push_note(text: impl Into<String>) -> Effect {
    Effect::PushNote {
        line: StyledLine::plain(text.into()),
    }
}

async fn fetch_capped_text(client: &Client, url: &str, cap: usize) -> Result<String, PluginError> {
    let bytes = fetch_capped_bytes(client, url, cap).await?;
    String::from_utf8(bytes)
        .map_err(|_| PluginError::Internal(format!("{url}: response is not UTF-8")))
}

async fn fetch_capped_bytes(
    client: &Client,
    url: &str,
    cap: usize,
) -> Result<Vec<u8>, PluginError> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| PluginError::Internal(format!("fetch {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(PluginError::Internal(format!(
            "{url} returned HTTP {}",
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| PluginError::Internal(format!("read body of {url}: {e}")))?;
    if bytes.len() > cap {
        return Err(PluginError::Internal(format!(
            "{url} exceeds {cap}-byte cap (got {})",
            bytes.len()
        )));
    }
    Ok(bytes.to_vec())
}

fn extract_id(toml_text: &str) -> Result<String, PluginError> {
    let v: toml::Value =
        toml::from_str(toml_text).map_err(|e| PluginError::Internal(format!("parse toml: {e}")))?;
    v.get("plugin")
        .and_then(|p| p.get("id"))
        .and_then(|i| i.as_str())
        .map(String::from)
        .ok_or_else(|| PluginError::Internal("[plugin] id missing".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_id_finds_plugin_id() {
        let s = r#"
[plugin]
id = "acme.demo"
name = "Demo"
"#;
        assert_eq!(extract_id(s).unwrap(), "acme.demo");
    }

    #[test]
    fn extract_id_fails_when_missing() {
        let s = r#"
[plugin]
name = "Demo"
"#;
        let err = extract_id(s).unwrap_err();
        assert!(matches!(err, PluginError::Internal(_)));
    }

    #[test]
    fn extract_id_fails_on_malformed_toml() {
        let s = "this is :: not toml ==";
        let err = extract_id(s).unwrap_err();
        assert!(matches!(err, PluginError::Internal(_)));
    }

    #[test]
    fn push_note_carries_plain_line() {
        let eff = push_note("hi");
        match eff {
            Effect::PushNote { line } => {
                assert_eq!(line.spans.len(), 1);
                assert_eq!(line.spans[0].text, "hi");
            }
            _ => panic!("expected PushNote"),
        }
    }
}

#[cfg(test)]
mod http_tests {
    //! End-to-end tests for the `install` entry point that stand up a
    //! local HTTP server via `httpmock`. Gated on the static-world wasm
    //! fixture being present (the same fixture the
    //! `external_plugins.rs` integration test consumes); when absent
    //! the tests skip rather than fail.
    //!
    //! These tests live next to `install` (rather than in
    //! `tests/plugins_install.rs`) because `savvagent` is a binary
    //! crate with no public library API; integration tests cannot
    //! reach `install::install`. The `#[cfg(test)]` mod is the
    //! shortest path to coverage while keeping `install` private to
    //! its module.

    use super::*;
    use std::path::PathBuf;

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("savvagent-plugin-wasm")
            .join("tests")
            .join("fixtures")
            .join("static.wasm")
    }

    fn fixture_available() -> bool {
        let path = fixture_path();
        if !path.is_file() {
            eprintln!(
                "skipping /plugins install http test: {} missing — \
                 run `just build-fixtures` to build it",
                path.display()
            );
            return false;
        }
        true
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn install_emits_open_trust_modal_effect_on_success() {
        if !fixture_available() {
            return;
        }
        let server = httpmock::MockServer::start();
        let wasm_bytes = std::fs::read(fixture_path()).expect("read fixture");

        let wasm_mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/plugin.wasm");
            then.status(200).body(wasm_bytes.clone());
        });

        let wasm_url = format!("http://{}/plugin.wasm", server.address());
        let toml_body = format!(
            r#"[plugin]
id = "fixture.static"
name = "Fixture Static"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
wasm = "{wasm_url}"
"#
        );

        let toml_mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/plugin.toml");
            then.status(200).body(&toml_body);
        });

        let tmp = tempfile::tempdir().unwrap();
        let tmp_home_path = tmp.path().to_path_buf();
        let toml_url = format!("http://{}/plugin.toml", server.address());
        let effects = install(tmp.path(), &toml_url)
            .await
            .expect("install should succeed");

        toml_mock.assert();
        wasm_mock.assert();

        assert_eq!(effects.len(), 1, "expected one effect");
        match &effects[0] {
            Effect::OpenScreen { id, args } => {
                assert_eq!(id, "plugins.trust-modal");
                match args {
                    ScreenArgs::PluginsTrustModal {
                        id: plugin_id,
                        name,
                        version,
                        source_url,
                        hash,
                        staging_dir,
                        home_dir,
                    } => {
                        assert_eq!(plugin_id, "fixture.static");
                        assert_eq!(name, "Fixture Static");
                        assert_eq!(version, "0.1.0");
                        assert_eq!(source_url, &toml_url);
                        assert!(!hash.is_empty(), "hash must be populated");
                        assert!(staging_dir.is_dir(), "staging dir must persist");
                        assert!(
                            staging_dir.join("plugin.toml").is_file(),
                            "staging plugin.toml must exist"
                        );
                        assert!(
                            staging_dir.join("plugin.wasm").is_file(),
                            "staging plugin.wasm must exist"
                        );
                        assert_eq!(
                            home_dir, &tmp_home_path,
                            "home_dir must propagate through to the modal"
                        );
                        // Clean up the leaked tempdir manually.
                        let _ = std::fs::remove_dir_all(staging_dir);
                    }
                    other => panic!("expected PluginsTrustModal, got {other:?}"),
                }
            }
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn install_rejects_oversize_toml() {
        let server = httpmock::MockServer::start();
        let big_toml = "x".repeat(64 * 1024 + 1);
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/plugin.toml");
            then.status(200).body(big_toml);
        });
        let tmp = tempfile::tempdir().unwrap();
        let url = format!("http://{}/plugin.toml", server.address());
        let err = install(tmp.path(), &url)
            .await
            .expect_err("oversize TOML must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("65536") || msg.contains("cap"),
            "expected size-cap error, got: {msg}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn install_rejects_non_200_status() {
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/plugin.toml");
            then.status(404).body("not found");
        });
        let tmp = tempfile::tempdir().unwrap();
        let url = format!("http://{}/plugin.toml", server.address());
        let err = install(tmp.path(), &url).await.expect_err("404 must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("404") || msg.contains("HTTP"),
            "expected HTTP status error, got: {msg}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn install_rejects_manifest_without_wasm_url() {
        let server = httpmock::MockServer::start();
        let toml_body = r#"[plugin]
id = "fixture.static"
name = "Fixture Static"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#;
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/plugin.toml");
            then.status(200).body(toml_body);
        });
        let tmp = tempfile::tempdir().unwrap();
        let url = format!("http://{}/plugin.toml", server.address());
        let err = install(tmp.path(), &url)
            .await
            .expect_err("missing wasm URL must fail");
        assert!(
            err.to_string().contains("wasm"),
            "expected wasm-URL error, got: {err}"
        );
    }
}
