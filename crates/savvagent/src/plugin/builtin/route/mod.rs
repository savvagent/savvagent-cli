//! `internal:route` — manage user routing rules from `~/.savvagent/routing.toml`.
//!
//! Two subcommands:
//! - `/route reload` → `Effect::ReloadRoutingRules`
//! - `/route show`   → `Effect::ShowRoutingRules`
//! - `/route` (bare) → same as `show` (parity with `/sandbox` no-args = status).

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, SlashSpec,
    StyledLine,
};

/// Plugin that registers the `/route` slash command.
pub struct RoutePlugin;

impl RoutePlugin {
    /// Construct a new [`RoutePlugin`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for RoutePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for RoutePlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "route".into(),
            summary: rust_i18n::t!("slash.route-summary").to_string(),
            args_hint: Some("[reload | show]".into()),
            requires_arg: false,
        }];
        Manifest {
            id: PluginId::new("internal:route").expect("valid built-in id"),
            name: "Routing rules".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.route-description").to_string(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        if name != "route" {
            return Ok(vec![]);
        }
        let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
        match sub {
            "" | "show" => Ok(vec![Effect::ShowRoutingRules]),
            "reload" => Ok(vec![Effect::ReloadRoutingRules]),
            other => {
                let msg = rust_i18n::t!("routing.route-usage").to_string();
                Ok(vec![Effect::PushNote {
                    line: StyledLine::plain(format!("{msg} (got `{other}`)")),
                }])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bare_route_is_show() {
        let mut p = RoutePlugin::new();
        let effs = p.handle_slash("route", vec![]).await.unwrap();
        assert_eq!(effs.len(), 1);
        assert!(matches!(effs[0], Effect::ShowRoutingRules));
    }

    #[tokio::test]
    async fn route_show_emits_show_effect() {
        let mut p = RoutePlugin::new();
        let effs = p.handle_slash("route", vec!["show".into()]).await.unwrap();
        assert!(matches!(effs[0], Effect::ShowRoutingRules));
    }

    #[tokio::test]
    async fn route_reload_emits_reload_effect() {
        let mut p = RoutePlugin::new();
        let effs = p
            .handle_slash("route", vec!["reload".into()])
            .await
            .unwrap();
        assert!(matches!(effs[0], Effect::ReloadRoutingRules));
    }

    #[tokio::test]
    async fn unknown_subcommand_emits_usage_note() {
        let mut p = RoutePlugin::new();
        let effs = p.handle_slash("route", vec!["wat".into()]).await.unwrap();
        assert!(matches!(effs[0], Effect::PushNote { .. }));
    }
}
