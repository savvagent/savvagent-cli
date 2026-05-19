//! End-to-end routing-rules integration tests.
//!
//! Three scenarios:
//!
//! 1. **Rule fires** — host with anthropic + gemini, `routing.toml` with
//!    one keyword rule pointing at gemini, run a streaming turn whose
//!    user text matches; assert `TurnEvent::RouteSelected` carries
//!    `RoutingReason::Rule { name: "to-gemini" }` and gemini's stub
//!    received the call with the named model.
//! 2. **Skip-disconnected** — same rule shape but only anthropic
//!    connected; the matched rule's provider is missing so routing
//!    falls through to `RoutingReason::Default` and anthropic's stub
//!    handles the call.
//! 3. **Reload-mid-turn race** — concurrent loop of
//!    `run_turn_streaming` reads vs `reload_routing_rules` writes; the
//!    test asserts no panic and no deadlock under contention.

use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
use savvagent_host::{
    Host, HostConfig, ProviderEndpoint, ProviderRegistration, RoutingReason, StartupConnectPolicy,
    TurnEvent,
};
use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ContentBlock, ListModelsResponse, ProviderError, ProviderId,
    StopReason, StreamEvent,
};
use tokio::sync::{Mutex, mpsc};

/// Minimal provider stub: records the model it was asked to handle and
/// returns a canned `end_turn` response. Mirrors the recording stub in
/// `tests/modality_routing.rs`.
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

/// Build a provider with a single non-vision model that is also the
/// default.
fn caps_one(model: &str) -> ProviderCapabilities {
    ProviderCapabilities::new(
        vec![ModelCapabilities {
            id: model.into(),
            display_name: model.into(),
            supports_vision: false,
            supports_audio: false,
            context_window: 0,
            cost_tier: CostTier::Standard,
        }],
        model.into(),
    )
    .expect("valid caps")
}

fn write_routing_toml(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("routing.toml");
    let mut f = std::fs::File::create(&path).expect("create routing.toml");
    f.write_all(content.as_bytes()).expect("write routing.toml");
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
async fn rule_fires_and_routes_to_named_provider() {
    let a_seen = Arc::new(Mutex::new(None));
    let g_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        caps_one("claude-haiku-4-5"),
        Arc::clone(&a_seen),
    );
    let g_reg = reg("gemini", caps_one("gemini-2.0-flash"), Arc::clone(&g_seen));

    let (_dir, path) = write_routing_toml(
        r#"
[[rule]]
name = "to-gemini"
match = { keywords = ["refactor"] }
use = "gemini/gemini-2.0-flash"
"#,
    );

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "claude-haiku-4-5",
    );
    cfg.providers = vec![a_reg, g_reg];
    cfg.startup_connect = StartupConnectPolicy::All;
    cfg.routing_rules_path = Some(path);

    let host = Host::start(cfg).await.expect("host starts");

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let _ = host
        .run_turn_streaming("please refactor this", tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 200).await;
    let saw_rule = events.iter().any(|ev| {
        matches!(
            ev,
            TurnEvent::RouteSelected {
                reason: RoutingReason::Rule { name },
                ..
            } if name == "to-gemini"
        )
    });
    assert!(
        saw_rule,
        "expected RouteSelected with reason=Rule(to-gemini); got {events:?}"
    );

    let routed = events
        .iter()
        .find_map(|ev| match ev {
            TurnEvent::RouteSelected {
                provider_id,
                model_id,
                ..
            } => Some((provider_id.clone(), model_id.clone())),
            _ => None,
        })
        .expect("RouteSelected event present");
    assert_eq!(routed.0.as_str(), "gemini");
    assert_eq!(routed.1, "gemini-2.0-flash");

    // Gemini's stub saw the dispatched model; anthropic's stub did not.
    assert_eq!(
        g_seen.lock().await.as_deref(),
        Some("gemini-2.0-flash"),
        "gemini stub should have been invoked with the rule's target model"
    );
    assert_eq!(
        *a_seen.lock().await,
        None,
        "anthropic stub must not be invoked when the rule routes elsewhere"
    );

    host.shutdown().await;
}

