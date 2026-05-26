//! `internal:splash` — startup HUD + parse-error rendering, exposed as a
//! Screen. The poll-loop wiring stays in main.rs in PR 3; PR 7 replaces
//! the poll with on_event(Connect) dispatch from the host.

pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
    PluginKind, Screen, ScreenArgs, ScreenLayout, ScreenSpec, SlashSpec,
};

use screen::{CachedHud, SplashScreen};

/// Plugin wrapper for the startup HUD screen.
///
/// Holds a [`CachedHud`] that is updated via [`HostEvent::Connect`] and
/// passed to each new [`SplashScreen`] instance so render state persists
/// across open/close cycles.
pub struct SplashPlugin {
    /// Most recently received connect state, forwarded to new screen instances.
    pub last_render: Option<CachedHud>,
}

impl SplashPlugin {
    /// Create a new `SplashPlugin` with no cached connect state.
    pub fn new() -> Self {
        Self { last_render: None }
    }
}

impl Default for SplashPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for SplashPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.screens = vec![ScreenSpec {
            id: "splash".into(),
            layout: ScreenLayout::Fullscreen { hide_chrome: false },
        }];
        contributions.slash_commands = vec![SlashSpec {
            name: "splash".into(),
            summary: rust_i18n::t!("slash.splash-summary").to_string(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        contributions.hooks = vec![HookKind::HostStarting, HookKind::Connect];

        Manifest {
            id: PluginId::new("internal:splash").expect("valid built-in id"),
            name: "Splash".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.splash-description").to_string(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        Ok(vec![Effect::OpenScreen {
            id: "splash".into(),
            args: ScreenArgs::None,
        }])
    }

    fn create_screen(&self, id: &str, _args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        if id != "splash" {
            return Err(PluginError::ScreenNotFound(id.to_string()));
        }
        Ok(Box::new(SplashScreen::new(self.last_render.clone())))
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        match event {
            HostEvent::HostStarting => {
                // No-op — provider plugins fire HostEvent::Connect when
                // they bring up a client; PR 7's apply_effects emits the
                // Connect event after a successful RegisterProvider.
            }
            HostEvent::Connect { provider_id } => {
                self.last_render = Some(CachedHud {
                    connected: true,
                    last_provider: Some(provider_id),
                });
            }
            _ => {}
        }
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::ProviderId;

    /// Splash starts disconnected; receiving a `Connect` event must flip
    /// the cached HUD state so the next `SplashScreen` instance renders
    /// "Connected to <provider>." instead of the spinner.
    #[tokio::test]
    async fn splash_flips_to_connected_on_connect() {
        let mut plugin = SplashPlugin::new();
        assert!(plugin.last_render.is_none(), "starts disconnected");
        plugin
            .on_event(HostEvent::Connect {
                provider_id: ProviderId::new("anthropic").expect("valid id"),
            })
            .await
            .expect("on_event returns Ok");
        let hud = plugin
            .last_render
            .as_ref()
            .expect("HUD state populated after Connect");
        assert!(hud.connected, "connected flag must be true");
        assert_eq!(
            hud.last_provider.as_ref().map(|p| p.as_str()),
            Some("anthropic"),
            "provider id propagates from event to cached HUD"
        );
    }

    /// `HostStarting` is the no-op partner of `Connect`: it tells the
    /// plugin the host is alive but no provider has come up yet, so the
    /// HUD must stay in the spinner state.
    #[tokio::test]
    async fn splash_host_starting_does_not_flip_to_connected() {
        let mut plugin = SplashPlugin::new();
        plugin
            .on_event(HostEvent::HostStarting)
            .await
            .expect("on_event returns Ok");
        assert!(
            plugin.last_render.is_none(),
            "HostStarting alone must not mark the HUD connected"
        );
    }
}
