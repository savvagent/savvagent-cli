//! `internal:provider-anthropic` — thin Anthropic shim.
//!
//! On `HostStarting` (and on `/connect anthropic`) the plugin attempts to
//! read an API key from the keyring; on success it constructs an
//! [`provider_anthropic::AnthropicProvider`], wraps it in
//! [`savvagent_mcp::InProcessProviderClient`] so the runtime receives a
//! `Box<dyn ProviderClient>`, and emits
//! [`savvagent_plugin::Effect::RegisterProvider`].
//!
//! No keyring entry → emit a [`savvagent_plugin::Effect::PushNote`] so the
//! user knows credentials are missing.

use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::{
    CostTier, ModelAlias, ModelCapabilities, ProviderCapabilities, ProviderRegistration,
};
use savvagent_mcp::{InProcessProviderClient, ProviderClient};
use savvagent_plugin::{
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
    PluginKind, ProviderId, ProviderSpec, Region, SlashSpec, SlotSpec, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};

use super::provider_common::BuiltinProviderPlugin;

/// Provider plugin id (used by `apply_effects` to look up the right shim
/// when a [`Effect::RegisterProvider`] arrives).
const PLUGIN_ID: &str = "internal:provider-anthropic";
/// Provider id this plugin registers.
const PROVIDER_ID: &str = "anthropic";
/// Human-readable label.
const DISPLAY_NAME: &str = "Anthropic";

/// Anthropic provider shim.
pub(crate) struct ProviderAnthropicPlugin {
    client: Option<Box<dyn ProviderClient>>,
    /// Set to `true` while this provider is the host's active provider.
    /// Updated via [`HostEvent::ActiveProviderChanged`]; used by
    /// [`render_slot`] to prepend the `▸ ` active marker.
    active: bool,
}

impl ProviderAnthropicPlugin {
    /// Construct a new shim with no client yet.
    pub(crate) fn new() -> Self {
        Self {
            client: None,
            active: false,
        }
    }

