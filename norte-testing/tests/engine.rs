//! Harness integration tests driven by the scripted model.

use std::sync::Arc;
use std::time::Duration;

use norte_core::{
    CallId, ContinuityFragment, ContinuityScope, ContinuitySemantics, Error, Feature, FinishReason,
    FnExecutor, LogprobValue, Message, MessageStatus, Part, Result, Role, StreamEvent,
    TokenLogprob, ToolContext, ToolRegistry, WireFamily,
};
use norte_harness::Store;
use norte_harness::{
    ConsultationPoint, EngineEvent, Harness, HarnessBuilder, HarnessConfig, Interception,
    InterceptionContext, Phase, TurnInterceptor,
};
use norte_testing::{
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

#[norte_core::tool(name = "echo", description = "Echo the provided text")]
struct Echo;

#[async_trait::async_trait]
impl FnExecutor<EchoArgs, EchoResult> for Echo {
    async fn call(&self, args: EchoArgs) -> Result<EchoResult> {
        Ok(EchoResult { echoed: args.text })
    }
}

struct Fixture {
    engine: Harness,
    store: Arc<InMemoryStore>,
    model: Arc<ScriptedModel>,
}

fn fixture(model: Arc<ScriptedModel>, tools: ToolRegistry, config: HarnessConfig) -> Fixture {
    let store = Arc::new(InMemoryStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let engine = HarnessBuilder::new()
        .store(store.clone())
        .blobs(blobs)
        .model(model.clone())
        .tools(tools)
        .config(config)
        .build()
        .expect("build engine");
    Fixture {
        engine,
        store,
        model,
    }
}

#[tokio::test]
async fn a_text_turn_is_committed_and_reported() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("hello world"));
    let harness = fixture(scripted, ToolRegistry::new(), HarnessConfig::default());

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

    let harness = fixture(scripted, tools, HarnessConfig::default());
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

/// One assistant message carrying two tool calls, in order, each with the
/// arguments `{"text": <text>}`.
fn two_call_turn(first: (CallId, &str, &str), second: (CallId, &str, &str)) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    for (index, (call_id, tool, text)) in [(0_u32, first), (1, second)] {
        events.push(StreamEvent::ToolCallStart {
            index,
            call_id,
            name: tool.to_owned(),
        });
        events.push(StreamEvent::ToolCallDelta {
            index,
            call_id,
            name: None,
            arguments: json!({ "text": text }).to_string(),
        });
        events.push(StreamEvent::ToolCallDone { index });
    }
    events.push(StreamEvent::Finish(FinishReason::ToolCalls));
    events
}

#[tokio::test]
async fn two_calls_in_one_message_each_get_a_result() {
    let first = CallId::new();
    let second = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn(two_call_turn(
                (first, "echo", "one"),
                (second, "echo", "two"),
            ))
            .turn_text("both done"),
    );
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");

    let harness = fixture(scripted, tools, HarnessConfig::default());
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
/// Runs one echo turn and reports how many commits had succeeded when the tool
/// finished, so early and sequential execution can be compared.
async fn tool_finish_commit_offset(config: HarnessConfig) -> usize {
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

    let harness = fixture(scripted, tools, config);
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
    let early = tool_finish_commit_offset(HarnessConfig {
        early_tool_execution: true,
        ..HarnessConfig::default()
    })
    .await;
    let sequential = tool_finish_commit_offset(HarnessConfig::default()).await;
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
    let harness = fixture(scripted, ToolRegistry::new(), HarnessConfig::default());
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
async fn a_steering_message_lands_before_the_message_that_follows_it() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("ok"));
    let harness = fixture(scripted, ToolRegistry::new(), HarnessConfig::default());

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
    let texts: Vec<String> = history
        .iter()
        .filter(|message| message.role == Role::User)
        .map(Message::text)
        .collect();
    assert_eq!(texts, ["go"], "the turn carries its own message");
}

