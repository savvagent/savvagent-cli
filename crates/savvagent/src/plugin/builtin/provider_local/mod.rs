//! `internal:provider-local` — keyless Ollama shim.
//!
//! Unlike the other provider shims, this one needs no credentials. On
//! `HostStarting` (and on `/connect local`) the plugin builds an in-process
//! [`savvagent_mcp::ProviderClient`] and emits
//! [`savvagent_plugin::Effect::RegisterProvider`]. The health-check is a
//! no-op; failures surface on the first turn.

use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::{CostTier, ModelCapabilities, ProviderCapabilities, ProviderRegistration};
use savvagent_mcp::{InProcessProviderClient, ProviderClient};
use savvagent_plugin::{
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
    PluginKind, ProviderId, ProviderSpec, Region, SlashSpec, SlotSpec, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};

use super::provider_common::{BuiltinProviderPlugin, build_dynamic_caps};

const PLUGIN_ID: &str = "internal:provider-local";
const PROVIDER_ID: &str = "local";
const DISPLAY_NAME: &str = "Local (Ollama)";

/// Local (Ollama) provider shim. Keyless.
pub(crate) struct ProviderLocalPlugin {
    client: Option<Box<dyn ProviderClient>>,
    /// Sticky bit set when a previous build attempt failed; the slash and
    /// hook re-entry then take the "endpoint unreachable" branch instead of
    /// repeatedly retrying a known-bad build and emitting `RegisterProvider`
    /// for a dead client. Cleared on a successful build, so `/connect local`
    /// after the user starts `ollama serve` can succeed.
    last_build_failed: bool,
    /// Set to `true` while this provider is the host's active provider.
    active: bool,
}

impl ProviderLocalPlugin {
    /// Construct a new shim with no client yet.
    pub(crate) fn new() -> Self {
        Self {
            client: None,
            last_build_failed: false,
            active: false,
        }
    }

    /// Test-only helper that pre-installs a stub client without going
    /// through the local provider build path.
    #[cfg(test)]
    pub(crate) fn with_test_client(client: Box<dyn ProviderClient>) -> Self {
        Self {
            client: Some(client),
            last_build_failed: false,
            active: false,
        }
    }

    /// Test-only seam: directly set the active flag without going through
    /// `on_event`.
    #[cfg(test)]
    pub(crate) fn set_active_for_render(&mut self, active: bool) {
        self.active = active;
    }

    /// Capability metadata for the local (Ollama) provider. The actual
    /// model list is discovered at runtime from `ollama list`; we expose
    /// a single representative entry so the picker always has something to
    /// display before the first turn.
    pub(crate) fn capabilities() -> ProviderCapabilities {
        ProviderCapabilities::new(
            vec![ModelCapabilities {
                id: "llama3.2".into(),
                display_name: "Llama 3.2 (default)".into(),
                supports_vision: false,
                supports_audio: false,
                context_window: 128_000,
                cost_tier: CostTier::Free,
            }],
            "llama3.2".into(),
        )
        .expect("static provider capabilities are valid")
    }

    /// Attempt to build a [`ProviderRegistration`] from the local Ollama
    /// endpoint. Unlike the cloud providers, no keyring lookup is needed;
    /// the call always tries to build a client. Returns `Ok(None)` when the
    /// builder fails (Ollama not running) — not an error, the user can start
    /// `ollama serve` and run `/connect local` later.
    pub(crate) async fn try_build_registration(
        &self,
    ) -> Result<Option<(ProviderRegistration, Option<String>)>, String> {
        let provider = match provider_local::OllamaProvider::builder().build() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "ollama provider build failed at startup");
                // Treat a failed build as "not available" rather than a hard
                // error — Ollama might simply not be running yet.
                return Ok(None);
            }
        };
        let client: Arc<dyn ProviderClient + Send + Sync> =
            Arc::new(InProcessProviderClient::new(Arc::new(provider)));
        // Prefer the live /api/tags catalog so the picker shows the
        // models the user has actually pulled. Falls back to the
        // single-entry placeholder when Ollama isn't reachable or has
        // no models pulled yet.
        let (caps, note) =
            build_dynamic_caps(client.as_ref(), Self::capabilities(), DISPLAY_NAME).await;
        let reg = ProviderRegistration::new(
            savvagent_protocol::ProviderId::new(PROVIDER_ID)
                .expect("PROVIDER_ID is a valid provider id"),
            DISPLAY_NAME,
            client,
            caps,
        );
        Ok(Some((reg, note)))
    }

    /// Try to construct an in-process Ollama client. Returns `Some(())` on
    /// success.
    ///
    /// A previously-failed build short-circuits this call. The user can
    /// retry via `/connect local`, which clears the sticky bit on success
    /// once `ollama serve` is reachable.
    fn try_connect_local(&mut self) -> Option<()> {
        if self.client.is_some() {
            return Some(());
        }
        if self.last_build_failed {
            // A prior call set the sticky bit; surface "endpoint unreachable"
            // again rather than spinning the builder per slash/hook re-entry.
            return None;
        }
        let provider = match provider_local::OllamaProvider::builder().build() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "ollama provider build failed");
                self.last_build_failed = true;
                return None;
            }
        };
        let client: Box<dyn ProviderClient> =
            Box::new(InProcessProviderClient::new(Arc::new(provider)));
        self.client = Some(client);
        self.last_build_failed = false;
        Some(())
    }

    /// Test-only: clear the sticky-failure bit. Lets unit tests drive
    /// the same plugin through "fail then succeed" sequences if the
    /// underlying builder is ever made injectable.
    #[cfg(test)]
    #[allow(dead_code)]
    fn reset_last_build_failed(&mut self) {
        self.last_build_failed = false;
    }
}

