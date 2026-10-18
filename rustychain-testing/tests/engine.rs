use std::sync::Arc;
use std::time::Duration;

use rustychain_core::{
    CallId, ContinuityFragment, ContinuityScope, ContinuitySemantics, Engine, EngineBuilder,
    EngineConfig, EngineEvent, Error, FinishReason, FnExecutor, Message, MessageStatus, Part,
    Phase, PolicySet, Result, Role, Store, StreamEvent, ToolApprovalPolicy, ToolContext, ToolName,
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

#[tokio::test]
async fn two_calls_in_one_message_each_get_a_result() {
    fn call(index: u32, call_id: CallId, text: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ToolCallStart {
                index,
                call_id,
                name: "echo".to_owned(),
            },
            StreamEvent::ToolCallDelta {
                index,
                call_id,
                name: None,
                arguments: json!({ "text": text }).to_string(),
            },
            StreamEvent::ToolCallDone { index },
        ]
    }

    let first = CallId::new();
    let second = CallId::new();
    let mut events = call(0, first, "one");
    events.extend(call(1, second, "two"));
    events.push(StreamEvent::Finish(FinishReason::ToolCalls));
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn(events)
            .turn_text("both done"),
    );
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");

    let harness = harness(scripted, tools, PolicySet::default());
    let session = harness.engine.create().await.expect("create");
    let outcome = harness
        .engine
        .send(session.id, Message::user("echo twice"))
        .finish()
        .await
        .expect("turn");

    assert_eq!(outcome.session.phase, Phase::Idle);
    assert_eq!(
        outcome.message.as_ref().map(Message::text).as_deref(),
        Some("both done")
    );
    assert_eq!(harness.model.request_count(), 2);

    let history = harness
        .store
        .history(outcome.session.head)
        .await
        .expect("history");
    let tool_message = history
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("tool message");
    let results = tool_message.tool_results();
    assert_eq!(results.len(), 2, "one result per call id");
    assert_eq!(results[0].call_id, first);
    assert_eq!(results[1].call_id, second);
    assert_eq!(results[0].structured, Some(json!({ "echoed": "one" })));
    assert_eq!(results[1].structured, Some(json!({ "echoed": "two" })));
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
async fn a_denied_call_comes_back_to_the_model_as_a_failure() {
    let call_id = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_tool_call(call_id, "echo", &json!({ "text": "secret" }))
            .turn_text("understood, I will not run it"),
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

    let pending = harness
        .engine
        .pending_approvals(session.id)
        .await
        .expect("pending");
    assert_eq!(pending.len(), 1);
    harness
        .engine
        .deny(pending[0].id, "tester", "not on my watch")
        .await
        .expect("deny");

    let outcome = harness
        .engine
        .resume(session.id)
        .finish()
        .await
        .expect("resume");
    assert_eq!(outcome.session.phase, Phase::Idle);
    assert_eq!(
        outcome.message.as_ref().map(Message::text).as_deref(),
        Some("understood, I will not run it"),
        "the turn carries on and the model answers"
    );

    let history = harness
        .store
        .history(outcome.session.head)
        .await
        .expect("history");
    let result = history[2].tool_results()[0];
    assert!(
        result.is_error,
        "a denial is a failed call, not a dead turn"
    );
    assert!(
        result
            .parts
            .iter()
            .filter_map(Part::as_text)
            .any(|text| text.contains("not on my watch")),
        "the reason must reach the model"
    );
    assert!(
        harness
            .engine
            .pending_approvals(session.id)
            .await
            .expect("pending")
            .is_empty(),
        "a denied call is decided, so it is no longer pending"
    );
}

#[tokio::test]
async fn a_tool_scoped_grant_covers_later_calls() {
    let first = CallId::new();
    let second = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_tool_call(first, "echo", &json!({ "text": "one" }))
            .turn_tool_call(second, "echo", &json!({ "text": "two" }))
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

    let pending = harness
        .engine
        .pending_approvals(session.id)
        .await
        .expect("pending");
    assert_eq!(pending.len(), 1);
    harness
        .engine
        .approve(
            pending[0].id,
            rustychain_core::GrantScope::Tool(ToolName::new("echo").expect("tool")),
            "tester",
        )
        .await
        .expect("approve");

    let outcome = harness
        .engine
        .resume(session.id)
        .finish()
        .await
        .expect("resume");
    assert_eq!(outcome.session.phase, Phase::Idle);
    assert_eq!(
        outcome.message.as_ref().map(Message::text).as_deref(),
        Some("finished"),
        "the second call runs under the standing grant without parking"
    );
    assert_eq!(harness.model.request_count(), 3);

    let records = harness
        .store
        .approvals(session.id)
        .await
        .expect("approvals");
    assert_eq!(
        records.len(),
        1,
        "the second call needed no approval record of its own"
    );

    let history = harness
        .store
        .history(outcome.session.head)
        .await
        .expect("history");
    let results = history
        .iter()
        .flat_map(Message::tool_results)
        .filter(|result| !result.is_error)
        .count();
    assert_eq!(results, 2, "both calls ran");
}

