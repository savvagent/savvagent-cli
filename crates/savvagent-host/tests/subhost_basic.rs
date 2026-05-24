//! End-to-end smoke for `SubHost`. Builds a `Host` with a stub provider
//! that immediately returns `end_turn` with fixed text, constructs a
//! `SubHost`, and asserts the final assistant text round-trips back out
//! of `run_subagent`.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
use savvagent_host::{
    Host, HostConfig, ProviderEndpoint, ProviderRegistration, StartupConnectPolicy, SubHost,
    SubagentContext,
};
use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ContentBlock, ListModelsResponse, ProviderError, ProviderId,
    StopReason, StreamEvent,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Stub `ProviderClient` that always returns a single `Text` block and
/// `StopReason::EndTurn`. No tool-use, no streaming — just enough to
/// drive `SubHost::run_subagent` through one iteration of its loop.
struct FixedProvider {
    reply: &'static str,
}

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
                text: self.reply.into(),
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

fn fixed_caps(model: &str) -> ProviderCapabilities {
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
    .expect("valid test caps")
}

#[tokio::test]
async fn subhost_returns_text_on_end_turn() {
    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "stub-model",
    );
    cfg.providers = vec![ProviderRegistration::new(
        ProviderId::new("stub").unwrap(),
        "stub",
        Arc::new(FixedProvider {
            reply: "hello from subagent",
        }) as Arc<dyn ProviderClient + Send + Sync>,
        fixed_caps("stub-model"),
    )];
    cfg.startup_connect = StartupConnectPolicy::All;

    let host = Arc::new(Host::start(cfg).await.expect("host starts"));

    let ctx = SubagentContext {
        depth: 1,
        agent_name: "test-agent".into(),
        parent_session_id: "session-1".into(),
    };
    let cancellation = CancellationToken::new();

    let sub = SubHost::new(
        host.clone(),
        ctx,
        "You are a test agent.".into(),
        None,
        HashSet::new(),
        vec![],
        cancellation,
        None, // events
    )
    .await
    .expect("SubHost::new returns Ok");

    let result = sub.run_subagent("hi".into()).await.expect("subagent ok");
    assert_eq!(result, "hello from subagent");
}