#[tokio::test]
async fn rule_with_disconnected_provider_falls_through() {
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        caps_one("claude-haiku-4-5"),
        Arc::clone(&a_seen),
    );

    let (_dir, path) = write_routing_toml(
        r#"
[[rule]]
name = "to-gemini-disconnected"
match = { keywords = ["refactor"] }
use = "gemini/gemini-2.0-flash"
"#,
    );

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
        .run_turn_streaming("please refactor", tx)
        .await
        .expect("turn completes");

    let events = collect_events(&mut rx, 200).await;
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
        .expect("RouteSelected event present");
    assert_eq!(
        routed.2,
        RoutingReason::Default,
        "rule must skip when its target provider is not connected"
    );
    assert_eq!(routed.0.as_str(), "anthropic");
    assert_eq!(routed.1, "claude-haiku-4-5");

    assert_eq!(
        a_seen.lock().await.as_deref(),
        Some("claude-haiku-4-5"),
        "anthropic stub handles the fall-through call"
    );

    host.shutdown().await;
}

#[tokio::test]
async fn reload_during_turn_does_not_deadlock_or_panic() {
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        caps_one("claude-haiku-4-5"),
        Arc::clone(&a_seen),
    );

    let (_dir, path) = write_routing_toml(
        r#"
[[rule]]
name = "r1"
match = { keywords = ["x"] }
use = "anthropic/claude-haiku-4-5"
"#,
    );

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "claude-haiku-4-5",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;
    cfg.routing_rules_path = Some(path.clone());

    let host = Arc::new(Host::start(cfg).await.expect("host starts"));

    let host_t = Arc::clone(&host);
    let reload_path = path.clone();
    let reloader = tokio::spawn(async move {
        for i in 0..20 {
            let body = format!(
                "[[rule]]\nname = \"r{i}\"\nmatch = {{ keywords = [\"x\"] }}\nuse = \"anthropic/claude-haiku-4-5\"\n"
            );
            std::fs::write(&reload_path, body).expect("rewrite routing.toml");
            let _ = host_t.reload_routing_rules().await;
            // `yield_now` instead of `sleep(5ms)` keeps the contention
            // window without holding up the test on slow CI runners.
            tokio::task::yield_now().await;
        }
    });

    for _ in 0..10 {
        let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
        let _ = host.run_turn_streaming("xenon", tx).await;
        while rx.recv().await.is_some() {}
    }

    reloader.await.expect("reloader task joins cleanly");
    // No panic and no deadlock — bounded loop above must terminate.
}

#[tokio::test]
async fn reload_with_parse_error_keeps_prior_rules() {
    let a_seen = Arc::new(Mutex::new(None));
    let a_reg = reg(
        "anthropic",
        caps_one("claude-haiku-4-5"),
        Arc::clone(&a_seen),
    );

    let (_d, path) = write_routing_toml(
        r#"
[[rule]]
name = "valid"
match = { keywords = ["x"] }
use = "anthropic/claude-haiku-4-5"
"#,
    );

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "claude-haiku-4-5",
    );
    cfg.providers = vec![a_reg];
    cfg.startup_connect = StartupConnectPolicy::All;
    cfg.routing_rules_path = Some(path.clone());

    let host = Host::start(cfg).await.expect("host starts");
    // Sanity: the valid file loaded.
    assert_eq!(host.routing_rules_snapshot().await.rules.len(), 1);

    // Overwrite the file with invalid TOML.
    std::fs::write(&path, "this is not [[ valid ]] toml = unterminated").unwrap();
    let err = host.reload_routing_rules().await;
    assert!(err.is_err(), "expected reload to surface the parse error");

    // The prior rules must still be in place.
    let after = host.routing_rules_snapshot().await;
    assert_eq!(
        after.rules.len(),
        1,
        "prior rules should survive parse error"
    );
    assert_eq!(after.rules[0].name, "valid");

    host.shutdown().await;
}
