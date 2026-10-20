//! Engine integration tests driven by the scripted model.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use norte_core::{
    CallId, ContinuityFragment, ContinuityScope, ContinuitySemantics, Decision, Engine,
    EngineBuilder, EngineConfig, EngineEvent, Error, Feature, FinishReason, FnExecutor,
    LogprobValue, Message, MessageStatus, Part, Phase, Policy, PolicySet, PolicyView, Result, Role,
    Store, StreamEvent, TokenLogprob, ToolApprovalPolicy, ToolContext, ToolName, ToolRegistry,
    ToolResultPart, WireFamily,
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

#[derive(Debug)]
#[norte_core::tool(name = "plain", description = "A tool nobody has to approve")]
struct Plain {
    hits: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl FnExecutor<EchoArgs, EchoResult> for Plain {
    async fn call(&self, args: EchoArgs) -> Result<EchoResult> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        Ok(EchoResult { echoed: args.text })
    }
}

#[derive(Debug)]
#[norte_core::tool(name = "guarded", description = "A tool the operator must approve")]
struct Guarded {
    hits: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl FnExecutor<EchoArgs, EchoResult> for Guarded {
    async fn call(&self, args: EchoArgs) -> Result<EchoResult> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        Ok(EchoResult { echoed: args.text })
    }
}

struct Rewrite;

impl Policy for Rewrite {
    fn name(&self) -> &'static str {
        "rewrite"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        let text = view
            .call
            .arguments
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        Decision::Transform(json!({ "text": format!("{text}!") }))
    }
}

#[derive(Debug)]
#[norte_core::tool(
    name = "contextual",
    description = "Records the host context it ran with"
)]
struct Contextual {
    seen: Arc<Mutex<Vec<BTreeMap<String, serde_json::Value>>>>,
}

#[async_trait::async_trait]
impl FnExecutor<EchoArgs, EchoResult> for Contextual {
    async fn call(&self, args: EchoArgs) -> Result<EchoResult> {
        Ok(EchoResult { echoed: args.text })
    }

    async fn call_with_context(&self, context: &ToolContext, args: EchoArgs) -> Result<EchoResult> {
        self.seen
            .lock()
            .expect("lock")
            .push(context.metadata.clone());
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

/// Two counting tools and a policy that gates `guarded`, so a message can run
/// one call and then park on the next.
fn counting_tools() -> (ToolRegistry, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let plain = Arc::new(AtomicUsize::new(0));
    let guarded = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools
        .register_declarator(&Plain {
            hits: Arc::clone(&plain),
        })
        .expect("register plain");
    tools
        .register_declarator(&Guarded {
            hits: Arc::clone(&guarded),
        })
        .expect("register guarded");
    (tools, plain, guarded)
}

fn guarded_policy() -> PolicySet {
    PolicySet::new(vec![Arc::new(ToolApprovalPolicy::new(
        "guard",
        vec![ToolName::new("guarded").expect("tool")],
        "guarded needs a human",
    ))])
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

#[tokio::test]
async fn a_park_after_a_call_does_not_run_that_call_again() {
    let first = CallId::new();
    let second = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/plain"))
            .turn(two_call_turn(
                (first, "plain", "one"),
                (second, "guarded", "two"),
            ))
            .turn_text("done"),
    );
    let (tools, plain_hits, guarded_hits) = counting_tools();
    let harness = harness(scripted, tools, guarded_policy());

    let session = harness.engine.create().await.expect("create");
    let outcome = harness
        .engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");
    assert_eq!(outcome.session.phase, Phase::AwaitingApproval);
    assert_eq!(
        plain_hits.load(Ordering::SeqCst),
        1,
        "the call before the park ran once"
    );
    assert_eq!(
        guarded_hits.load(Ordering::SeqCst),
        0,
        "the gated call has not run"
    );

    // The result the message already produced travels with the park commit, so a
    // resume seeds it instead of running the call a second time (FR-APPR-8).
    let stored: Vec<ToolResultPart> = serde_json::from_value(
        outcome
            .session
            .metadata
            .get(norte_core::PARTIAL_TOOLS_KEY)
            .expect("the park commit records the results already produced")
            .clone(),
    )
    .expect("partial results decode");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].call_id, first);
    assert_eq!(stored[0].structured, Some(json!({ "echoed": "one" })));

    let pending = harness
        .engine
        .pending_approvals(session.id)
        .await
        .expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].call_id, second);
    harness
        .engine
        .approve(
            pending[0].id,
            norte_core::GrantScope::Call(second),
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
    assert_eq!(guarded_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        plain_hits.load(Ordering::SeqCst),
        1,
        "resuming must not re-run the call that ran before the park"
    );
}

