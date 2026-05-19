//! End-to-end heuristic-classifier integration tests.
//!
//! Three scenarios:
//!
//! 1. **Short factoid** — heuristics=true, active=opus (Premium). A short
//!    `?`-bearing turn routes to haiku (Cheap) with badge `Heuristic(short)`.
//! 2. **Coding keyword** — heuristics=true, active=haiku (Cheap). A
//!    "refactor"-bearing turn routes to opus (Premium) with badge
//!    `Heuristic(coding)`.
//! 3. **Heuristic off** — heuristics=false. The same inputs route to the
//!    active model with `RoutingReason::Default`.

use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
use savvagent_host::{
    HeuristicKind, Host, HostConfig, ProviderEndpoint, ProviderRegistration, RoutingReason,
    StartupConnectPolicy, TurnEvent,
};
use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ContentBlock, ListModelsResponse, ProviderError, ProviderId,
    StopReason, StreamEvent,
};
use tokio::sync::{Mutex, mpsc};

struct StubProvider {
    seen_model: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl ProviderClient for StubProvider {
    async fn complete(
        &self,
        req: CompleteRequest,
        _stream: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        *self.seen_model.lock().await = Some(req.model.clone());
        Ok(CompleteResponse {
            id: "stub-0".into(),
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

fn caps_haiku_opus(default: &str) -> ProviderCapabilities {
    ProviderCapabilities::new(
        vec![
            ModelCapabilities {
                id: "claude-haiku-4-5".into(),
                display_name: "Claude Haiku 4.5".into(),
                supports_vision: false,
                supports_audio: false,
                context_window: 0,
                cost_tier: CostTier::Cheap,
            },
            ModelCapabilities {
                id: "claude-opus-4-7".into(),
                display_name: "Claude Opus 4.7".into(),
                supports_vision: false,
                supports_audio: false,
                context_window: 0,
                cost_tier: CostTier::Premium,
            },
        ],
        default.into(),
    )
    .expect("valid caps")
}

fn write_routing_toml(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("routing.toml");
    let mut f = std::fs::File::create(&path).expect("create routing.toml");
    f.write_all(content.as_bytes()).expect("write");
    (dir, path)
}

fn reg(
    id: &str,
    caps: ProviderCapabilities,
    seen: Arc<Mutex<Option<String>>>,
) -> ProviderRegistration {
    ProviderRegistration::new(
        ProviderId::new(id).expect("valid provider id"),
        id,
        Arc::new(StubProvider { seen_model: seen }) as Arc<dyn ProviderClient + Send + Sync>,
        caps,
    )
}

async fn collect_events(rx: &mut mpsc::Receiver<TurnEvent>, timeout_ms: u64) -> Vec<TurnEvent> {
    let mut out = Vec::new();
    while let Ok(Some(ev)) =
        tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx.recv()).await
    {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn heuristic_short_factoid_routes_to_cheap_model() {
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        caps_haiku_opus("claude-opus-4-7"),
        Arc::clone(&a_seen),
    );

    let (_dir, path) = write_routing_toml("heuristics = true\n");

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "claude-opus-4-7",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;
    cfg.routing_rules_path = Some(path);

    let host = Host::start(cfg).await.expect("host starts");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _ = host
        .run_turn_streaming("what is 2+2?", tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 200).await;
    let saw_heuristic = events.iter().any(|ev| {
        matches!(
            ev,
            TurnEvent::RouteSelected {
                reason: RoutingReason::Heuristic { kind: HeuristicKind::ShortFactoid },
                model_id,
                ..
            } if model_id == "claude-haiku-4-5"
        )
    });
    assert!(
        saw_heuristic,
        "expected Heuristic(short) → haiku; got {events:?}"
    );

    assert_eq!(
        a_seen.lock().await.as_deref(),
        Some("claude-haiku-4-5"),
        "the provider should have been invoked with the cheap model"
    );

    host.shutdown().await;
}

#[tokio::test]
async fn heuristic_coding_routes_to_premium_model() {
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        caps_haiku_opus("claude-haiku-4-5"),
        Arc::clone(&a_seen),
    );

    let (_dir, path) = write_routing_toml("heuristics = true\n");

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "claude-haiku-4-5",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;
    cfg.routing_rules_path = Some(path);

    let host = Host::start(cfg).await.expect("host starts");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _ = host
        .run_turn_streaming("please refactor this function", tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 200).await;
    let saw_heuristic = events.iter().any(|ev| {
        matches!(
            ev,
            TurnEvent::RouteSelected {
                reason: RoutingReason::Heuristic { kind: HeuristicKind::Coding },
                model_id,
                ..
            } if model_id == "claude-opus-4-7"
        )
    });
    assert!(
        saw_heuristic,
        "expected Heuristic(coding) → opus; got {events:?}"
    );

    assert_eq!(
        a_seen.lock().await.as_deref(),
        Some("claude-opus-4-7"),
        "the provider should have been invoked with the premium model"
    );

    host.shutdown().await;
}

#[tokio::test]
async fn heuristic_off_falls_through_to_default() {
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        caps_haiku_opus("claude-opus-4-7"),
        Arc::clone(&a_seen),
    );

    // No routing.toml ⇒ heuristics defaults to false.
    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "claude-opus-4-7",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;
    cfg.routing_rules_path = None;

    let host = Host::start(cfg).await.expect("host starts");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _ = host
        .run_turn_streaming("what is 2+2?", tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 200).await;
    let saw_default = events.iter().any(|ev| {
        matches!(
            ev,
            TurnEvent::RouteSelected {
                reason: RoutingReason::Default,
                model_id,
                ..
            } if model_id == "claude-opus-4-7"
        )
    });
    assert!(
        saw_default,
        "expected Default → opus (heuristics off); got {events:?}"
    );

    host.shutdown().await;
}
