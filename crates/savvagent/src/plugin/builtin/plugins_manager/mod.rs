//! `internal:plugins-manager` — enable/disable Optional plugins.
//!
//! The slash command `/plugins` opens the [`PluginsManagerScreen`] modal;
//! the runtime populates its row list from the registry + manifests after
//! the empty screen is pushed (see `apply_effects::open_screen`). Toggles
//! flow back through [`Effect::TogglePlugin`], which the runtime persists
//! to `~/.savvagent/plugins.toml`.

pub mod install;
pub mod persistence;
pub mod screen;
pub mod trust_modal;

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec,
};
use savvagent_plugin_wasm::trust::TrustFile;

use install::push_note;
use screen::PluginsManagerScreen;
use trust_modal::PluginsTrustModal;

/// Core plugin exposing `/plugins` and the manager modal.
pub struct PluginsManagerPlugin;

impl PluginsManagerPlugin {
    /// Construct a new `PluginsManagerPlugin`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for PluginsManagerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for PluginsManagerPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "plugins".into(),
            summary: rust_i18n::t!("slash.plugins-summary").to_string(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        contributions.screens = vec![
            ScreenSpec {
                id: "plugins.manager".into(),
                layout: ScreenLayout::CenteredModal {
                    width_pct: 80,
                    height_pct: 80,
                    title: Some(rust_i18n::t!("picker.plugins-manager.modal-title").to_string()),
                },
            },
            ScreenSpec {
                id: "plugins.trust-modal".into(),
                layout: ScreenLayout::CenteredModal {
                    width_pct: 70,
                    height_pct: 60,
                    title: Some(
                        rust_i18n::t!("picker.plugins-trust-modal.modal-title").to_string(),
                    ),
                },
            },
        ];

        Manifest {
            id: PluginId::new("internal:plugins-manager").expect("valid built-in id"),
            name: "Plugins manager".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.plugins-manager-description").to_string(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        // The runtime ignores `ScreenArgs::PluginsManager`'s body and fills
        // the row list via apply_effects::open_screen, so we don't need to
        // pre-fetch anything for the bare `/plugins` form.
        if name != "plugins" {
            return Ok(vec![]);
        }
        let home = match dirs::home_dir() {
            Some(h) => h,
            None => {
                return Ok(vec![push_note(
                    "/plugins: could not resolve $HOME; aborting",
                )]);
            }
        };
        match args.first().map(String::as_str) {
            None | Some("list") => Ok(vec![Effect::OpenScreen {
                id: "plugins.manager".into(),
                args: ScreenArgs::PluginsManager,
            }]),
            Some("install") => match args.get(1) {
                Some(url) => install::install(&home, url).await,
                None => Ok(vec![push_note("usage: /plugins install <plugin.toml URL>")]),
            },
            Some("trust") => trust_cmd(&args, &home),
            Some("revoke") => revoke_cmd(&args, &home),
            Some("remove") => remove_cmd(&args, &home),
            Some("enable") => enable_cmd(&args, &home),
            Some("disable") => disable_cmd(&args, &home),
            Some(unknown) => Ok(vec![push_note(format!(
                "unknown /plugins subcommand: {unknown}"
            ))]),
        }
    }

    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        match (id, args) {
            ("plugins.manager", _) => {
                // The screen needs a populated row list, but the plugin
                // instance has no read access to the registry.
                // `apply_effects::open_screen` calls back into the registry
                // after we return and replaces this empty screen with one
                // populated via `PluginsManagerScreen::with_rows`.
                Ok(Box::new(PluginsManagerScreen::empty()))
            }
            (
                "plugins.trust-modal",
                ScreenArgs::PluginsTrustModal {
                    id: pid,
                    name,
                    version,
                    source_url,
                    hash,
                    staging_dir,
                    home_dir,
                },
            ) => Ok(Box::new(PluginsTrustModal::new(
                pid,
                name,
                version,
                source_url,
                hash,
                staging_dir,
                home_dir,
            ))),
            (other, _) => Err(PluginError::ScreenNotFound(other.to_string())),
        }
    }
}