    /// Try to read the API key from the keyring and, on success, build an
    /// in-process [`ProviderClient`]. Returns `Some(())` when a client was
    /// installed, `None` when credentials were unavailable.
    ///
    /// Keyring backend errors (Secret Service daemon down, locked macOS
    /// Keychain, etc.) and provider client `BuildError`s (TLS init failure,
    /// broken proxy env vars, fd exhaustion) are surfaced via `tracing` so
    /// the user can distinguish "no credentials" from "credentials present
    /// but plumbing failed."
    fn try_connect_from_keyring(&mut self) -> Option<()> {
        if self.client.is_some() {
            // Already connected; nothing to do.
            return Some(());
        }
        let key = match crate::creds::load(PROVIDER_ID) {
            Ok(Some(k)) => k,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(provider = PROVIDER_ID, error = %e,
                    "keyring read failed; treating as missing credentials");
                return None;
            }
        };
        let provider = match provider_anthropic::AnthropicProvider::builder()
            .api_key(&key)
            .build()
        {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(provider = PROVIDER_ID, error = %e,
                    "provider client build failed despite credentials present");
                return None;
            }
        };
        let client: Box<dyn ProviderClient> =
            Box::new(InProcessProviderClient::new(Arc::new(provider)));
        self.client = Some(client);
        Some(())
    }

    /// Capability metadata for all Anthropic (Claude) models the plugin
    /// supports. Matches the model list in [`crate::providers::PROVIDERS`]
    /// and what the `/model` picker exposes.
    pub(crate) fn capabilities() -> ProviderCapabilities {
        ProviderCapabilities::new(
            vec![
                ModelCapabilities {
                    id: "claude-opus-4-7".into(),
                    display_name: "Claude Opus 4.7".into(),
                    supports_vision: true,
                    supports_audio: false,
                    context_window: 200_000,
                    cost_tier: CostTier::Premium,
                },
                ModelCapabilities {
                    id: "claude-sonnet-4-5".into(),
                    display_name: "Claude Sonnet 4.5".into(),
                    supports_vision: true,
                    supports_audio: false,
                    context_window: 200_000,
                    cost_tier: CostTier::Standard,
                },
                ModelCapabilities {
                    id: "claude-haiku-4-5".into(),
                    display_name: "Claude Haiku 4.5".into(),
                    supports_vision: true,
                    supports_audio: false,
                    context_window: 200_000,
                    cost_tier: CostTier::Cheap,
                },
                ModelCapabilities {
                    id: "claude-sonnet-4-6".into(),
                    display_name: "Claude Sonnet 4.6".into(),
                    supports_vision: true,
                    supports_audio: false,
                    context_window: 200_000,
                    cost_tier: CostTier::Standard,
                },
            ],
            "claude-haiku-4-5".into(),
        )
        .expect("static provider capabilities are valid")
    }

    /// Attempt to build a [`ProviderRegistration`] from the keyring and the
    /// plugin's static capability metadata.
    ///
    /// Returns `Ok(None)` when credentials are absent — this is not an error;
    /// the user can run `/connect anthropic` later. Returns `Ok(Some(_))` when
    /// the client was built successfully. Returns `Err(_)` for hard failures
    /// (keyring backend error or TLS init failure) that warrant a warning log.
    pub(crate) async fn try_build_registration(
        &self,
    ) -> Result<Option<ProviderRegistration>, String> {
        let key = match crate::creds::load(PROVIDER_ID) {
            Ok(Some(k)) => k,
            Ok(None) => return Ok(None),
            Err(e) => return Err(format!("keyring read: {e}")),
        };
        let provider = provider_anthropic::AnthropicProvider::builder()
            .api_key(&key)
            .build()
            .map_err(|e| format!("client build: {e}"))?;
        let client: Arc<dyn ProviderClient + Send + Sync> =
            Arc::new(InProcessProviderClient::new(Arc::new(provider)));
        Ok(Some(
            ProviderRegistration::new(
                savvagent_protocol::ProviderId::new(PROVIDER_ID)
                    .expect("PROVIDER_ID is a valid provider id"),
                DISPLAY_NAME,
                client,
                Self::capabilities(),
            )
            .with_aliases(vec![
                ModelAlias {
                    alias: "opus".into(),
                    provider: ProviderId::new("anthropic")
                        .expect("static alias provider id is valid"),
                    model: "claude-opus-4-7".into(),
                },
                ModelAlias {
                    alias: "sonnet".into(),
                    provider: ProviderId::new("anthropic")
                        .expect("static alias provider id is valid"),
                    model: "claude-sonnet-4-6".into(),
                },
                ModelAlias {
                    alias: "haiku".into(),
                    provider: ProviderId::new("anthropic")
                        .expect("static alias provider id is valid"),
                    model: "claude-haiku-4-5".into(),
                },
            ]),
        ))
    }

    /// Test-only helper that pre-installs a stub client without going
    /// through the keyring. Used by the end-to-end registry-wiring test
    /// to exercise the slash → register → take chain without touching
    /// the user's credential store.
    #[cfg(test)]
    pub(crate) fn with_test_client(client: Box<dyn ProviderClient>) -> Self {
        Self {
            client: Some(client),
            active: false,
        }
    }

    /// Test-only seam: directly set the active flag without going through
    /// `on_event`. Used by unit tests that exercise `render_slot` in
    /// isolation without wiring up the full plugin dispatch machinery.
    #[cfg(test)]
    pub(crate) fn set_active_for_render(&mut self, active: bool) {
        self.active = active;
    }
}

