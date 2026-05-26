//! Sanity test: every WIT file in the crate parses cleanly with `wit-parser`,
//! and the canonical interfaces (`types`, `spp`) plus the three world
//! skeletons (`plugin-static`, `plugin-interactive`, `plugin-provider`)
//! resolve in a single `savvagent:plugin@0.1.0` package, with each world
//! exporting the contract the host adapters depend on.
//!
//! This guards against (a) accidental syntactic regressions in any `.wit`
//! file and (b) the multi-file directory drifting out of single-package
//! shape — Task 2 generates host bindings off the same `wit/` tree via
//! `wasmtime::component::bindgen!`, which requires the parse to succeed.
//!
//! Task 2 extension: the basic "package + world names" assertions are
//! complemented by per-world export checks. The planned
//! `tests/world_validates.rs` was merged into this file rather than added
//! as a sibling because it would have duplicated the resolve+package
//! lookup and produced two near-identical failure surfaces. See the Task 2
//! commit message for the merge rationale.

use std::path::PathBuf;

fn load() -> wit_parser::Resolve {
    let wit_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("wit");
    let mut resolve = wit_parser::Resolve::default();
    resolve
        .push_dir(&wit_dir)
        .expect("wit/ must contain parseable .wit files");
    resolve
}

#[test]
fn package_namespace_and_interfaces() {
    let resolve = load();
    // Exactly one package — the directory is single-package on purpose.
    let pkg = resolve
        .packages
        .iter()
        .next()
        .map(|(_, p)| p)
        .expect("at least one package");
    assert_eq!(
        pkg.name.namespace, "savvagent",
        "package namespace must stay `savvagent`",
    );
    assert_eq!(
        pkg.name.name, "plugin",
        "package name must stay `plugin` (spp interface inlined per spp.wit's reconciliation note)",
    );

    // Both top-level interfaces must be present and resolvable.
    assert!(
        pkg.interfaces.contains_key("types"),
        "shared.wit must declare interface `types`",
    );
    assert!(
        pkg.interfaces.contains_key("spp"),
        "spp.wit must declare interface `spp`",
    );

    // All three world skeletons must resolve.
    for world in ["plugin-static", "plugin-interactive", "plugin-provider"] {
        assert!(
            pkg.worlds.contains_key(world),
            "world `{world}` must resolve from its skeleton file",
        );
    }
}

/// Look up a world by name and assert the export and import key sets match
/// the contract Tasks 4–6 will adapt against. We compare on the *export
/// key strings* the parser exposes (function/interface name) so a rename
/// is caught even if the type signature drifts.
fn assert_world_shape(world_name: &str, expected_exports: &[&str], expected_imports: &[&str]) {
    let resolve = load();
    let (_, pkg) = resolve
        .packages
        .iter()
        .next()
        .expect("at least one package");
    let world_id = pkg.worlds[world_name];
    let world = &resolve.worlds[world_id];

    let exports: std::collections::BTreeSet<String> = world
        .exports
        .iter()
        .map(|(key, _item)| resolve.name_world_key(key))
        .collect();
    let imports: std::collections::BTreeSet<String> = world
        .imports
        .iter()
        .map(|(key, _item)| resolve.name_world_key(key))
        .collect();

    for ex in expected_exports {
        assert!(
            exports.contains(*ex),
            "world `{world_name}` is missing export `{ex}`; found exports = {exports:?}",
        );
    }
    for im in expected_imports {
        assert!(
            imports.contains(*im),
            "world `{world_name}` is missing import `{im}`; found imports = {imports:?}",
        );
    }
}

#[test]
fn plugin_static_world_shape() {
    assert_world_shape(
        "plugin-static",
        &[
            "manifest",
            "handle-slash",
            "on-event",
            "render-slot",
            "themes",
        ],
        &["log", "current-theme"],
    );
}

#[test]
fn plugin_interactive_world_shape() {
    // The interactive world is content-only: the host paints chrome around
    // the styled lines `render`/`tips` return on the `screen-instance`
    // resource. There are no draw-* host imports — that design was rejected
    // because it would have required `unsafe` pointer plumbing for no gain
    // over the existing sync-returns-Vec<StyledLine> `Screen` trait. See
    // `wit/plugin-interactive.wit`'s top-level comment for the rationale.
    assert_world_shape(
        "plugin-interactive",
        // `create-screen` and the per-instance methods are inside the
        // `screens` interface (they all share the `screen-instance`
        // resource and the bindgen requires the resource and the
        // constructor to live in the same interface to avoid an
        // import/export boundary crossing). `manifest` stays at world
        // level as a plain function export.
        &["manifest", "savvagent:plugin/screens@0.1.0"],
        &["log", "current-theme"],
    );
}

#[test]
fn plugin_provider_world_shape() {
    // Interface imports are surfaced under their fully-qualified
    // `<namespace>:<package>/<interface>@<version>` form, while inline
    // function imports keep their plain name.
    assert_world_shape(
        "plugin-provider",
        &["manifest", "complete", "list-models", "count-tokens"],
        &[
            "log",
            "savvagent:plugin/http-capability@0.1.0",
            "savvagent:plugin/keyring-capability@0.1.0",
            "savvagent:plugin/progress-capability@0.1.0",
        ],
    );
}
