//! Integration tests for the public `discover()` API — round-trip a
//! realistic plugin directory tree through the four-path walker.

use savvagent_plugin_wasm::discovery::{SourceScope, discover};

#[test]
fn empty_paths_returns_empty() {
    let d = discover(None, None);
    assert!(d.plugins.is_empty());
    assert!(d.warnings.is_empty());
}

#[test]
fn user_scope_when_project_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let plugin_dir = home.join(".savvagent/plugins/acme.demo");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#,
    )
    .unwrap();

    let d = discover(None, Some(home));
    assert_eq!(d.plugins.len(), 1);
    assert_eq!(d.plugins[0].source_scope, SourceScope::UserSavvagent);
    assert_eq!(d.plugins[0].manifest.plugin.id, "acme.demo");
}

#[test]
fn user_claude_scope_when_only_claude_present() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let plugin_dir = home.join(".claude/plugins/acme.demo");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        r#"
[plugin]
id = "acme.demo"
name = "Demo"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#,
    )
    .unwrap();

    let d = discover(None, Some(home));
    assert_eq!(d.plugins.len(), 1);
    assert_eq!(d.plugins[0].source_scope, SourceScope::UserClaude);
}

#[test]
fn multiple_distinct_ids_all_returned() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    for id in ["acme.one", "acme.two", "acme.three"] {
        let plugin_dir = home.join(".savvagent/plugins").join(id);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            format!(
                r#"
[plugin]
id = "{id}"
name = "{id}"
version = "0.1.0"
world = "plugin-static"
savvagent = "^0.18"
"#
            ),
        )
        .unwrap();
    }

    let d = discover(None, Some(home));
    let mut ids: Vec<String> = d
        .plugins
        .into_iter()
        .map(|p| p.manifest.plugin.id)
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["acme.one", "acme.three", "acme.two"]);
}

#[test]
fn non_dir_entries_ignored() {
    // A file (not directory) sitting in the plugins root must not be
    // mistaken for a plugin candidate.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let plugins_root = home.join(".savvagent/plugins");
    std::fs::create_dir_all(&plugins_root).unwrap();
    std::fs::write(plugins_root.join("README.md"), b"not a plugin").unwrap();

    let d = discover(None, Some(home));
    assert!(d.plugins.is_empty());
    assert!(d.warnings.is_empty());
}
