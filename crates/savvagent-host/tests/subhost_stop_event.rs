//! Asserts SubHost emits TurnEvent::SubagentStop on clean end_turn,
//! and does NOT emit on cancellation.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
use savvagent_host::{
    Host, HostConfig, ProviderEndpoint, ProviderRegistration, StartupConnectPolicy, SubHost,
    SubHostError, SubagentContext, TurnEvent,
};
use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ContentBlock, ListModelsResponse, ProviderError, ProviderId,
    StopReason, StreamEvent,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct FixedProvider;

#[async_trait]
impl ProviderClient for FixedProvider {
    async fn complete(
        &self,
        req: CompleteRequest,
        _events: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        Ok(CompleteResponse {
            id: "fixed-0".into(),
            model: req.model,
            content: vec![ContentBlock::Text {
                text: "done".into(),
            }],
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

fn caps(model: &str) -> ProviderCapabilities {
    ProviderCapabilities::new(
        vec![ModelCapabilities {
            id: model.into(),
            display_name: model.into(),
            supports_vision: false,
            supports_audio: false,
            context_window: 1000,
            cost_tier: CostTier::Standard,
        }],
        model.into(),
    )
    .expect("valid caps")
}

async fn build_host_with_fixed_provider() -> Arc<Host> {
    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "m",
    );
    cfg.providers = vec![ProviderRegistration::new(
        ProviderId::new("stub").unwrap(),
        "stub",
        Arc::new(FixedProvider) as Arc<dyn ProviderClient + Send + Sync>,
        caps("m"),
    )];
    cfg.startup_connect = StartupConnectPolicy::All;
    Arc::new(Host::start(cfg).await.expect("host starts"))
}

#[tokio::test]
async fn subagent_stop_event_fires_on_end_turn() {
    let host = build_host_with_fixed_provider().await;
    let (tx, mut rx) = mpsc::channel::<TurnEvent>(8);

    let sub = SubHost::new(
        host,
        SubagentContext {
            depth: 1,
            agent_name: "code-reviewer".into(),
            parent_session_id: "session-1".into(),
        },
        "You are a reviewer.".into(),
        None,
        HashSet::new(),
        vec![],
        CancellationToken::new(),
        Some(tx),
    )
    .await
    .expect("SubHost::new returns Ok");

    let text = sub
        .run_subagent("review the diff".into())
        .await
        .expect("ok");
    assert_eq!(text, "done");

    // Drain the channel and assert exactly one SubagentStop with the right name.
    let mut saw_stop = false;
    while let Ok(event) = rx.try_recv() {
        if let TurnEvent::SubagentStop {
            agent_name,
            success,
        } = event
        {
            assert_eq!(agent_name, "code-reviewer");
            assert!(success);
            saw_stop = true;
        }
    }
    assert!(saw_stop, "expected SubagentStop event after end_turn");
}

#[tokio::test]
async fn subagent_stop_event_does_not_fire_on_cancellation() {
    let host = build_host_with_fixed_provider().await;
    let (tx, mut rx) = mpsc::channel::<TurnEvent>(8);
    let token = CancellationToken::new();
    token.cancel(); // Cancel BEFORE running.

    let sub = SubHost::new(
        host,
        SubagentContext {
            depth: 1,
            agent_name: "code-reviewer".into(),
            parent_session_id: "session-1".into(),
        },
        "You are a reviewer.".into(),
        None,
        HashSet::new(),
        vec![],
        token,
        Some(tx),
    )
    .await
    .expect("SubHost::new returns Ok");

    let result = sub.run_subagent("review the diff".into()).await;
    assert!(matches!(result, Err(SubHostError::Cancelled)));

    // Drain and assert NO SubagentStop event was emitted.
    while let Ok(event) = rx.try_recv() {
        if matches!(event, TurnEvent::SubagentStop { .. }) {
            panic!("SubagentStop should not fire on cancellation");
        }
    }
}