impl Default for ProviderLocalPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ProviderLocalPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.providers = vec![ProviderSpec {
            id: ProviderId::new(PROVIDER_ID).expect("valid provider id"),
            display_name: DISPLAY_NAME.into(),
            requires_credential: false,
            in_process: true,
        }];
        contributions.slash_commands = vec![SlashSpec {
            name: format!("connect {PROVIDER_ID}"),
            summary: format!("Connect to {DISPLAY_NAME}"),
            args_hint: None,
            requires_arg: false,
        }];
        contributions.slots = vec![SlotSpec {
            slot_id: "home.footer.left".into(),
            priority: 130,
        }];
        contributions.hooks = vec![HookKind::HostStarting, HookKind::ActiveProviderChanged];

        Manifest {
            id: PluginId::new(PLUGIN_ID).expect("valid built-in id"),
            name: DISPLAY_NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Local provider via Ollama (keyless)".into(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(&mut self, _: &str, _: Vec<String>) -> Result<Vec<Effect>, PluginError> {
        if self.try_connect_local().is_some() {
            return Ok(vec![Effect::RegisterProvider {
                id: ProviderId::new(PROVIDER_ID).expect("valid"),
                display_name: DISPLAY_NAME.into(),
            }]);
        }
        Ok(vec![Effect::PushNote {
            line: StyledLine::plain(format!(
                "{DISPLAY_NAME} endpoint not reachable. Start `ollama serve` and try again."
            )),
        }])
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        match event {
            HostEvent::HostStarting => {
                if self.try_connect_local().is_some() {
                    return Ok(vec![Effect::RegisterProvider {
                        id: ProviderId::new(PROVIDER_ID).expect("valid"),
                        display_name: DISPLAY_NAME.into(),
                    }]);
                }
                Ok(vec![])
            }
            HostEvent::ActiveProviderChanged { ref id } => {
                self.active = id.as_str() == PROVIDER_ID;
                Ok(vec![])
            }
            _ => Ok(vec![]),
        }
    }

    fn render_slot(&self, slot_id: &str, _: Region) -> Vec<StyledLine> {
        if slot_id != "home.footer.left" {
            return vec![];
        }
        if self.client.is_some() {
            let mods = TextMods {
                bold: true,
                ..TextMods::default()
            };
            let prefix = if self.active { "\u{25b8} " } else { "  " };
            vec![StyledLine {
                spans: vec![StyledSpan {
                    text: format!("{prefix}{DISPLAY_NAME}"),
                    fg: Some(ThemeColor::Success),
                    bg: None,
                    modifiers: mods,
                }],
            }]
        } else {
            vec![]
        }
    }
}

impl BuiltinProviderPlugin for ProviderLocalPlugin {
    fn take_client(&mut self) -> Option<Box<dyn ProviderClient>> {
        self.client.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Local is keyless, so the "no-creds" path doesn't apply. `/connect local`
    /// builds a client and emits `RegisterProvider` when Ollama is reachable,
    /// or `PushNote` when the build fails.
    #[tokio::test]
    async fn connect_emits_register_or_push_note() {
        let mut p = ProviderLocalPlugin::new();
        let effs = p.handle_slash("connect local", vec![]).await.unwrap();
        assert_eq!(effs.len(), 1);
        match &effs[0] {
            Effect::RegisterProvider { id, .. } => {
                assert_eq!(id.as_str(), PROVIDER_ID);
            }
            Effect::PushNote { .. } => {
                // Builder failed (very rare — only on misconfigured envs).
            }
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn manifest_marks_provider_keyless() {
        let p = ProviderLocalPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), PLUGIN_ID);
        assert!(!m.contributions.providers[0].requires_credential);
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == format!("connect {PROVIDER_ID}"))
        );
    }

    #[test]
    fn render_slot_marks_active_provider() {
        rust_i18n::set_locale("en");
        use async_trait::async_trait;
        use savvagent_mcp::ProviderClient;
        use savvagent_protocol::{
            CompleteRequest, CompleteResponse, ListModelsResponse, ProviderError, StreamEvent,
        };
        use tokio::sync::mpsc;

        struct StubClient;
        #[async_trait]
        impl ProviderClient for StubClient {
            async fn complete(
                &self,
                _: CompleteRequest,
                _: Option<mpsc::Sender<StreamEvent>>,
            ) -> Result<CompleteResponse, ProviderError> {
                unreachable!()
            }
            async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
                unreachable!()
            }
        }

        let mut p = ProviderLocalPlugin::with_test_client(Box::new(StubClient));
        p.set_active_for_render(true);
        let lines = p.render_slot(
            "home.footer.left",
            Region {
                x: 0,
                y: 0,
                width: 80,
                height: 1,
            },
        );
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect::<Vec<_>>()
            .join("");
        assert!(
            joined.starts_with('\u{25b8}'),
            "active marker missing in: {joined}"
        );

        p.set_active_for_render(false);
        let lines = p.render_slot(
            "home.footer.left",
            Region {
                x: 0,
                y: 0,
                width: 80,
                height: 1,
            },
        );
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect::<Vec<_>>()
            .join("");
        assert!(
            joined.starts_with("  "),
            "inactive prefix missing in: {joined}"
        );
        rust_i18n::set_locale("en");
    }
}
