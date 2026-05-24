//! Asserts that an in-process tool registered on a `Host` is dispatched
//! via `call_in_process` (NOT the stdio path) when the parent model
//! emits a matching `ToolUse`. The previous wiring in
//! `Host::run_turn_inner` only built the stdio set into `tool_defs` and
//! routed every `ToolUse` through `call_with_bash_net_override`, which
//! returned an "must be dispatched via call_in_process" guardrail error
//! for in-process names. This regression test pins the fixed behaviour:
//!
//! 1. `tool_defs()` aggregates stdio + in-process tools, so the parent
//!    model sees the in-process tool's spec.
//! 2. The parent dispatch loop checks `in_process_has(name)` and routes
//!    matching `ToolUse`s through `call_in_process` with a real
//!    `ToolCallContext`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use savvagent_host::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
use savvagent_host::{
    Host, HostConfig, PermissionDecision, PermissionPolicy, ProviderEndpoint, ProviderRegistration,
    StartupConnectPolicy, ToolCallContext, ToolDef,
};
use savvagent_mcp::ProviderClient;
use savvagent_plugin::{InProcessToolHandler, InProcessToolHandlerArc};
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ContentBlock, ListModelsResponse, ProviderError, ProviderId,
    StopReason, StreamEvent,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

type InputLog = Arc<std::sync::Mutex<Vec<Value>>>;
type SessionLog = Arc<std::sync::Mutex<Vec<String>>>;

/// Records every call's input + the host session_id seen via the
/// `ToolCallContext`. Lets the test assert both that the handler ran
/// and that it received a typed context.
struct RecordingHandler {
    inputs: InputLog,
    seen_session_ids: SessionLog,
}

impl RecordingHandler {
    fn new() -> (InputLog, SessionLog, Self) {
        let inputs: InputLog = Arc::new(std::sync::Mutex::new(Vec::new()));
        let session_ids: SessionLog = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler = Self {
            inputs: Arc::clone(&inputs),
            seen_session_ids: Arc::clone(&session_ids),
        };
        (inputs, session_ids, handler)
    }
}

#[async_trait]
impl InProcessToolHandler for RecordingHandler {
    async fn call(
        &self,
        input: Value,
        ctx: Arc<dyn std::any::Any + Send + Sync>,
    ) -> Result<Value, String> {
        self.inputs.lock().unwrap().push(input.clone());
        if let Some(tc) = ctx.downcast_ref::<ToolCallContext>() {
            self.seen_session_ids
                .lock()
                .unwrap()
                .push(tc.host.session_id());
        }
        Ok(Value::String("recording-ok".into()))
    }
}