#[tokio::test]
async fn early_tool_execution_does_not_run_a_call_twice_across_a_park() {
    let first = CallId::new();
    let second = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/plain"))
            .turn(two_call_turn(
                (first, "plain", "one"),
                (second, "guarded", "two"),
            ))
            .turn_text("done"),
    );
    let (tools, plain_hits, guarded_hits) = counting_tools();
    let config = EngineConfig {
        early_tool_execution: true,
        ..EngineConfig::default()
    };
    let harness = harness_with_config(scripted, tools, guarded_policy(), config);

    let session = harness.engine.create().await.expect("create");
    let outcome = harness
        .engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");
    assert_eq!(outcome.session.phase, Phase::AwaitingApproval);
    assert_eq!(
        plain_hits.load(Ordering::SeqCst),
        1,
        "the early call ran during the stream"
    );

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
            norte_core::GrantScope::Call(second),
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
    assert_eq!(guarded_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        plain_hits.load(Ordering::SeqCst),
        1,
        "an early call must not run again after the park"
    );

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
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].call_id, first, "results keep call order");
    assert_eq!(results[1].call_id, second);
    assert!(
        !outcome.session.metadata.contains_key("norte.partial_tools"),
        "the parked-results key is cleared once the tool message commits"
    );
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
        .approve(record.id, norte_core::GrantScope::Call(call_id), "tester")
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
    assert!(
        !outcome.session.grants.is_empty(),
        "the approval leaves a grant"
    );
}

#[tokio::test]
async fn a_later_transform_cannot_bypass_an_earlier_approval() {
    let call_id = CallId::new();
    let hits = Arc::new(AtomicUsize::new(0));
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_tool_call(call_id, "plain", &json!({ "text": "secret" }))
            .turn_text("finished"),
    );
    let mut tools = ToolRegistry::new();
    tools
        .register_declarator(&Plain {
            hits: Arc::clone(&hits),
        })
        .expect("register plain");
    let policies = PolicySet::new(vec![
        Arc::new(ToolApprovalPolicy::new(
            "guard",
            vec![ToolName::new("plain").expect("tool")],
            "plain touches the world",
        )),
        Arc::new(Rewrite),
    ]);
    let harness = harness(scripted, tools, policies);

    let session = harness.engine.create().await.expect("create");
    let outcome = harness
        .engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");
    assert_eq!(outcome.session.phase, Phase::AwaitingApproval);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "a transform after an approval must not run the call"
    );

    let pending = harness
        .engine
        .pending_approvals(session.id)
        .await
        .expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].arguments, Some(json!({ "text": "secret!" })));

    harness
        .engine
        .approve(
            pending[0].id,
            norte_core::GrantScope::Call(call_id),
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
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let history = harness
        .store
        .history(outcome.session.head)
        .await
        .expect("history");
    let result = history[2].tool_results()[0];
    assert_eq!(result.structured, Some(json!({ "echoed": "secret!" })));
}

#[tokio::test]
async fn an_earlier_transform_is_what_the_human_approves_and_runs() {
    let call_id = CallId::new();
    let hits = Arc::new(AtomicUsize::new(0));
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_tool_call(call_id, "plain", &json!({ "text": "secret" }))
            .turn_text("finished"),
    );
    let mut tools = ToolRegistry::new();
    tools
        .register_declarator(&Plain {
            hits: Arc::clone(&hits),
        })
        .expect("register plain");
    let policies = PolicySet::new(vec![
        Arc::new(Rewrite),
        Arc::new(ToolApprovalPolicy::new(
            "guard",
            vec![ToolName::new("plain").expect("tool")],
            "plain touches the world",
        )),
    ]);
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
    assert_eq!(pending[0].arguments, Some(json!({ "text": "secret!" })));
    harness
        .engine
        .approve(
            pending[0].id,
            norte_core::GrantScope::Call(call_id),
            "tester",
        )
        .await
        .expect("approve");
    harness
        .engine
        .resume(session.id)
        .finish()
        .await
        .expect("resume");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_host_context_reaches_a_tool_that_resumes_after_approval() {
    let call_id = CallId::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_tool_call(call_id, "contextual", &json!({ "text": "hello" }))
            .turn_text("done"),
    );
    let mut tools = ToolRegistry::new();
    tools
        .register_declarator(&Contextual {
            seen: Arc::clone(&seen),
        })
        .expect("register contextual");
    let policies = PolicySet::new(vec![Arc::new(ToolApprovalPolicy::new(
        "guard",
        vec![ToolName::new("contextual").expect("tool")],
        "contextual touches the world",
    ))]);

    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(scripted)
        .tools(tools)
        .policies(policies)
        .tool_metadata(|info| {
            BTreeMap::from([
                ("tenant".to_owned(), json!("acme")),
                ("tool".to_owned(), json!(info.tool.as_str())),
                ("turn".to_owned(), json!(info.turn)),
                ("arguments".to_owned(), info.arguments.clone()),
            ])
        })
        .build()
        .expect("build engine");

    let session = engine.create().await.expect("create");
    let outcome = engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");
    assert_eq!(outcome.session.phase, Phase::AwaitingApproval);
    assert!(
        seen.lock().expect("lock").is_empty(),
        "the parked call has not run yet"
    );

    let pending = engine.pending_approvals(session.id).await.expect("pending");
    engine
        .approve(
            pending[0].id,
            norte_core::GrantScope::Call(call_id),
            "tester",
        )
        .await
        .expect("approve");
    engine.resume(session.id).finish().await.expect("resume");

    let seen = seen.lock().expect("lock");
    assert_eq!(seen.len(), 1, "the resumed call ran exactly once");
    assert_eq!(seen[0]["tenant"], json!("acme"));
    assert_eq!(seen[0]["tool"], json!("contextual"));
    assert_eq!(seen[0]["arguments"], json!({ "text": "hello" }));
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
            norte_core::GrantScope::Tool(ToolName::new("echo").expect("tool")),
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
        .approve(approval, norte_core::GrantScope::Call(call_id), "tester")
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
            norte_core::GrantScope::Tool(ToolName::new("echo").expect("tool")),
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
    let failing = Arc::new(norte_testing::FailingModel::new(model("test/echo"), "boom"));
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