/// `/plugins trust <id>` — flip an existing record back to `trusted = true`
/// without changing its hash. No-op if the id has no record (we don't
/// invent one because we'd have to fabricate a hash).
fn trust_cmd(args: &[String], home: &Path) -> Result<Vec<Effect>, PluginError> {
    let Some(id) = args.get(1) else {
        return Ok(vec![push_note("usage: /plugins trust <id>")]);
    };
    let mut tf = load_trust(home)?;
    let Some(rec) = tf.plugins.get_mut(id) else {
        return Ok(vec![push_note(format!(
            "/plugins trust: no record for {id}; run `/plugins install <url>` first"
        ))]);
    };
    rec.trusted = true;
    rec.disabled_reason.clear();
    save_trust(home, &tf)?;
    Ok(vec![push_note(format!(
        "/plugins trust: {id} re-trusted; restart to apply"
    ))])
}

/// `/plugins revoke <id>` — remove the trust record entirely. The on-disk
/// plugin files stay where they are; only the consent ledger is touched.
fn revoke_cmd(args: &[String], home: &Path) -> Result<Vec<Effect>, PluginError> {
    let Some(id) = args.get(1) else {
        return Ok(vec![push_note("usage: /plugins revoke <id>")]);
    };
    let mut tf = load_trust(home)?;
    tf.revoke(id);
    save_trust(home, &tf)?;
    Ok(vec![push_note(format!(
        "/plugins revoke: {id} trust revoked; restart to apply"
    ))])
}

/// `/plugins remove <id>` — revoke + delete the plugin directory.
fn remove_cmd(args: &[String], home: &Path) -> Result<Vec<Effect>, PluginError> {
    let Some(id) = args.get(1) else {
        return Ok(vec![push_note("usage: /plugins remove <id>")]);
    };
    let mut tf = load_trust(home)?;
    tf.revoke(id);
    save_trust(home, &tf)?;
    // Try both user-scope locations. We don't touch project-scope dirs
    // because they're checked in; the user can delete those directly.
    let candidates: [PathBuf; 2] = [
        home.join(".savvagent/plugins").join(id),
        home.join(".claude/plugins").join(id),
    ];
    let mut removed_any = false;
    let mut errs: Vec<String> = Vec::new();
    for cand in &candidates {
        if cand.exists() {
            match std::fs::remove_dir_all(cand) {
                Ok(()) => removed_any = true,
                Err(e) => errs.push(format!("{}: {e}", cand.display())),
            }
        }
    }
    let mut note = format!("/plugins remove: {id} trust revoked");
    if removed_any {
        note.push_str("; directory deleted");
    } else if errs.is_empty() {
        note.push_str("; no on-disk directory found");
    }
    if !errs.is_empty() {
        note.push_str(&format!("; errors: {}", errs.join(", ")));
    }
    Ok(vec![push_note(note)])
}

/// `/plugins enable <id>` — clear `disabled_reason` on an existing record.
fn enable_cmd(args: &[String], home: &Path) -> Result<Vec<Effect>, PluginError> {
    let Some(id) = args.get(1) else {
        return Ok(vec![push_note("usage: /plugins enable <id>")]);
    };
    let mut tf = load_trust(home)?;
    if !tf.plugins.contains_key(id) {
        return Ok(vec![push_note(format!(
            "/plugins enable: no record for {id}"
        ))]);
    }
    tf.clear_disabled(id);
    save_trust(home, &tf)?;
    Ok(vec![push_note(format!(
        "/plugins enable: {id} enabled; restart to apply"
    ))])
}

/// `/plugins disable <id>` — set a non-empty `disabled_reason`.
fn disable_cmd(args: &[String], home: &Path) -> Result<Vec<Effect>, PluginError> {
    let Some(id) = args.get(1) else {
        return Ok(vec![push_note("usage: /plugins disable <id>")]);
    };
    let mut tf = load_trust(home)?;
    if !tf.plugins.contains_key(id) {
        return Ok(vec![push_note(format!(
            "/plugins disable: no record for {id}"
        ))]);
    }
    tf.set_disabled(id, "user-disabled");
    save_trust(home, &tf)?;
    Ok(vec![push_note(format!(
        "/plugins disable: {id} disabled; restart to apply"
    ))])
}

fn load_trust(home: &Path) -> Result<TrustFile, PluginError> {
    TrustFile::load(home).map_err(|e| PluginError::Internal(format!("trust file: {e}")))
}

