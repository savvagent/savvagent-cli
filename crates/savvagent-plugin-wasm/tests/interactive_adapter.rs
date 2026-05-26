//! End-to-end integration tests for the interactive-world adapter.
//!
//! Each test loads `tests/fixtures/interactive.wasm` (a real wasm
//! component built from `tests/fixtures-src/interactive`), instantiates
//! it via [`InteractiveAdapter::new`], opens a screen, and exercises one
//! [`Screen`] trait method. The fixture lives in source form alongside
//! this test so it can be rebuilt via `just build-fixtures`; the binary
//! is committed so day-to-day `cargo test` doesn't require the wasm
//! toolchain.
//!
//! Every test uses `#[tokio::test(flavor = "multi_thread")]` because
//! [`InteractiveAdapter::create_screen`] (the sync trait method on the
//! `Plugin` trait surface) bridges into async wasm calls via
//! [`tokio::task::block_in_place`], which requires a multi-thread
//! runtime.

use std::sync::Arc;

use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, KeyMods, Plugin, Region, ScreenArgs,
};
use savvagent_plugin_wasm::adapter::InteractiveAdapter;
use savvagent_plugin_wasm::host_imports::theme;
use savvagent_plugin_wasm::manifest::PluginManifest;

const PLUGIN_TOML: &str = r#"
[plugin]
id = "fixture.interactive"
name = "fixture-interactive"
version = "0.1.0"
world = "plugin-interactive"
savvagent = "^0.18"
"#;

