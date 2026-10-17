use std::sync::Arc;
use std::time::Duration;

use rustychain_core::{
    CallId, ContinuityFragment, ContinuityScope, ContinuitySemantics, Engine, EngineBuilder,
    EngineConfig, EngineEvent, Error, FinishReason, FnExecutor, Message, MessageStatus, Phase,
    PolicySet, Result, Role, Store, StreamEvent, ToolApprovalPolicy, ToolContext, ToolName,
    ToolRegistry, WireFamily,
};
use rustychain_testing::{
    InMemoryBlobStore, InMemoryStore, ScriptedModel, StrictContinuity, model, text_capabilities,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EchoArgs {
    text: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct EchoResult {
    echoed: String,
}

#[rustychain_core::tool(name = "echo", description = "Echo the provided text")]
struct Echo;

#[async_trait::async_trait]
impl FnExecutor<EchoArgs, EchoResult> for Echo {
    async fn call(&self, args: EchoArgs) -> Result<EchoResult> {
        Ok(EchoResult { echoed: args.text })
    }
}

struct Harness {
    engine: Engine,
    store: Arc<InMemoryStore>,
    model: Arc<ScriptedModel>,
}

fn harness(model: Arc<ScriptedModel>, tools: ToolRegistry, policies: PolicySet) -> Harness {
    harness_with_config(model, tools, policies, EngineConfig::default())
}

fn harness_with_config(
    model: Arc<ScriptedModel>,
    tools: ToolRegistry,
    policies: PolicySet,
    config: EngineConfig,
) -> Harness {
    let store = Arc::new(InMemoryStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let engine = EngineBuilder::new()
        .store(store.clone())
        .blobs(blobs)
        .model(model.clone())
        .tools(tools)
        .policies(policies)
        .config(config)
        .build()
        .expect("build engine");
    Harness {
        engine,
        store,
        model,
    }
}

#[tokio::test]
async fn a_text_turn_is_committed_and_reported() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("hello world"));
    let harness = harness(scripted, ToolRegistry::new(), PolicySet::default());

    let session = harness.engine.create().await.expect("create");
    let outcome = harness
        .engine
        .send(session.id, Message::user("ping"))
        .finish()
        .await
        .expect("turn");

    assert_eq!(outcome.session.phase, Phase::Idle);
    assert_eq!(
        outcome.message.as_ref().map(Message::text).as_deref(),
        Some("hello world")
    );

    let history = harness
        .store
        .history(outcome.session.head)
        .await
        .expect("history");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].role, Role::User);
    assert_eq!(history[1].role, Role::Assistant);
    assert_eq!(history[1].text(), "hello world");
    assert_eq!(harness.model.request_count(), 1);
}

#[tokio::test]
async fn tool_calls_run_and_feed_the_next_turn() {
    let call_id = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_tool_call(call_id, "echo", &json!({ "text": "hi" }))
            .turn_text("all done"),
    );
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");

    let harness = harness(scripted, tools, PolicySet::default());
    let session = harness.engine.create().await.expect("create");
    let outcome = harness
        .engine
        .send(session.id, Message::user("please echo"))
        .finish()
        .await
        .expect("turn");

    assert_eq!(outcome.session.phase, Phase::Idle);
    assert_eq!(
        outcome.message.as_ref().map(Message::text).as_deref(),
        Some("all done")
    );
    assert_eq!(harness.model.request_count(), 2);

    let history = harness
        .store
        .history(outcome.session.head)
        .await
        .expect("history");
    assert_eq!(history.len(), 4);
    assert_eq!(history[2].role, Role::Tool);
    let result = history[2].tool_results()[0];
    assert_eq!(result.call_id, call_id);
    assert!(!result.is_error);
    assert_eq!(result.structured, Some(json!({ "echoed": "hi" })));

    let second_request = harness.model.requests()[1].clone();
    let tool_message = second_request
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("tool message reached the model");
    assert_eq!(tool_message.tool_results()[0].call_id, call_id);
}

