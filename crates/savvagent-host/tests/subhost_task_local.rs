//! Verifies SUBAGENT_NAME is set during SubHost dispatch and absent outside.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use savvagent_host::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
use savvagent_host::pre_tool_gate::{PreToolDecision, PreToolUseGate};
use savvagent_host::{
    Host, HostConfig, ProviderEndpoint, ProviderRegistration, SUBAGENT_NAME, StartupConnectPolicy,
    SubHost, SubagentContext,
};
use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ContentBlock, ListModelsResponse, ProviderError, ProviderId,
    StopReason, StreamEvent, ToolDef,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// A provider that issues exactly one tool call (`probe`) then end_turns.
struct ToolThenEndProvider {
    fired: Mutex<bool>,
}

#[async_trait]
impl ProviderClient for ToolThenEndProvider {
    async fn complete(
        &self,
        req: CompleteRequest,
        _events: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        let should_fire = {
            let mut fired = self.fired.lock().unwrap();
            if !*fired {
                *fired = true;
                true
            } else {
                false
            }
        };
        if should_fire {
            return Ok(CompleteResponse {
                id: "0".into(),
                model: req.model,
                content: vec![ContentBlock::ToolUse {
                    id: "1".into(),
                    name: "probe".into(),
                    input: json!({}),
                }],
                stop_reason: StopReason::ToolUse,
                stop_sequence: None,
                usage: Default::default(),
            });
        }
        Ok(CompleteResponse {
            id: "1".into(),
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

// Gate that captures the SUBAGENT_NAME value seen during check().
struct CapturingGate {
    captured: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl PreToolUseGate for CapturingGate {
    async fn check(&self, _name: &str, _input: &Value) -> PreToolDecision {
        let seen = SUBAGENT_NAME.try_with(|v| v.clone()).ok().flatten();
        *self.captured.lock().unwrap() = seen;
        PreToolDecision::Allow
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
    .expect("caps")
}

#[tokio::test]
async fn subagent_name_set_during_dispatch() {
    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "m",
    );
    cfg.providers = vec![ProviderRegistration::new(
        ProviderId::new("stub").unwrap(),
        "stub",
        Arc::new(ToolThenEndProvider {
            fired: Mutex::new(false),
        }) as Arc<dyn ProviderClient + Send + Sync>,
        caps("m"),
    )];
    cfg.startup_connect = StartupConnectPolicy::All;
    let host = Arc::new(Host::start(cfg).await.expect("host starts"));

    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    host.set_pre_tool_gate(Arc::new(CapturingGate {
        captured: captured.clone(),
    }))
    .await;

    let mut allowed = HashSet::new();
    allowed.insert("probe".to_string());

    let sub = SubHost::new(
        host,
        SubagentContext {
            depth: 1,
            agent_name: "code-reviewer".into(),
            parent_session_id: "session-1".into(),
        },
        "You are a reviewer.".into(),
        None,
        allowed,
        vec![ToolDef {
            name: "probe".into(),
            description: "test tool".into(),
            input_schema: json!({"type": "object"}),
        }],
        CancellationToken::new(),
        None,
    )
    .await
    .expect("SubHost ok");

    let _ = sub.run_subagent("go".into()).await;

    let final_seen = captured.lock().unwrap().clone();
    assert_eq!(final_seen, Some("code-reviewer".to_string()));
}

#[tokio::test]
async fn subagent_name_absent_outside_scope() {
    let seen = SUBAGENT_NAME.try_with(|v| v.clone()).ok().flatten();
    assert!(
        seen.is_none(),
        "SUBAGENT_NAME must be None outside any scope"
    );
}