/// Provider that emits a single `ToolUse` for `my-in-process` on the
/// first round, then `end_turn` with assistant text on subsequent rounds.
struct ScriptedProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ProviderClient for ScriptedProvider {
    async fn complete(
        &self,
        req: CompleteRequest,
        _events: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let (content, stop) = if n == 0 {
            (
                vec![ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "my-in-process".into(),
                    input: json!({"echo": "hello"}),
                }],
                StopReason::ToolUse,
            )
        } else {
            (
                vec![ContentBlock::Text {
                    text: "done".into(),
                }],
                StopReason::EndTurn,
            )
        };
        Ok(CompleteResponse {
            id: format!("resp-{n}"),
            model: req.model,
            content,
            stop_reason: stop,
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
    .expect("valid test caps")
}

#[tokio::test]
async fn parent_dispatch_routes_in_process_tool() {
    // 1. Build the host. We use the pool path with one scripted provider
    //    so the host fully resolves (the legacy single-provider rmcp
    //    path tries to dial a real HTTP transport at start).
    let project_root = std::env::temp_dir().join("savvagent-in-proc-dispatch-test");
    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "stub-model",
    )
    .with_project_root(project_root.clone())
    .with_policy(PermissionPolicy::transient(project_root))
    .with_session_id("session-under-test");
    cfg.providers = vec![ProviderRegistration::new(
        ProviderId::new("stub").unwrap(),
        "stub",
        Arc::new(ScriptedProvider {
            calls: AtomicUsize::new(0),
        }) as Arc<dyn ProviderClient + Send + Sync>,
        caps("stub-model"),
    )];
    cfg.startup_connect = StartupConnectPolicy::All;

    let host = Arc::new(Host::start(cfg).await.expect("host starts"));
    host.wire_self_arc();

    // 2. Pre-Allow our synthetic tool name so the parent's `Ask` default
    //    doesn't pause the turn waiting for a permission decision.
    host.add_session_rule(
        "my-in-process",
        &json!({"echo": "hello"}),
        PermissionDecision::Allow,
    )
    .await;

    // 3. Register the in-process tool. The test handler records every
    //    call and the host's reported session_id.
    let (inputs, session_ids, handler) = RecordingHandler::new();
    let spec = ToolDef {
        name: "my-in-process".into(),
        description: "test in-process tool".into(),
        input_schema: json!({
            "type": "object",
            "properties": {"echo": {"type": "string"}},
            "required": ["echo"]
        }),
    };
    let registry = host
        .tool_registry_arc()
        .await
        .expect("tool registry present");
    registry
        .register_in_process_tool(spec, InProcessToolHandlerArc::new(handler))
        .await;

    // 4. The aggregator surface (`tool_defs`) must include the
    //    in-process tool so the parent model can see it.
    let defs = registry.tool_defs().await;
    assert!(
        defs.iter().any(|d| d.name == "my-in-process"),
        "tool_defs() must include in-process tools; got {:?}",
        defs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>()
    );

    // 5. Run a turn. The scripted provider asks for `my-in-process`; the
    //    parent's dispatch loop must route through `call_in_process`.
    let (tx, mut rx) = mpsc::channel(64);
    let host_for_run = host.clone();
    let runner = tokio::spawn(async move { host_for_run.run_turn_streaming("hi", tx).await });
    while rx.recv().await.is_some() {}
    let outcome = runner.await.unwrap().expect("turn ok");

    // 6. The handler was invoked exactly once with our input.
    let recorded = inputs.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1, "handler must be called exactly once");
    assert_eq!(recorded[0], json!({"echo": "hello"}));

    // 7. The handler saw a real `ToolCallContext` whose host.session_id()
    //    matches the value we threaded through `HostConfig::with_session_id`.
    let seen = session_ids.lock().unwrap().clone();
    assert_eq!(
        seen.as_slice(),
        ["session-under-test"],
        "handler must observe the host session_id via ToolCallContext"
    );

    // 8. The recorded outcome's single tool call must NOT carry the
    //    "must be dispatched via call_in_process" guardrail error.
    assert_eq!(outcome.tool_calls.len(), 1);
    let call = &outcome.tool_calls[0];
    assert_eq!(call.name, "my-in-process");
    assert!(
        !call.result.contains("call_in_process"),
        "in-process tool must not hit the stdio guardrail; got: {}",
        call.result
    );
    assert_eq!(call.result, "recording-ok");
    assert_eq!(outcome.text, "done");
}

#[tokio::test]
async fn host_session_id_defaults_to_unique_uuid() {
    // Two hosts built without `with_session_id` must produce distinct
    // UUIDs; both must be non-empty.
    let mk = || async {
        let project_root = std::env::temp_dir().join("savvagent-session-id-uniqueness-test");
        let mut cfg = HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "http://unused".into(),
            },
            "stub-model",
        )
        .with_project_root(project_root.clone())
        .with_policy(PermissionPolicy::transient(project_root));
        cfg.providers = vec![ProviderRegistration::new(
            ProviderId::new("stub").unwrap(),
            "stub",
            Arc::new(ScriptedProvider {
                calls: AtomicUsize::new(0),
            }) as Arc<dyn ProviderClient + Send + Sync>,
            caps("stub-model"),
        )];
        cfg.startup_connect = StartupConnectPolicy::All;
        Host::start(cfg).await.expect("host starts")
    };

    let h1 = mk().await;
    let h2 = mk().await;
    let s1 = h1.session_id();
    let s2 = h2.session_id();
    assert!(
        !s1.is_empty(),
        "session id must default to a non-empty uuid"
    );
    assert!(
        !s2.is_empty(),
        "session id must default to a non-empty uuid"
    );
    assert_ne!(s1, s2, "default session ids must be unique per host");
}