/// Stage the fixture's `plugin.toml` and `plugin.wasm` under a temp dir.
fn stage_fixture(dir: &tempfile::TempDir) -> Arc<PluginManifest> {
    let plugin_dir = dir.path();
    std::fs::write(plugin_dir.join("plugin.toml"), PLUGIN_TOML).expect("write plugin.toml");
    let wasm_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("interactive.wasm");
    std::fs::copy(&wasm_src, plugin_dir.join("plugin.wasm")).expect("copy interactive.wasm");
    Arc::new(
        PluginManifest::load(&plugin_dir.join("plugin.toml"), "fixture.interactive")
            .expect("parse plugin.toml"),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn interactive_adapter_renders_hello() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let theme = theme::provider(Vec::new());
    let adapter = InteractiveAdapter::new(dm, tmp.path(), theme)
        .await
        .expect("InteractiveAdapter::new");

    // Use the async path explicitly — the sync `create_screen` Plugin
    // trait method would also work here, but the async path skips the
    // block_in_place bridge and keeps the test focused on the wasm flow.
    let mut screen = adapter
        .create_screen_async("test", ScreenArgs::None)
        .await
        .expect("create_screen_async");

    let lines = screen.render(Region {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    });
    assert_eq!(lines.len(), 1, "fresh screen renders exactly one line");
    assert_eq!(
        lines[0].spans.len(),
        1,
        "fresh screen line has exactly one span"
    );
    assert_eq!(lines[0].spans[0].text, "hello");

    let tips = screen.tips();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0].spans[0].text, "press any key");

    let effects = screen
        .on_key(KeyEventPortable {
            code: KeyCodePortable::Char('a'),
            modifiers: KeyMods::default(),
        })
        .await
        .expect("on_key");
    assert_eq!(effects.len(), 1, "fixture emits one PushNote per key");
    match &effects[0] {
        Effect::PushNote { line } => {
            assert_eq!(line.spans.len(), 1);
            assert_eq!(line.spans[0].text, "keyed");
        }
        other => panic!("expected PushNote, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn screen_render_after_key_reflects_new_state() {
    // The fixture increments `key_count` on every on_key and emits a
    // second styled-line `keys=<count>` when that counter is > 0. This
    // test exercises the cache-refresh-after-key path: after the first
    // on_key, the cached `render` snapshot must include the new line.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let theme = theme::provider(Vec::new());
    let adapter = InteractiveAdapter::new(dm, tmp.path(), theme)
        .await
        .expect("InteractiveAdapter::new");

    let mut screen = adapter
        .create_screen_async("test", ScreenArgs::None)
        .await
        .expect("create_screen_async");

    let region = Region {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };

    // Sanity: fresh screen has no key-count line.
    let before = screen.render(region);
    assert_eq!(before.len(), 1);

    // First key event.
    let _ = screen
        .on_key(KeyEventPortable {
            code: KeyCodePortable::Char('x'),
            modifiers: KeyMods::default(),
        })
        .await
        .expect("on_key 1");
    let after_1 = screen.render(region);
    assert_eq!(after_1.len(), 2, "after one key: hello + keys=1");
    assert_eq!(after_1[1].spans[0].text, "keys=1");

    // Second key event — counter should advance to 2.
    let _ = screen
        .on_key(KeyEventPortable {
            code: KeyCodePortable::Char('y'),
            modifiers: KeyMods {
                shift: true,
                ..Default::default()
            },
        })
        .await
        .expect("on_key 2");
    let after_2 = screen.render(region);
    assert_eq!(after_2.len(), 2);
    assert_eq!(after_2[1].spans[0].text, "keys=2");
}

#[tokio::test(flavor = "multi_thread")]
async fn interactive_adapter_create_screen_unknown_id_errors() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let theme = theme::provider(Vec::new());
    let adapter = InteractiveAdapter::new(dm, tmp.path(), theme)
        .await
        .expect("InteractiveAdapter::new");

    let result = adapter.create_screen_async("nope", ScreenArgs::None).await;
    match result {
        Err(savvagent_plugin::PluginError::ScreenNotFound(id)) => assert_eq!(id, "nope"),
        Err(other) => panic!("expected ScreenNotFound, got {other:?}"),
        Ok(_) => panic!("expected error, got Ok"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn interactive_adapter_sync_create_screen_bridges_via_block_in_place() {
    // Exercises the `Plugin::create_screen` sync trait method explicitly,
    // since that's the path the runtime actually uses (the trait surface
    // is sync-returns-Box<dyn Screen>).
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let theme = theme::provider(Vec::new());
    let adapter = InteractiveAdapter::new(dm, tmp.path(), theme)
        .await
        .expect("InteractiveAdapter::new");

    let screen = (&adapter as &dyn Plugin)
        .create_screen("test", ScreenArgs::None)
        .expect("sync create_screen via block_in_place");
    assert_eq!(screen.id(), "test");
}

#[tokio::test(flavor = "multi_thread")]
async fn interactive_adapter_manifest_caches_and_reports_runtime_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let theme = theme::provider(Vec::new());
    let adapter = InteractiveAdapter::new(dm, tmp.path(), theme)
        .await
        .expect("InteractiveAdapter::new");

    let m = adapter.manifest();
    // `<org>.<name>` ⇒ `<org>:<name>` per `convert::disk_id_to_plugin_id`.
    assert_eq!(m.id.as_str(), "fixture:interactive");
    assert_eq!(m.name, "fixture-interactive");
    assert_eq!(m.version, "0.1.0");
    assert_eq!(m.contributions.screens.len(), 1);
    assert_eq!(m.contributions.screens[0].id, "test");
}

#[tokio::test(flavor = "multi_thread")]
async fn each_screen_open_owns_its_own_state() {
    // The plan called this "per-screen-open store" — two screens opened
    // off the same adapter must not share `key_count` state.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dm = stage_fixture(&tmp);
    let theme = theme::provider(Vec::new());
    let adapter = InteractiveAdapter::new(dm, tmp.path(), theme)
        .await
        .expect("InteractiveAdapter::new");

    let mut a = adapter
        .create_screen_async("test", ScreenArgs::None)
        .await
        .expect("open a");
    let b = adapter
        .create_screen_async("test", ScreenArgs::None)
        .await
        .expect("open b");

    let region = Region {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };

    // Mutate `a` three times, leave `b` untouched.
    for _ in 0..3 {
        let _ = a
            .on_key(KeyEventPortable {
                code: KeyCodePortable::Char('a'),
                modifiers: KeyMods::default(),
            })
            .await
            .expect("a on_key");
    }
    assert_eq!(a.render(region).len(), 2);
    assert_eq!(a.render(region)[1].spans[0].text, "keys=3");
    assert_eq!(b.render(region).len(), 1, "b is still pristine");
}