/// Runs one echo turn and reports how many commits had succeeded when the tool
/// finished, so early and sequential execution can be compared.
async fn tool_finish_commit_offset(config: EngineConfig) -> usize {
    use futures_util::StreamExt;

    let call_id = CallId::new();
    let events = vec![
        StreamEvent::ToolCallStart {
            index: 0,
            call_id,
            name: "echo".to_owned(),
        },
        StreamEvent::ToolCallDelta {
            index: 0,
            call_id,
            name: None,
            arguments: json!({ "text": "hi" }).to_string(),
        },
        StreamEvent::ToolCallDone { index: 0 },
        StreamEvent::Finish(FinishReason::ToolCalls),
    ];
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn(events)
            .turn_text("all done"),
    );
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");

    let harness = harness_with_config(scripted, tools, PolicySet::default(), config);
    let session = harness.engine.create().await.expect("create");

    let mut stream = harness
        .engine
        .send(session.id, Message::user("please echo"));
    let mut commits = 0usize;
    let mut offset = None;
    while let Some(event) = stream.next().await {
        match event.expect("event") {
            EngineEvent::CommitSucceeded { .. } => commits += 1,
            EngineEvent::ToolCallFinished { .. } if offset.is_none() => offset = Some(commits),
            _ => {}
        }
    }
    stream.finish().await.expect("turn");
    offset.expect("the tool finished")
}

#[tokio::test]
async fn early_tool_execution_finishes_during_the_stream() {
    let early = tool_finish_commit_offset(EngineConfig {
        early_tool_execution: true,
        ..EngineConfig::default()
    })
    .await;
    let sequential = tool_finish_commit_offset(EngineConfig::default()).await;
    assert!(
        early < sequential,
        "early execution finishes while the assistant message streams, before its commit"
    );
    assert_eq!(
        early + 1,
        sequential,
        "early execution finishes exactly one commit earlier"
    );
}

#[tokio::test]
async fn an_unknown_tool_is_reported_to_the_model_as_an_error() {
    let call_id = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_tool_call(call_id, "missing", &json!({}))
            .turn_text("recovered"),
    );
    let harness = harness(scripted, ToolRegistry::new(), PolicySet::default());
    let session = harness.engine.create().await.expect("create");
    let outcome = harness
        .engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");

    let history = harness
        .store
        .history(outcome.session.head)
        .await
        .expect("history");
    let result = history[2].tool_results()[0];
    assert!(result.is_error);
    assert_eq!(
        outcome.message.as_ref().map(Message::text).as_deref(),
        Some("recovered")
    );
}

#[tokio::test]
async fn a_required_approval_blocks_then_resumes() {
    let call_id = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_tool_call(call_id, "echo", &json!({ "text": "secret" }))
            .turn_text("finished"),
    );
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");
    let policies = PolicySet::new(vec![Arc::new(ToolApprovalPolicy::new(
        "guard",
        vec![ToolName::new("echo").expect("tool")],
        "echo touches the world",
    ))]);
    let harness = harness(scripted, tools, policies);

    let session = harness.engine.create().await.expect("create");
    let outcome = harness
        .engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");
    assert_eq!(outcome.session.phase, Phase::AwaitingApproval);
    assert_eq!(harness.model.request_count(), 1);

    let pending = harness
        .engine
        .pending_approvals(session.id)
        .await
        .expect("pending");
    assert_eq!(pending.len(), 1);
    let record = pending[0].clone();

    harness
        .engine
        .approve(
            record.id,
            rustychain_core::GrantScope::Call(call_id),
            "tester",
        )
        .await
        .expect("approve");

    let outcome = harness
        .engine
        .resume(session.id)
        .finish()
        .await
        .expect("turn");
    assert_eq!(outcome.session.phase, Phase::Idle);
    assert_eq!(
        outcome.message.as_ref().map(Message::text).as_deref(),
        Some("finished")
    );

    let history = harness
        .store
        .history(outcome.session.head)
        .await
        .expect("history");
    let result = history[2].tool_results()[0];
    assert!(!result.is_error);
    assert_eq!(result.structured, Some(json!({ "echoed": "secret" })));
    assert!(!outcome.session.grants.is_empty());
}

#[tokio::test]
async fn cancelling_a_turn_leaves_the_session_resumable() {
    let events = vec![
        rustychain_core::StreamEvent::TextStart { index: 0 },
        rustychain_core::StreamEvent::TextDelta {
            index: 0,
            text: "partial".to_owned(),
        },
        rustychain_core::StreamEvent::Finish(rustychain_core::FinishReason::Stop),
    ];
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo")).turn_delayed(events, Duration::from_millis(50)),
    );
    let harness = harness(scripted, ToolRegistry::new(), PolicySet::default());
    let session = harness.engine.create().await.expect("create");

    let stream = harness.engine.send(session.id, Message::user("go"));
    stream.cancel();
    let outcome = stream.finish().await.expect("turn");

    assert_eq!(outcome.session.phase, Phase::Idle);
    let head = outcome.session.head.expect("head");
    let message = harness.store.node(head).await.expect("node").message;
    assert_eq!(message.role, Role::Assistant);
    assert_eq!(
        message.origin.as_ref().map(|origin| origin.status),
        Some(MessageStatus::Interrupted)
    );
}

