//! End-to-end: with a temp HOME containing a trusted static plugin,
//! `register_external` surfaces it as a `Box<dyn Plugin>` with the right
//! manifest id.
//!
//! ## Why this lives in the savvagent crate's tests/, not plugin-wasm's
//!
//! Task 9's contract is "external plugins land in the TUI's registry
//! alongside built-ins." The plugin-wasm crate's own `register.rs`
//! unit tests cover the trust-state matrix (Ok / Untrusted /
//! HashMismatch / Disabled); this file proves the layer above also
//! works — i.e. a real `plugin-static` wasm component, properly
//! trusted, makes it all the way through discovery → trust check →
//! `StaticAdapter::new` → `Box<dyn Plugin>` and exposes the manifest
//! the registry would key off of.
//!
//! ## Fixture handling
//!
//! The test depends on the static-world wasm fixture committed at
//! `crates/savvagent-plugin-wasm/tests/fixtures/static.wasm`. If the
//! fixture is absent (CI image without the binary, fresh clone before
//! `just build-fixtures`), the test gracefully skips rather than
//! failing — the lower-level adapter tests still exercise the same
//! fixture on the same boxes.

use std::sync::Arc;

use savvagent_plugin::Plugin;
use savvagent_plugin_wasm::host_imports::theme;
use savvagent_plugin_wasm::register::register_external;
use savvagent_plugin_wasm::trust::{TrustFile, tree_hash};

/// Path to the static-world wasm fixture, resolved relative to this
/// integration test's `CARGO_MANIFEST_DIR` (i.e. `crates/savvagent/`).
/// Mirrors the lookup used by the plugin-wasm crate's own integration
/// tests so both paths read the same artifact.
fn fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("savvagent-plugin-wasm")
        .join("tests")
        .join("fixtures")
        .join("static.wasm")
}

/// Skip the test cleanly when the wasm fixture isn't present. Returns
/// `true` when the test should proceed.
fn fixture_available() -> bool {
    let path = fixture_path();
    if !path.is_file() {
        eprintln!(
            "skipping external-plugins integration test: {} missing — \
             run `just build-fixtures` to build it",
            path.display()
        );
        return false;
    }
    true
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_static_plugin_appears_in_registered_plugins() {
    if !fixture_available() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path();

    // Stage a plugin under the user-scope `.savvagent/plugins/` tier.
    // The directory name must match the manifest id; the fixture's
    // built-in `manifest()` export returns `fixture.static`, but the
    // discovery layer keys off `plugin.toml`'s id (which we set to
    // match the directory name).
    let plugin_id = "fixture.static";
    let plugin_dir = home.join(".savvagent/plugins").join(plugin_id);
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        format!(
            r#"
[plugin]
id = "{plugin_id}"
name = "Fixture Static"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#,
        ),
    )
    .expect("write plugin.toml");
    std::fs::copy(fixture_path(), plugin_dir.join("plugin.wasm")).expect("copy fixture");

    // Pre-trust the on-disk content. Without this the discovery layer
    // would surface an "untrusted" warning and skip the plugin —
    // exactly the path covered by `register_external`'s own unit
    // tests, not the one this integration test cares about.
    let hash = tree_hash(&plugin_dir).expect("tree_hash");
    let mut trust = TrustFile::default();
    trust.trust(plugin_id, hash, None);
    trust.save(home).expect("trust save");

    // Run register_external against the synthesized HOME. No project
    // root — we only want to exercise the user-tier path.
    let theme = theme::provider(Vec::new());
    let result = register_external(None, home, theme)
        .await
        .expect("register");

    assert!(
        result.warnings.is_empty(),
        "expected no warnings, got: {:?}",
        result.warnings
    );
    assert_eq!(
        result.plugins.len(),
        1,
        "expected exactly one external plugin"
    );
    assert!(
        result.provider_clients.is_empty(),
        "static plugin must not surface as a provider client"
    );

    // The adapter must round-trip the manifest id back. This is the
    // hook the TUI's `PluginRegistry::new` uses to key the
    // slash/render/hook dispatch maps, so if this works the registry
    // wiring works.
    //
    // Note: disk-side ids use the `<org>.<name>` separator; the runtime
    // `PluginId` shape uses `<vendor>:<rest>` (matches built-ins like
    // `internal:themes`). The conversion lives in
    // `savvagent_plugin_wasm::convert::disk_id_to_plugin_id`.
    let manifest = result.plugins[0].manifest();
    let runtime_id = plugin_id.replacen('.', ":", 1);
    assert_eq!(manifest.id.as_str(), runtime_id);

    // And the adapter is a real `Box<dyn Plugin>` — Send + Sync, can
    // be wrapped in Arc<Mutex<_>> exactly like a built-in. We don't
    // need to instantiate the registry here; the type-level assertion
    // below proves the surface is compatible.
    let _adapter: Arc<tokio::sync::Mutex<dyn Plugin>> = {
        // Drain the result into the Arc-wrapping pattern the registry
        // uses. If this compiles, the trait-object surface lines up.
        let mut plugins = result.plugins;
        Arc::new(tokio::sync::Mutex::new(BoxedPluginBridge(
            plugins.remove(0),
        )))
    };
}

/// Thin newtype that mirrors the registry's internal `BoxedPlugin`
/// adapter — it lets us wrap a `Box<dyn Plugin>` in `Arc<Mutex<dyn Plugin>>`
/// without the registry's `pub(crate)` machinery. We only assert the
/// type round-trips; no method calls.
struct BoxedPluginBridge(Box<dyn Plugin>);

#[async_trait::async_trait]
impl Plugin for BoxedPluginBridge {
    fn manifest(&self) -> savvagent_plugin::Manifest {
        self.0.manifest()
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<savvagent_plugin::Effect>, savvagent_plugin::PluginError> {
        self.0.handle_slash(name, args).await
    }

    async fn on_event(
        &mut self,
        evt: savvagent_plugin::HostEvent,
    ) -> Result<Vec<savvagent_plugin::Effect>, savvagent_plugin::PluginError> {
        self.0.on_event(evt).await
    }

    fn render_slot(
        &self,
        slot_id: &str,
        region: savvagent_plugin::Region,
    ) -> Vec<savvagent_plugin::StyledLine> {
        self.0.render_slot(slot_id, region)
    }
}

/// A plugin discovered on disk but never trusted is surfaced as a
/// warning and skipped — the registry should never see it. Locks in
/// the "untrusted plugins don't sneak in" contract.
#[tokio::test(flavor = "multi_thread")]
async fn untrusted_static_plugin_is_skipped_with_warning() {
    if !fixture_available() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path();

    let plugin_id = "fixture.static";
    let plugin_dir = home.join(".savvagent/plugins").join(plugin_id);
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        format!(
            r#"
[plugin]
id = "{plugin_id}"
name = "Fixture Static"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#,
        ),
    )
    .expect("write plugin.toml");
    std::fs::copy(fixture_path(), plugin_dir.join("plugin.wasm")).expect("copy fixture");

    // Note: no trust entry. The plugin must be skipped.
    let theme = theme::provider(Vec::new());
    let result = register_external(None, home, theme)
        .await
        .expect("register");

    assert!(
        result.plugins.is_empty(),
        "untrusted plugin must not appear in plugins"
    );
    assert_eq!(result.warnings.len(), 1, "exactly one warning expected");
    assert!(
        result.warnings[0].contains("untrusted"),
        "warning must explain why the plugin was skipped: {}",
        result.warnings[0]
    );
}