#[tokio::test]
async fn cancelling_a_turn_leaves_the_session_resumable() {
    let events = vec![
        norte_core::StreamEvent::TextStart { index: 0 },
        norte_core::StreamEvent::TextDelta {
            index: 0,
            text: "partial".to_owned(),
        },
        norte_core::StreamEvent::Finish(norte_core::FinishReason::Stop),
    ];
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo")).turn_delayed(events, Duration::from_millis(50)),
    );
    let harness = fixture(scripted, ToolRegistry::new(), HarnessConfig::default());
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

    let harness = fixture(scripted, tools, HarnessConfig::default());
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
    let failing = Arc::new(norte_testing::FailingModel::new(model("test/echo"), "boom"));
    let engine = HarnessBuilder::new()
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

/// A turn that failed mid-flight stays resumable: the user message is already
/// committed, the session sits in `Streaming`, and `resume` picks the turn up
/// without asking the same question twice. This is what a host does after a
/// transient provider error (`norte_http::is_transient`).
#[tokio::test]
async fn a_failed_turn_is_resumed_where_it_stopped() {
    let store = Arc::new(InMemoryStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let failing = HarnessBuilder::new()
        .store(store.clone())
        .blobs(blobs.clone())
        .model(Arc::new(norte_testing::FailingModel::new(
            model("test/echo"),
            "boom",
        )))
        .build()
        .expect("build");
    let session = failing.create().await.expect("create");
    failing
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect_err("the provider failed");

    let parked = failing.session(session.id).await.expect("session");
    assert_eq!(parked.phase, Phase::Streaming);
    assert_eq!(
        failing.history(session.id).await.expect("history").len(),
        1,
        "the question is committed, and nothing else"
    );

    // Another engine — another process, another model — takes the session over.
    let resumed = HarnessBuilder::new()
        .store(store)
        .blobs(blobs)
        .model(Arc::new(
            ScriptedModel::new(model("test/echo")).turn_text("recovered"),
        ))
        .build()
        .expect("build");
    let outcome = resumed.resume(session.id).finish().await.expect("resume");
    assert_eq!(
        outcome.message.as_ref().map(Message::text).as_deref(),
        Some("recovered")
    );

    let history = resumed.history(session.id).await.expect("history");
    assert_eq!(
        history
            .iter()
            .map(|message| message.role)
            .collect::<Vec<_>>(),
        vec![Role::User, Role::Assistant],
        "the question is not duplicated"
    );
}

#[tokio::test]
async fn tool_context_reaches_the_executor() {
    #[norte_core::tool(name = "probe", description = "Probe the tool context")]
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
    let harness = fixture(scripted, tools, HarnessConfig::default());
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
    let engine = HarnessBuilder::new()
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
    let switched = HarnessBuilder::new()
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

#[tokio::test]
async fn logprobs_without_the_capability_fail_before_the_request() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("never"));
    let harness = fixture(scripted, ToolRegistry::new(), HarnessConfig::default());
    let session = harness.engine.create().await.expect("create");

    let error = harness
        .engine
        .turn(session.id)
        .input(Message::user("go"))
        .logprobs(3)
        .start()
        .finish()
        .await
        .expect_err("capability error");

    assert!(matches!(error, Error::Capability { .. }));
    assert_eq!(
        harness.model.request_count(),
        0,
        "the request is rejected before the wire"
    );
}

#[tokio::test]
async fn a_logprob_count_above_the_cap_is_a_limit_error() {
    let mut capabilities = text_capabilities();
    capabilities.features.insert(Feature::Logprobs);
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .with_capabilities(capabilities)
            .turn_text("never"),
    );
    let harness = fixture(scripted, ToolRegistry::new(), HarnessConfig::default());
    let session = harness.engine.create().await.expect("create");

    let error = harness
        .engine
        .turn(session.id)
        .input(Message::user("go"))
        .logprobs(21)
        .start()
        .finish()
        .await
        .expect_err("limit error");

    assert!(matches!(error, Error::LimitExceeded { limit: 20, .. }));
    assert_eq!(harness.model.request_count(), 0);
}