impl Default for ProviderAnthropicPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ProviderAnthropicPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.providers = vec![ProviderSpec {
            id: ProviderId::new(PROVIDER_ID).expect("valid provider id"),
            display_name: DISPLAY_NAME.into(),
            requires_credential: true,
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
            priority: 100,
        }];
        contributions.hooks = vec![HookKind::HostStarting, HookKind::ActiveProviderChanged];

        Manifest {
            id: PluginId::new(PLUGIN_ID).expect("valid built-in id"),
            name: DISPLAY_NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Anthropic provider (Claude family)".into(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        let rekey = args.iter().any(|a| a == "--rekey");
        if !rekey && self.try_connect_from_keyring().is_some() {
            // Stored key worked; register without opening the modal.
            return Ok(vec![Effect::RegisterProvider {
                id: ProviderId::new(PROVIDER_ID).expect("valid"),
                display_name: DISPLAY_NAME.into(),
            }]);
        }
        // No stored key, --rekey explicitly requested, or stored key
        // didn't yield a working client: open the modal.
        Ok(vec![Effect::PromptApiKey {
            provider_id: ProviderId::new(PROVIDER_ID).expect("valid"),
        }])
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        match event {
            HostEvent::HostStarting => {
                if self.try_connect_from_keyring().is_some() {
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

impl BuiltinProviderPlugin for ProviderAnthropicPlugin {
    fn take_client(&mut self) -> Option<Box<dyn ProviderClient>> {
        self.client.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no keyring entry available (CI default), `/connect anthropic`
    /// must emit [`Effect::PromptApiKey`] so the runtime opens the
    /// masked input — not a dead-end note telling the user to do
    /// what they just did.
    #[tokio::test]
    #[serial_test::serial]
    async fn no_creds_emits_prompt_api_key() {
        rust_i18n::set_locale("en");
        // Best-effort: clear any existing entry so the keyring read returns
        // `Ok(None)` on platforms that have a backend. On CI (no backend),
        // load() returns Ok(None) regardless.
        let _ = keyring::Entry::new("savvagent", PROVIDER_ID).map(|e| e.delete_credential());

        let mut p = ProviderAnthropicPlugin::new();
        let effs = p.handle_slash("connect anthropic", vec![]).await.unwrap();
        match &effs[0] {
            Effect::PromptApiKey { provider_id } => {
                assert_eq!(provider_id.as_str(), PROVIDER_ID);
            }
            other => panic!("expected PromptApiKey, got {other:?}"),
        }
        rust_i18n::set_locale("en");
    }

    /// `/connect anthropic` with a stored key must NOT emit
    /// `Effect::PromptApiKey`; it must instead emit `RegisterProvider`
    /// immediately via the keyring path.
    #[tokio::test]
    #[serial_test::serial]
    async fn handle_slash_with_stored_key_skips_modal() {
        rust_i18n::set_locale("en");

        // Install a stored key for the duration of the test.
        let _ = keyring::Entry::new("savvagent", PROVIDER_ID).map(|e| e.set_password("test-key"));

        let mut p = ProviderAnthropicPlugin::new();
        let effs = p.handle_slash("connect anthropic", vec![]).await.unwrap();
        let saw_prompt = effs
            .iter()
            .any(|e| matches!(e, Effect::PromptApiKey { .. }));
        assert!(
            !saw_prompt,
            "with a stored key, /connect must not open the modal; got effects: {effs:?}"
        );
        let saw_register = effs.iter().any(
            |e| matches!(e, Effect::RegisterProvider { id, .. } if id.as_str() == PROVIDER_ID),
        );
        assert!(
            saw_register,
            "must register the provider silently; got effects: {effs:?}"
        );

        // Cleanup.
        let _ = keyring::Entry::new("savvagent", PROVIDER_ID).map(|e| e.delete_credential());
        rust_i18n::set_locale("en");
    }

    /// `--rekey` must open the API-key modal even when the plugin already
    /// has a constructed client, letting the user update their key.
    #[tokio::test]
    #[serial_test::serial]
    async fn handle_slash_with_rekey_flag_opens_modal_even_when_client_exists() {
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

        let mut p = ProviderAnthropicPlugin::with_test_client(Box::new(StubClient));
        let effs = p
            .handle_slash("connect anthropic", vec!["--rekey".into()])
            .await
            .unwrap();
        assert!(
            effs.iter()
                .any(|e| matches!(e, Effect::PromptApiKey { .. }))
        );
        rust_i18n::set_locale("en");
    }

    #[test]
    fn manifest_declares_provider_and_slash() {
        let p = ProviderAnthropicPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), PLUGIN_ID);
        assert_eq!(m.contributions.providers.len(), 1);
        assert_eq!(m.contributions.providers[0].id.as_str(), PROVIDER_ID);
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == format!("connect {PROVIDER_ID}"))
        );
        assert!(m.contributions.hooks.contains(&HookKind::HostStarting));
    }

    #[test]
    fn render_slot_returns_empty_when_disconnected() {
        let p = ProviderAnthropicPlugin::new();
        let lines = p.render_slot(
            "home.footer.left",
            Region {
                x: 0,
                y: 0,
                width: 80,
                height: 1,
            },
        );
        assert!(lines.is_empty());
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

        let mut p = ProviderAnthropicPlugin::with_test_client(Box::new(StubClient));
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
