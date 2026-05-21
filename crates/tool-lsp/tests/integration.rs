//! End-to-end tool-lsp tests against the fake-lsp fixture.
//!
//! These don't go through MCP; they exercise the dispatch layer
//! directly (config → pool → session → convert). Wiring the rmcp
//! server surface itself is covered by manual smoke tests until we
//! have a host-side harness that also speaks MCP back to a child.
//!
//! The fake-lsp binary path is resolved by shelling out to `cargo
//! build` for the fixture's manifest and then probing the workspace's
//! target directory. Cargo doesn't expose `CARGO_BIN_EXE_<name>` for
//! binaries owned by `path` dev-dependencies (only for binaries owned
//! by the crate-under-test), so we explicitly build the fixture from
//! the test itself. Same approach as
//! `crates/savvagent-host/tests/resources_integration.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use tempfile::tempdir;
use tool_lsp::{LspConfig, LspPool};

/// Build the `fake-lsp` fixture binary (idempotent) and return its
/// absolute path. We can't rely on Cargo's `CARGO_BIN_EXE_<name>` env
/// var — that's only set for binaries owned by the crate-under-test,
/// not for binaries owned by a `path` dev-dependency — so we shell out
/// to `cargo build` ourselves. After a successful build, the binary
/// lives at `<workspace_target>/debug/fake-lsp` (or the platform
/// `.exe` equivalent), which we locate by walking up from the running
/// test executable's directory.
fn fake_lsp_bin() -> PathBuf {
    // CARGO_MANIFEST_DIR points at `crates/tool-lsp`. The fixture
    // manifest is at `<manifest_dir>/tests/fixtures/fake-lsp/Cargo.toml`.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_manifest = manifest_dir
        .join("tests")
        .join("fixtures")
        .join("fake-lsp")
        .join("Cargo.toml");
    assert!(
        fixture_manifest.exists(),
        "fixture manifest must exist at {fixture_manifest:?}"
    );

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(&cargo)
        .args(["build", "--quiet", "--bin", "fake-lsp", "--manifest-path"])
        .arg(&fixture_manifest)
        .status()
        .expect("invoke cargo build for fake-lsp fixture");
    assert!(status.success(), "cargo build for fake-lsp failed");

    // Locate the freshly-built binary. The test binary lives at
    // `<target>/debug/deps/integration-<hash>`; the fixture's binary
    // lives at `<target>/debug/fake-lsp` (the workspace shares a
    // single target dir across all bins).
    let test_bin = std::env::current_exe().expect("current_exe");
    let target_debug = ancestor_named(&test_bin, "debug").unwrap_or_else(|| {
        panic!(
            "current_exe path {test_bin:?} must have a `debug` ancestor; \
             test layouts other than the standard `target/debug/deps/<bin>` \
             aren't supported here"
        )
    });
    let bin_name = if cfg!(windows) {
        "fake-lsp.exe"
    } else {
        "fake-lsp"
    };
    let fixture_bin = target_debug.join(bin_name);
    assert!(
        fixture_bin.exists(),
        "fake-lsp binary must exist at {fixture_bin:?} after cargo build"
    );
    fixture_bin
}

/// Return the first ancestor of `path` whose final component equals
/// `name`. Used to climb out of `target/debug/deps/` up to
/// `target/debug/`.
fn ancestor_named(path: &Path, name: &str) -> Option<PathBuf> {
    path.ancestors()
        .find(|p| p.file_name().and_then(|s| s.to_str()) == Some(name))
        .map(PathBuf::from)
}

/// Write an `lsp.toml` into `dir` pointing the `rust` language at the
/// freshly-built fake-lsp binary, and return the config path.
fn write_config(dir: &std::path::Path) -> PathBuf {
    let cfg = dir.join("lsp.toml");
    let bin = fake_lsp_bin().display().to_string();
    // Use Debug formatting (`{:?}`) on the path string so backslashes
    // and spaces are escaped into a valid TOML basic-string literal.
    fs::write(
        &cfg,
        format!(
            r#"
[[language]]
id = "rust"
extensions = ["rs"]
root_markers = ["Cargo.toml"]
command = {bin:?}
args = []
"#
        ),
    )
    .unwrap();
    cfg
}

/// Build a minimal Rust project (Cargo.toml + src/lib.rs) inside
/// `dir` and return its canonicalized root. Canonicalizing matches
/// the discipline `tools::definition::resolve_inside_root` applies:
/// on macOS, `/tmp` is a symlink to `/private/tmp`, so without this
/// the resolved-path/starts_with(root) check would fail.
fn build_project(dir: &std::path::Path) -> PathBuf {
    let project = dir.join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    fs::write(project.join("src/lib.rs"), "fn main() {}\n").unwrap();
    project.canonicalize().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn definition_returns_translated_location() {
    let dir = tempdir().unwrap();
    let project = build_project(dir.path());

    let cfg_path = write_config(dir.path());
    let cfg = LspConfig::load(&cfg_path, None).unwrap();
    let pool = LspPool::default();

    let on_diagnostics: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_| {});
    let out = tool_lsp::tools::definition::dispatch(
        tool_lsp::tools::definition::LspDefinitionInput {
            path: "src/lib.rs".into(),
            line: 0,
            character: 3,
        },
        &cfg,
        &pool,
        &project,
        on_diagnostics,
    )
    .await
    .unwrap();
    assert_eq!(out.locations.len(), 1);
    assert_eq!(out.locations[0].path, "src/lib.rs");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn references_returns_one_location() {
    let dir = tempdir().unwrap();
    let project = build_project(dir.path());

    let cfg_path = write_config(dir.path());
    let cfg = LspConfig::load(&cfg_path, None).unwrap();
    let pool = LspPool::default();
    let on_diagnostics: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_| {});

    let out = tool_lsp::tools::references::dispatch(
        tool_lsp::tools::references::LspReferencesInput {
            path: "src/lib.rs".into(),
            line: 0,
            character: 3,
            include_declaration: true,
        },
        &cfg,
        &pool,
        &project,
        on_diagnostics,
    )
    .await
    .unwrap();
    assert_eq!(out.locations.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnostics_are_observed_after_initialize() {
    let dir = tempdir().unwrap();
    let project = build_project(dir.path());

    let cfg_path = write_config(dir.path());
    let cfg = LspConfig::load(&cfg_path, None).unwrap();
    let pool = LspPool::default();

    let observed: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_clone = Arc::clone(&observed);
    let on_diagnostics: Arc<dyn Fn(&str) + Send + Sync> =
        Arc::new(move |uri: &str| observed_clone.lock().unwrap().push(uri.to_string()));

    // Spawn a session by issuing any tool call; definition is fine.
    let _ = tool_lsp::tools::definition::dispatch(
        tool_lsp::tools::definition::LspDefinitionInput {
            path: "src/lib.rs".into(),
            line: 0,
            character: 0,
        },
        &cfg,
        &pool,
        &project,
        on_diagnostics,
    )
    .await
    .unwrap();

    // Give the read loop a beat to process the fixture's
    // publishDiagnostics. The notification arrives asynchronously
    // after the `initialized` notification is sent, which races with
    // the definition response.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let log = observed.lock().unwrap();
    assert!(
        log.iter().any(|u| u.ends_with("/src/lib.rs")),
        "on_diagnostics must have been invoked for the fixture's lib.rs URI; observed: {log:?}"
    );
}