#[tokio::test]
async fn an_unsupported_tool_shape_is_a_capability_error() {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");
    let mut capabilities = text_capabilities();
    capabilities.tool_kinds.clear();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .with_capabilities(capabilities)
            .turn_text("never"),
    );

    let harness = harness(scripted, tools, PolicySet::default());
    let session = harness.engine.create().await.expect("create");
    let error = harness
        .engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect_err("capability error");
    assert!(matches!(error, Error::Capability { .. }));
}

#[tokio::test]
async fn a_failing_model_surfaces_a_typed_error() {
    let store = Arc::new(InMemoryStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let failing = Arc::new(rustychain_testing::FailingModel::new(
        model("test/echo"),
        "boom",
    ));
    let engine = EngineBuilder::new()
        .store(store)
        .blobs(blobs)
        .model(failing)
        .build()
        .expect("build");
    let session = engine.create().await.expect("create");
    let error = engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect_err("model error");
    assert!(matches!(error, Error::Other(ref message) if message == "boom"));
}

#[tokio::test]
async fn tool_context_reaches_the_executor() {
    #[rustychain_core::tool(name = "probe", description = "Probe the tool context")]
    struct ContextProbe;

    #[derive(Debug, JsonSchema, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct NoArgs {}

    #[async_trait::async_trait]
    impl FnExecutor<NoArgs, serde_json::Value> for ContextProbe {
        async fn call(&self, _args: NoArgs) -> Result<serde_json::Value> {
            Ok(serde_json::Value::Null)
        }

        async fn call_with_context(
            &self,
            context: &ToolContext,
            _args: NoArgs,
        ) -> Result<serde_json::Value> {
            Ok(json!({
                "has_session": context.session.is_some(),
                "has_call": context.call_id.is_some(),
            }))
        }
    }

    let call_id = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_tool_call(call_id, "probe", &json!({}))
            .turn_text("done"),
    );
    let mut tools = ToolRegistry::new();
    tools
        .register_declarator(&ContextProbe)
        .expect("register probe");
    let harness = harness(scripted, tools, PolicySet::default());
    let session = harness.engine.create().await.expect("create");
    let outcome = harness
        .engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");
    let history = harness
        .store
        .history(outcome.session.head)
        .await
        .expect("history");
    let result = history[2].tool_results()[0];
    assert_eq!(
        result.structured,
        Some(json!({ "has_session": true, "has_call": true }))
    );
}

#[tokio::test]
async fn switching_models_mid_tool_loop_blocks_instead_of_dropping_tokens() {
    let call_id = CallId::new();
    let store = Arc::new(InMemoryStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let first_ref = model("test/first");
    let fragment = ContinuityFragment::new(
        WireFamily::new("test.strict").expect("family"),
        ContinuityScope::Exact(first_ref.clone()),
        1,
        ContinuitySemantics::Replace,
        b"opaque".to_vec(),
    );
    let first = Arc::new(
        ScriptedModel::new(first_ref.clone())
            .with_continuity(Arc::new(StrictContinuity::new(first_ref.clone())))
            .turn(vec![
                StreamEvent::ToolCallStart {
                    index: 0,
                    call_id,
                    name: "echo".to_owned(),
                },
                StreamEvent::ToolCallDelta {
                    index: 0,
                    call_id,
                    name: None,
                    arguments: json!({ "text": "hi" }).to_string(),
                },
                StreamEvent::Continuity { index: 0, fragment },
                StreamEvent::Finish(FinishReason::ToolCalls),
            ])
            .turn_text("done"),
    );
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");
    let engine = EngineBuilder::new()
        .store(store.clone())
        .blobs(blobs.clone())
        .model(first)
        .tools(tools)
        .build()
        .expect("build engine");
    let session = engine.create().await.expect("create");
    let outcome = engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("first turn");
    assert_eq!(outcome.session.phase, Phase::Idle);

    let second_ref = model("test/second");
    let second = Arc::new(
        ScriptedModel::new(second_ref.clone())
            .with_continuity(Arc::new(StrictContinuity::new(second_ref)))
            .turn_text("never"),
    );
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");
    let switched = EngineBuilder::new()
        .store(store.clone())
        .blobs(blobs)
        .model(second)
        .tools(tools)
        .build()
        .expect("build engine");
    let error = switched
        .send(session.id, Message::user("continue"))
        .finish()
        .await
        .expect_err("continuity blocked");
    assert!(matches!(error, Error::ContinuityBlocked(_)));
}