/// A turn that failed mid-flight stays resumable: the user message is already
/// committed, the session sits in `Streaming`, and `resume` picks the turn up
/// without asking the same question twice. This is what a host does after a
/// transient provider error (`norte_http::is_transient`).
#[tokio::test]
async fn a_failed_turn_is_resumed_where_it_stopped() {
    let store = Arc::new(InMemoryStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let failing = EngineBuilder::new()
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
    let resumed = EngineBuilder::new()
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

/// Denies a call once an earlier call in the same message has already run, so a
/// test can observe what `PolicyView::results` carries.
struct ResultGate;

impl Policy for ResultGate {
    fn name(&self) -> &'static str {
        "result-gate"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        if view.results.is_empty() {
            return Decision::Allow;
        }
        Decision::Deny("this request already ran a tool".to_owned())
    }
}

#[tokio::test]
async fn a_policy_sees_the_results_of_earlier_calls_in_the_same_message() {
    let first = CallId::new();
    let second = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn(two_call_turn(
                (first, "echo", "one"),
                (second, "echo", "two"),
            ))
            .turn_text("done"),
    );
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");
    let policies = PolicySet::new(vec![Arc::new(ResultGate)]);
    let harness = harness(scripted, tools, policies);

    let session = harness.engine.create().await.expect("create");
    let outcome = harness
        .engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");
    assert_eq!(outcome.session.phase, Phase::Idle);

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
    assert!(!results[0].is_error, "the first call ran");
    assert_eq!(results[0].structured, Some(json!({ "echoed": "one" })));
    assert!(results[1].is_error, "the second call was denied");
    let reason = results[1]
        .parts
        .iter()
        .find_map(Part::as_text)
        .expect("denial reason");
    assert_eq!(reason, "this request already ran a tool");
}

/// What one policy evaluation saw, recorded so a test can compare paths.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    results: usize,
    history: usize,
}

/// Records the view every evaluation received, and allows.
#[derive(Clone, Default)]
struct Probe {
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Policy for Probe {
    fn name(&self) -> &'static str {
        "probe"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        self.seen.lock().expect("probe").push(Seen {
            results: view.results.len(),
            history: view.history.len(),
        });
        Decision::Allow
    }
}

async fn probe_seen(config: EngineConfig) -> Vec<Seen> {
    let first = CallId::new();
    let second = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn(two_call_turn(
                (first, "echo", "one"),
                (second, "echo", "two"),
            ))
            .turn_text("done"),
    );
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");
    let probe = Probe::default();
    let seen = Arc::clone(&probe.seen);
    let policies = PolicySet::new(vec![Arc::new(probe)]);
    let harness = harness_with_config(scripted, tools, policies, config);

    let session = harness.engine.create().await.expect("create");
    harness
        .engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");
    seen.lock().expect("probe").clone()
}

#[tokio::test]
async fn the_results_a_policy_sees_depend_on_the_execution_path() {
    let sequential = probe_seen(EngineConfig::default()).await;
    assert_eq!(sequential.len(), 2);
    assert_eq!(
        sequential[0].results, 0,
        "nothing ran before the first call"
    );
    assert_eq!(
        sequential[1].results, 1,
        "the second call sees the first one"
    );
    assert_eq!(
        sequential[0].history, 2,
        "the request history is the input plus the assistant message"
    );
    assert_eq!(sequential[1].history, 2);

    let parallel = probe_seen(EngineConfig {
        early_tool_execution: true,
        ..EngineConfig::default()
    })
    .await;
    assert_eq!(parallel.len(), 2);
    assert!(
        parallel.iter().all(|seen| seen.results == 0),
        "early dispatch starts before anything has run"
    );
    assert!(
        parallel.iter().all(|seen| seen.history == 1),
        "early dispatch sees the input only; the message is still in flight"
    );
}

#[tokio::test]
async fn logprobs_without_the_capability_fail_before_the_request() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("never"));
    let harness = harness(scripted, ToolRegistry::new(), PolicySet::default());
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
    let harness = harness(scripted, ToolRegistry::new(), PolicySet::default());
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
    let baseline = harness(plain, ToolRegistry::new(), PolicySet::default());
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
    let harness = harness(scripted, ToolRegistry::new(), PolicySet::default());
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