fn save_trust(home: &Path, tf: &TrustFile) -> Result<(), PluginError> {
    tf.save(home)
        .map_err(|e| PluginError::Internal(format!("trust file: {e}")))
}

/// Classify a plugin id as built-in vs. external.
///
/// All [`PluginId`] values are validated to contain at least one `:` separator
/// (see `PluginId::new`), so on the runtime side both built-ins and
/// wasm-discovered plugins share the same shape: `<vendor>:<rest>`.
/// Built-ins reserve the `internal` vendor prefix; everything else is an
/// external (wasm) plugin.
///
/// The wasm side stores ids on disk as `<vendor>.<rest>` (per
/// `plugin.toml`) and `disk_id_to_plugin_id` rewrites that to
/// `<vendor>:<rest>` at registration time. By the time a row reaches the
/// plugins-manager screen the id is already in runtime form, so a single
/// vendor-prefix check is sufficient.
pub(crate) fn is_external_id(id: &PluginId) -> bool {
    !id.as_str().starts_with("internal:")
}

/// Build a short human-readable summary of a plugin's contributions, used
/// in the plugins-manager row label. Stable wording across releases so
/// the manager screen feels consistent.
pub(crate) fn summarize_contributions(contributions: &savvagent_plugin::Contributions) -> String {
    let mut parts: Vec<String> = Vec::new();
    let slash_n = contributions.slash_commands.len();
    if slash_n > 0 {
        parts.push(format!(
            "{slash_n} slash{}",
            if slash_n == 1 { "" } else { "es" }
        ));
    }
    let screen_n = contributions.screens.len();
    if screen_n > 0 {
        parts.push(format!(
            "{screen_n} screen{}",
            if screen_n == 1 { "" } else { "s" }
        ));
    }
    let theme_n = contributions.themes.len();
    if theme_n > 0 {
        parts.push(format!(
            "{theme_n} theme{}",
            if theme_n == 1 { "" } else { "s" }
        ));
    }
    let provider_n = contributions.providers.len();
    if provider_n > 0 {
        parts.push(format!(
            "{provider_n} provider{}",
            if provider_n == 1 { "" } else { "s" }
        ));
    }
    let hook_n = contributions.hooks.len();
    if hook_n > 0 {
        parts.push(format!(
            "{hook_n} hook{}",
            if hook_n == 1 { "" } else { "s" }
        ));
    }
    let slot_n = contributions.slots.len();
    if slot_n > 0 {
        parts.push(format!(
            "{slot_n} slot{}",
            if slot_n == 1 { "" } else { "s" }
        ));
    }
    let kb_n = contributions.keybindings.len();
    if kb_n > 0 {
        parts.push(format!(
            "{kb_n} keybinding{}",
            if kb_n == 1 { "" } else { "s" }
        ));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handle_slash_opens_manager_screen() {
        let mut p = PluginsManagerPlugin::new();
        let effs = p.handle_slash("plugins", vec![]).await.unwrap();
        match &effs[0] {
            Effect::OpenScreen { id, args } => {
                assert_eq!(id, "plugins.manager");
                assert!(matches!(args, ScreenArgs::PluginsManager));
            }
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    #[test]
    fn manifest_declares_screen_and_slash() {
        let p = PluginsManagerPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:plugins-manager");
        assert!(matches!(m.kind, PluginKind::Core));
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == "plugins")
        );
        assert!(
            m.contributions
                .screens
                .iter()
                .any(|s| s.id == "plugins.manager")
        );
    }

    #[test]
    fn create_screen_returns_empty_screen_for_id() {
        let p = PluginsManagerPlugin::new();
        let s = p
            .create_screen("plugins.manager", ScreenArgs::PluginsManager)
            .expect("screen created");
        assert_eq!(s.id(), "plugins.manager");
    }

    #[test]
    fn create_screen_rejects_unknown_id() {
        let p = PluginsManagerPlugin::new();
        // `dyn Screen` lacks a Debug impl, so we can't `.unwrap_err()`.
        match p.create_screen("not-mine", ScreenArgs::None) {
            Ok(_) => panic!("expected ScreenNotFound, got Ok(_)"),
            Err(PluginError::ScreenNotFound(s)) => assert_eq!(s, "not-mine"),
            Err(other) => panic!("expected ScreenNotFound, got {other:?}"),
        }
    }

    #[test]
    fn summarize_contributions_lists_each_populated_field() {
        let mut c = Contributions::default();
        c.slash_commands = vec![savvagent_plugin::SlashSpec {
            name: "x".into(),
            summary: "".into(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        c.screens = vec![savvagent_plugin::ScreenSpec {
            id: "x".into(),
            layout: ScreenLayout::Fullscreen { hide_chrome: false },
        }];
        let s = summarize_contributions(&c);
        assert!(s.contains("1 slash"));
        assert!(s.contains("1 screen"));
    }

    #[test]
    fn summarize_contributions_handles_empty() {
        let s = summarize_contributions(&Contributions::default());
        assert_eq!(s, "");
    }

    /// `is_external_id` returns `false` only when the id's vendor prefix is
    /// exactly `internal`; every other vendor (including ones that happen
    /// to *contain* the substring "internal" elsewhere in the id) is
    /// external. This pins both the happy paths and the obvious
    /// almost-collisions so a future refactor of the prefix scheme can't
    /// silently re-classify wasm plugins as built-ins.
    #[test]
    fn is_external_id_distinguishes_internal_from_vendor_prefix() {
        // Built-ins.
        let b1 = PluginId::new("internal:home-footer").expect("valid");
        let b2 = PluginId::new("internal:plugins-manager").expect("valid");
        assert!(!is_external_id(&b1));
        assert!(!is_external_id(&b2));

        // External wasm plugins (runtime form after disk_id_to_plugin_id).
        let e1 = PluginId::new("acme:demo").expect("valid");
        let e2 = PluginId::new("contoso:my-plugin").expect("valid");
        assert!(is_external_id(&e1));
        assert!(is_external_id(&e2));

        // Obvious almost-collisions: a vendor that *contains* "internal"
        // as a substring but isn't the exact prefix must remain external.
        let almost = PluginId::new("internal-corp:thing").expect("valid");
        assert!(is_external_id(&almost));
    }

    fn extract_note(eff: &Effect) -> String {
        match eff {
            Effect::PushNote { line } => line.spans.iter().map(|s| s.text.clone()).collect(),
            other => panic!("expected PushNote, got {other:?}"),
        }
    }

    #[test]
    fn revoke_cmd_with_no_id_returns_usage_note() {
        let tmp = tempfile::tempdir().unwrap();
        let effs = revoke_cmd(&["revoke".to_string()], tmp.path()).expect("revoke_cmd no panic");
        assert!(extract_note(&effs[0]).contains("usage: /plugins revoke"));
    }

    #[test]
    fn revoke_cmd_removes_trust_record_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // Seed a trust record.
        let mut tf = TrustFile::default();
        tf.trust("acme.demo", "deadbeef".into(), None);
        tf.save(home).unwrap();
        let effs = revoke_cmd(&["revoke".to_string(), "acme.demo".to_string()], home).unwrap();
        assert!(extract_note(&effs[0]).contains("acme.demo"));
        let tf2 = TrustFile::load(home).unwrap();
        assert!(!tf2.plugins.contains_key("acme.demo"));
    }

    #[test]
    fn disable_cmd_sets_disabled_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let mut tf = TrustFile::default();
        tf.trust("acme.demo", "deadbeef".into(), None);
        tf.save(home).unwrap();
        let effs = disable_cmd(&["disable".to_string(), "acme.demo".to_string()], home).unwrap();
        assert!(extract_note(&effs[0]).contains("disabled"));
        let tf2 = TrustFile::load(home).unwrap();
        assert!(
            !tf2.plugins
                .get("acme.demo")
                .unwrap()
                .disabled_reason
                .is_empty()
        );
    }

    #[test]
    fn enable_cmd_clears_disabled_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let mut tf = TrustFile::default();
        tf.trust("acme.demo", "deadbeef".into(), None);
        tf.set_disabled("acme.demo", "user-disabled");
        tf.save(home).unwrap();
        let effs = enable_cmd(&["enable".to_string(), "acme.demo".to_string()], home).unwrap();
        assert!(extract_note(&effs[0]).contains("enabled"));
        let tf2 = TrustFile::load(home).unwrap();
        assert!(
            tf2.plugins
                .get("acme.demo")
                .unwrap()
                .disabled_reason
                .is_empty()
        );
    }

    #[test]
    fn remove_cmd_revokes_trust_and_deletes_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let plugin_dir = home.join(".savvagent/plugins/acme.demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.toml"), b"x = 1").unwrap();
        let mut tf = TrustFile::default();
        tf.trust("acme.demo", "deadbeef".into(), None);
        tf.save(home).unwrap();
        let effs = remove_cmd(&["remove".to_string(), "acme.demo".to_string()], home).unwrap();
        let note = extract_note(&effs[0]);
        assert!(note.contains("acme.demo"));
        assert!(note.contains("directory deleted"));
        assert!(!plugin_dir.exists());
        let tf2 = TrustFile::load(home).unwrap();
        assert!(!tf2.plugins.contains_key("acme.demo"));
    }

    #[test]
    fn trust_cmd_re_enables_existing_record() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let mut tf = TrustFile::default();
        tf.trust("acme.demo", "deadbeef".into(), None);
        // Simulate a revoked record by flipping `trusted` off.
        tf.plugins.get_mut("acme.demo").unwrap().trusted = false;
        tf.set_disabled("acme.demo", "user-disabled");
        tf.save(home).unwrap();

        let effs = trust_cmd(&["trust".to_string(), "acme.demo".to_string()], home).unwrap();
        assert!(extract_note(&effs[0]).contains("re-trusted"));
        let tf2 = TrustFile::load(home).unwrap();
        let rec = tf2.plugins.get("acme.demo").unwrap();
        assert!(rec.trusted);
        assert!(rec.disabled_reason.is_empty());
    }

    #[test]
    fn trust_cmd_with_unknown_id_emits_helpful_note() {
        let tmp = tempfile::tempdir().unwrap();
        let effs = trust_cmd(&["trust".to_string(), "no.such".to_string()], tmp.path()).unwrap();
        assert!(extract_note(&effs[0]).contains("no record"));
    }

    #[tokio::test]
    async fn handle_slash_install_without_url_emits_usage() {
        let mut p = PluginsManagerPlugin::new();
        let effs = p
            .handle_slash("plugins", vec!["install".to_string()])
            .await
            .unwrap();
        assert!(extract_note(&effs[0]).contains("usage: /plugins install"));
    }

    #[tokio::test]
    async fn handle_slash_unknown_subcommand_emits_note() {
        let mut p = PluginsManagerPlugin::new();
        let effs = p
            .handle_slash("plugins", vec!["zonk".to_string()])
            .await
            .unwrap();
        assert!(extract_note(&effs[0]).contains("unknown /plugins subcommand: zonk"));
    }

    #[tokio::test]
    async fn handle_slash_list_subcommand_opens_manager() {
        let mut p = PluginsManagerPlugin::new();
        let effs = p
            .handle_slash("plugins", vec!["list".to_string()])
            .await
            .unwrap();
        match &effs[0] {
            Effect::OpenScreen { id, args } => {
                assert_eq!(id, "plugins.manager");
                assert!(matches!(args, ScreenArgs::PluginsManager));
            }
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    /// Confirm the plural-form branches: two slashes + two screens + one
    /// theme. Pins the exact wording so a future refactor that drops
    /// the pluralization (e.g. "2 slash") is caught here, not via UI
    /// review. Singular forms are already covered by
    /// `summarize_contributions_lists_each_populated_field`.
    #[test]
    fn summarize_contributions_pluralizes_correctly() {
        let mut c = Contributions::default();
        c.slash_commands = vec![
            savvagent_plugin::SlashSpec {
                name: "a".into(),
                summary: "".into(),
                args_hint: None,
                requires_arg: false,
                suppress_prompt_segments: vec![],
            },
            savvagent_plugin::SlashSpec {
                name: "b".into(),
                summary: "".into(),
                args_hint: None,
                requires_arg: false,
                suppress_prompt_segments: vec![],
            },
        ];
        c.screens = vec![
            savvagent_plugin::ScreenSpec {
                id: "a".into(),
                layout: ScreenLayout::Fullscreen { hide_chrome: false },
            },
            savvagent_plugin::ScreenSpec {
                id: "b".into(),
                layout: ScreenLayout::Fullscreen { hide_chrome: false },
            },
        ];
        let s = summarize_contributions(&c);
        assert!(s.contains("2 slashes"), "got: {s}");
        assert!(s.contains("2 screens"), "got: {s}");
    }
}