#[tokio::test]
async fn a_steering_message_is_drained_ahead_of_the_turn() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("ok"));
    let harness = harness(scripted, ToolRegistry::new(), PolicySet::default());

    let session = harness.engine.create().await.expect("create");
    let steered = harness
        .engine
        .steer(session.id, Message::user("steer"))
        .await
        .expect("steer");
    assert_eq!(steered.queue.len(), 1);

    let outcome = harness
        .engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");
    assert!(outcome.session.queue.is_empty(), "the queue is drained");

    let history = harness
        .store
        .history(outcome.session.head)
        .await
        .expect("history");
    let texts: Vec<String> = history
        .iter()
        .filter(|message| message.role == Role::User)
        .map(Message::text)
        .collect();
    assert_eq!(
        texts,
        ["steer", "go"],
        "the steering message lands before the message that drained it"
    );

    let requests = harness.model.requests();
    let seen: Vec<String> = requests[0].messages.iter().map(Message::text).collect();
    assert_eq!(seen, ["steer", "go"], "and the model sees both in order");
}

#[tokio::test]
async fn a_revoked_grant_stops_authorizing_the_call() {
    let call_id = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_tool_call(call_id, "echo", &json!({ "text": "secret" }))
            .turn_text("understood"),
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

    let pending = harness
        .engine
        .pending_approvals(session.id)
        .await
        .expect("pending");
    let approval = pending[0].id;
    harness
        .engine
        .approve(
            approval,
            rustychain_core::GrantScope::Call(call_id),
            "tester",
        )
        .await
        .expect("approve");

    let granted = harness.store.load(session.id).await.expect("session");
    let grant = granted.grants[0].id;
    let record = harness
        .engine
        .revoke(session.id, grant, "tester")
        .await
        .expect("revoke");
    assert!(!record.grants[0].is_active());

    let outcome = harness
        .engine
        .resume(session.id)
        .finish()
        .await
        .expect("resume");
    assert_eq!(outcome.session.phase, Phase::Idle);
    assert_eq!(
        outcome.message.as_ref().map(Message::text).as_deref(),
        Some("understood")
    );

    let history = harness
        .store
        .history(outcome.session.head)
        .await
        .expect("history");
    let result = history[2].tool_results()[0];
    assert!(result.is_error, "a revoked grant must not run the call");
    assert!(
        result
            .parts
            .iter()
            .filter_map(Part::as_text)
            .any(|text| text.contains("no active grant")),
        "the model must learn why"
    );
}

#[tokio::test]
async fn revoking_a_standing_grant_brings_the_question_back() {
    let first = CallId::new();
    let second = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_tool_call(first, "echo", &json!({ "text": "one" }))
            .turn_text("finished")
            .turn_tool_call(second, "echo", &json!({ "text": "two" }))
            .turn_text("done"),
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

    let pending = harness
        .engine
        .pending_approvals(session.id)
        .await
        .expect("pending");
    harness
        .engine
        .approve(
            pending[0].id,
            rustychain_core::GrantScope::Tool(ToolName::new("echo").expect("tool")),
            "tester",
        )
        .await
        .expect("approve");

    let outcome = harness
        .engine
        .resume(session.id)
        .finish()
        .await
        .expect("resume");
    assert_eq!(
        outcome.message.as_ref().map(Message::text).as_deref(),
        Some("finished"),
        "the tool-scoped grant covers the call"
    );

    let granted = harness.store.load(session.id).await.expect("session");
    harness
        .engine
        .revoke(session.id, granted.grants[0].id, "tester")
        .await
        .expect("revoke");

    let outcome = harness
        .engine
        .send(session.id, Message::user("again"))
        .finish()
        .await
        .expect("turn");
    assert_eq!(
        outcome.session.phase,
        Phase::AwaitingApproval,
        "with the grant revoked the next call must be asked about again"
    );
    let pending = harness
        .engine
        .pending_approvals(session.id)
        .await
        .expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].call_id, second);
    assert!(
        outcome.session.grants.iter().all(|held| !held.is_active()),
        "the standing grant is gone"
    );
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
