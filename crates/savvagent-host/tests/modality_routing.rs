//! Phase 4 end-to-end:
//!
//! 1. Image attachment with active provider that has a vision-capable
//!    sibling model → router redirects within the same provider with
//!    `RoutingReason::Modality { kind: Image }`. No cross-provider
//!    hop.
//! 2. Image attachment with active provider that has no vision-capable
//!    model AND no other connected providers → falls through to
//!    Default and `TurnEvent::ModalityWarning` fires.
//! 3. Image attachment with active provider that has no vision-capable
//!    model BUT another connected provider does → still falls through
//!    to Default (same-provider-only policy refuses the silent
//!    cross-provider hop) and `TurnEvent::ModalityWarning` fires.
//! 4. `@`-override pinned at a vision-incapable model + image →
//!    `RoutingReason::Override` wins; `TurnEvent::ModalityWarning`
//!    still fires so the user sees why the next call may fail.

use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
use savvagent_host::{
    Host, HostConfig, ProviderEndpoint, ProviderRegistration, RoutingReason, StartupConnectPolicy,
    TurnEvent,
};
use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ContentBlock, ImageSource, ListModelsResponse, MediaType,
    ProviderError, ProviderId, StopReason, StreamEvent,
};
use tokio::sync::{Mutex, mpsc};

/// A minimal provider that records which model it was asked to handle
/// and returns a canned `end_turn` response. One `RecordingProvider`
/// per registered `ProviderRegistration` lets each test inspect what
/// the host dispatched.
struct RecordingProvider {
    seen_model: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl ProviderClient for RecordingProvider {
    async fn complete(
        &self,
        req: CompleteRequest,
        _stream: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        *self.seen_model.lock().await = Some(req.model.clone());
        Ok(CompleteResponse {
            id: "rec-0".into(),
            model: req.model.clone(),
            content: vec![ContentBlock::Text { text: "ok".into() }],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            usage: Default::default(),
        })
    }
    async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
        Ok(ListModelsResponse {
            models: vec![],
            default_model_id: None,
        })
    }
}

/// Build a provider with one model that has the given `supports_vision`
/// value. The model id is used as the default model id.
fn caps_one(model: &str, vision: bool) -> ProviderCapabilities {
    ProviderCapabilities::new(
        vec![ModelCapabilities {
            id: model.into(),
            display_name: model.into(),
            supports_vision: vision,
            supports_audio: false,
            context_window: 0,
            cost_tier: CostTier::Standard,
        }],
        model.into(),
    )
    .expect("valid caps")
}

/// Build an anthropic-like provider with haiku (no vision) as default
/// and opus (vision) as a sibling.
fn caps_haiku_plus_opus() -> ProviderCapabilities {
    ProviderCapabilities::new(
        vec![
            ModelCapabilities {
                id: "haiku".into(),
                display_name: "Claude Haiku".into(),
                supports_vision: false,
                supports_audio: false,
                context_window: 0,
                cost_tier: CostTier::Cheap,
            },
            ModelCapabilities {
                id: "opus".into(),
                display_name: "Claude Opus".into(),
                supports_vision: true,
                supports_audio: false,
                context_window: 0,
                cost_tier: CostTier::Premium,
            },
        ],
        "haiku".into(),
    )
    .expect("valid caps")
}

fn image_blocks(prompt: &str) -> Vec<ContentBlock> {
    vec![
        ContentBlock::Text {
            text: prompt.into(),
        },
        ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: MediaType::Png,
                data: "AAAA".into(),
            },
        },
    ]
}

fn reg(
    id: &str,
    display: &str,
    caps: ProviderCapabilities,
    seen: Arc<Mutex<Option<String>>>,
) -> ProviderRegistration {
    ProviderRegistration::new(
        ProviderId::new(id).unwrap(),
        display,
        Arc::new(RecordingProvider { seen_model: seen })
            as Arc<dyn ProviderClient + Send + Sync>,
        caps,
    )
}

async fn collect_events(
    rx: &mut mpsc::Receiver<TurnEvent>,
    timeout_ms: u64,
) -> Vec<TurnEvent> {
    let mut out = Vec::new();
    while let Ok(Some(ev)) =
        tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx.recv()).await
    {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn image_input_redirects_to_same_provider_sibling_model() {
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        "Anthropic",
        caps_haiku_plus_opus(),
        Arc::clone(&a_seen),
    );

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "haiku",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;

    let host = Host::start(cfg).await.expect("host starts");
    assert_eq!(host.active_provider().await.as_str(), "anthropic");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _ = host
        .run_turn_streaming_with_blocks(image_blocks("describe this"), tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 50).await;
    let routed = events
        .iter()
        .find_map(|ev| match ev {
            TurnEvent::RouteSelected {
                provider_id,
                model_id,
                reason,
            } => Some((provider_id.clone(), model_id.clone(), reason.clone())),
            _ => None,
        })
        .expect("RouteSelected event");
    assert_eq!(routed.0.as_str(), "anthropic");
    assert_eq!(routed.1, "opus");
    assert!(matches!(routed.2, RoutingReason::Modality { .. }));

    // Mock saw "opus" — the redirect happened on the provider side.
    assert_eq!(*a_seen.lock().await, Some("opus".into()));

    // No ModalityWarning when redirect succeeds.
    let warning = events
        .iter()
        .find(|ev| matches!(ev, TurnEvent::ModalityWarning { .. }));
    assert!(warning.is_none(), "no warning when redirect succeeds");

    host.shutdown().await;
}