#[tokio::test]
async fn requested_logprobs_never_change_the_committed_message() {
    let plain = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("hello"));
    let baseline = fixture(plain, ToolRegistry::new(), HarnessConfig::default());
    let plain_session = baseline.engine.create().await.expect("create");
    let plain_outcome = baseline
        .engine
        .send(plain_session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");
    let plain_head = plain_outcome.session.head.expect("head");
    let plain_message = baseline.store.node(plain_head).await.expect("node").message;

    let mut capabilities = text_capabilities();
    capabilities.features.insert(Feature::Logprobs);
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .with_capabilities(capabilities)
            .turn(vec![
                StreamEvent::TextStart { index: 0 },
                StreamEvent::TextDelta {
                    index: 0,
                    text: "hello".to_owned(),
                },
                StreamEvent::Logprobs {
                    index: 0,
                    entries: vec![TokenLogprob {
                        token: "hello".to_owned(),
                        logprob: LogprobValue::new(-0.25),
                        bytes: Some(vec![104]),
                        alternatives: Vec::new(),
                        extension: std::collections::BTreeMap::new(),
                    }],
                },
                StreamEvent::Finish(FinishReason::Stop),
            ]),
    );
    let harness = fixture(scripted, ToolRegistry::new(), HarnessConfig::default());
    let session = harness.engine.create().await.expect("create");
    let outcome = harness
        .engine
        .turn(session.id)
        .input(Message::user("go"))
        .logprobs(2)
        .start()
        .finish()
        .await
        .expect("turn");

    let head = outcome.session.head.expect("head");
    let message = harness.store.node(head).await.expect("node").message;
    assert_eq!(
        serde_json::to_vec(&message.parts).expect("serialize"),
        serde_json::to_vec(&plain_message.parts).expect("serialize"),
        "log probabilities never enter the message parts"
    );
    assert_ne!(
        message
            .origin
            .as_ref()
            .and_then(|origin| origin.request_digest.as_ref()),
        plain_message
            .origin
            .as_ref()
            .and_then(|origin| origin.request_digest.as_ref()),
        "the digest hashes the request, so it does record that the turn asked"
    );
}

#[tokio::test]
async fn an_interceptor_can_refuse_a_tool_call() {
    struct RefuseEcho;

    #[async_trait::async_trait]
    impl TurnInterceptor for RefuseEcho {
        fn name(&self) -> &'static str {
            "refuse_echo"
        }

        async fn intercept(&self, context: &InterceptionContext<'_>) -> Result<Interception> {
            if let ConsultationPoint::ToolCall { call } = &context.point {
                if call.name == "echo" {
                    return Ok(Interception::RefuseCall {
                        reason: "echo is refused by policy".to_owned(),
                    });
                }
            }
            Ok(Interception::Continue)
        }
    }

    let call_id = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_tool_call(call_id, "echo", &json!({ "text": "hello" }))
            .turn_text("understood"),
    );
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");
    let store = Arc::new(InMemoryStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let engine = HarnessBuilder::new()
        .store(store.clone())
        .blobs(blobs)
        .model(scripted.clone())
        .tools(tools)
        .interceptors(vec![Arc::new(RefuseEcho)])
        .build()
        .expect("build engine");

    let session = engine.create().await.expect("create");
    let outcome = engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");

    assert_eq!(outcome.session.phase, Phase::Idle);
    let history = store.history(outcome.session.head).await.expect("history");
    let tool_msg = history
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("tool message");
    let results = tool_msg.tool_results();
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    let error_text = results[0]
        .parts
        .iter()
        .find_map(Part::as_text)
        .expect("error text");
    assert_eq!(error_text, "echo is refused by policy");
}