#[tokio::test]
async fn image_input_warns_when_active_provider_has_no_vision_and_no_others() {
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        "Anthropic",
        caps_one("haiku", false),
        Arc::clone(&a_seen),
    );

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "haiku",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;

    let host = Host::start(cfg).await.expect("host starts");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _ = host
        .run_turn_streaming_with_blocks(image_blocks("describe this"), tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 50).await;
    let routed = events
        .iter()
        .find_map(|ev| match ev {
            TurnEvent::RouteSelected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .expect("RouteSelected event");
    assert_eq!(routed, RoutingReason::Default);

    let warning = events
        .iter()
        .find_map(|ev| match ev {
            TurnEvent::ModalityWarning { message } => Some(message.clone()),
            _ => None,
        })
        .expect("ModalityWarning event");
    assert!(
        warning.contains("image") || warning.contains("vision"),
        "warning text should reference the modality; got: {warning}"
    );

    host.shutdown().await;
}

#[tokio::test]
async fn image_input_does_not_silently_cross_to_other_provider() {
    // Anthropic active, no vision. Gemini also connected with a vision
    // model. Same-provider-only policy refuses the silent hop —
    // routing falls through to Default with a warning. Phase 5's user
    // rules are the explicit cross-provider opt-in.
    let a_seen = Arc::new(Mutex::new(None));
    let g_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        "Anthropic",
        caps_one("haiku", false),
        Arc::clone(&a_seen),
    );
    let g_reg = reg(
        "gemini",
        "Gemini",
        caps_one("flash", true),
        Arc::clone(&g_seen),
    );

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "haiku",
    );
    cfg.providers = vec![a_reg, g_reg];
    cfg.startup_connect = StartupConnectPolicy::All;

    let host = Host::start(cfg).await.expect("host starts");
    assert_eq!(host.active_provider().await.as_str(), "anthropic");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _ = host
        .run_turn_streaming_with_blocks(image_blocks("describe this"), tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 50).await;
    let routed = events
        .iter()
        .find_map(|ev| match ev {
            TurnEvent::RouteSelected {
                provider_id,
                model_id,
                reason,
            } => Some((provider_id.clone(), model_id.clone(), reason.clone())),
            _ => None,
        })
        .expect("RouteSelected event");
    assert_eq!(routed.0.as_str(), "anthropic");
    assert_eq!(routed.1, "haiku");
    assert_eq!(routed.2, RoutingReason::Default);

    // Gemini's mock must not have been called.
    assert_eq!(*g_seen.lock().await, None);
    assert_eq!(*a_seen.lock().await, Some("haiku".into()));

    // Warning fires because vision was needed and the redirect failed.
    let warning = events
        .iter()
        .find(|ev| matches!(ev, TurnEvent::ModalityWarning { .. }));
    assert!(warning.is_some());

    host.shutdown().await;
}

#[tokio::test]
async fn override_wins_over_modality_and_still_warns() {
    // User typed `@anthropic:haiku <image>` — override pins a
    // vision-incapable model. RoutingReason must be Override; the
    // warning event still fires so the user sees why the next call
    // may fail.
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        "Anthropic",
        caps_haiku_plus_opus(),
        Arc::clone(&a_seen),
    );

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "opus",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;

    let host = Host::start(cfg).await.expect("host starts");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    // First block carries the @-prefix; second is the image. The
    // host's @-parser runs on the leading Text block per Task 4.
    let blocks = vec![
        ContentBlock::Text {
            text: "@anthropic:haiku describe this".into(),
        },
        ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: MediaType::Png,
                data: "AAAA".into(),
            },
        },
    ];
    let _ = host
        .run_turn_streaming_with_blocks(blocks, tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 50).await;
    let routed = events
        .iter()
        .find_map(|ev| match ev {
            TurnEvent::RouteSelected {
                provider_id,
                model_id,
                reason,
            } => Some((provider_id.clone(), model_id.clone(), reason.clone())),
            _ => None,
        })
        .expect("RouteSelected event");
    assert_eq!(routed.0.as_str(), "anthropic");
    assert_eq!(routed.1, "haiku");
    assert_eq!(routed.2, RoutingReason::Override);

    assert_eq!(*a_seen.lock().await, Some("haiku".into()));

    let warning = events
        .iter()
        .find(|ev| matches!(ev, TurnEvent::ModalityWarning { .. }));
    assert!(warning.is_some(), "warning must fire on override-no-vision");

    host.shutdown().await;
}
