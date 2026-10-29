//! Dedicated integration scenarios (FR-TEST-4): cancellation, concurrent writers
//! and process death are exercised on their own, not buried in unit tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use norte_core::{
    CallId, Error, FinishReason, FnExecutor, Message, MessageStatus, Result, Role, SessionId,
    StreamEvent, ToolContext, ToolRegistry,
};
use norte_harness::Store;
use norte_harness::{Commit, EngineEvent, Node, Phase};
use norte_harness::{HarnessBuilder, HarnessConfig};
use norte_testing::{InMemoryBlobStore, InMemoryStore, ScriptedModel, model};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

fn connection() -> Arc<InMemoryStore> {
    Arc::new(InMemoryStore::new())
}

fn session_commit(session_id: SessionId) -> Commit {
    Commit {
        session_id,
        expected_version: None,
        nodes: Vec::new(),
        head: None,
        phase: Phase::Idle,
        turn: 0,
        metadata: None,
    }
}

#[tokio::test]
async fn cancellation_interrupts_and_leaves_the_session_resumable() {
    let reference = model("test/echo");
    let events = vec![
        StreamEvent::TextStart { index: 0 },
        StreamEvent::TextDelta {
            index: 0,
            text: "a".to_owned(),
        },
        StreamEvent::TextDelta {
            index: 0,
            text: "b".to_owned(),
        },
        StreamEvent::TextDelta {
            index: 0,
            text: "c".to_owned(),
        },
        StreamEvent::Finish(FinishReason::Stop),
    ];
    let scripted = Arc::new(
        ScriptedModel::new(reference)
            .turn_delayed(events, Duration::from_millis(30))
            .turn_text("resumed"),
    );
    let engine = HarnessBuilder::new()
        .store(connection())
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(scripted)
        .build()
        .expect("engine");
    let session = engine.create().await.expect("create");

    let mut stream = engine.send(session.id, Message::user("go"));
    let _ = stream.next().await;
    stream.cancel();
    let outcome = stream.finish().await.expect("turn");

    let status = outcome
        .message
        .as_ref()
        .and_then(|message| message.origin.as_ref())
        .map(|origin| origin.status);
    assert_eq!(status, Some(MessageStatus::Interrupted));
    assert_eq!(outcome.session.phase, Phase::Idle);

    let resumed = engine
        .send(session.id, Message::user("again"))
        .finish()
        .await
        .expect("resumed turn");
    assert_eq!(
        resumed.message.as_ref().map(Message::text).as_deref(),
        Some("resumed")
    );
}

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
struct WaitArgs {}

#[derive(Debug, Serialize, Deserialize)]
struct WaitResult {
    cancelled: bool,
}

#[derive(Debug)]
#[norte_core::tool(name = "wait", description = "Waits until the turn is cancelled")]
struct Wait {
    started: Arc<Notify>,
    observed: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl FnExecutor<WaitArgs, WaitResult> for Wait {
    async fn call(&self, _args: WaitArgs) -> Result<WaitResult> {
        Ok(WaitResult { cancelled: false })
    }

    async fn call_with_context(
        &self,
        context: &ToolContext,
        _args: WaitArgs,
    ) -> Result<WaitResult> {
        self.started.notify_one();
        context.cancelled().await;
        self.observed.store(true, Ordering::SeqCst);
        Ok(WaitResult { cancelled: true })
    }
}

#[tokio::test]
async fn cancelling_a_turn_reaches_the_running_tool() {
    let store = connection();
    let started = Arc::new(Notify::new());
    let observed = Arc::new(AtomicBool::new(false));
    let mut tools = ToolRegistry::new();
    tools
        .register_declarator(&Wait {
            started: Arc::clone(&started),
            observed: Arc::clone(&observed),
        })
        .expect("register wait");

    let call_id = CallId::new();
    let scripted = Arc::new(ScriptedModel::new(model("test/wait")).turn(vec![
        StreamEvent::ToolCallStart {
            index: 0,
            call_id,
            name: "wait".to_owned(),
        },
        StreamEvent::ToolCallDelta {
            index: 0,
            call_id,
            name: None,
            arguments: "{}".to_owned(),
        },
        StreamEvent::ToolCallDone { index: 0 },
        StreamEvent::Finish(FinishReason::ToolCalls),
    ]));
    let engine = HarnessBuilder::new()
        .store(Arc::clone(&store) as Arc<dyn Store>)
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(scripted)
        .tools(tools)
        .build()
        .expect("engine");
    let session = engine.create().await.expect("create");

    let mut stream = engine.send(session.id, Message::user("go"));
    loop {
        if let EngineEvent::ToolCallStarted { .. } =
            stream.next().await.expect("event").expect("ok")
        {
            break;
        }
    }
    started.notified().await;
    stream.cancel();
    let outcome = stream.finish().await.expect("turn");

    assert!(
        observed.load(Ordering::SeqCst),
        "the tool must see the turn's cancellation token"
    );
    assert_eq!(outcome.session.phase, Phase::Idle);

    let head = outcome.session.head.expect("head");
    let history = store.history(Some(head)).await.expect("history");
    let assistant = history
        .iter()
        .find(|message| message.role == Role::Assistant)
        .expect("the assistant message");
    assert!(assistant.has_tool_calls(), "the call is still recorded");
    let answered = history
        .iter()
        .flat_map(|message| message.tool_results())
        .filter(|result| result.call_id == call_id)
        .count();
    assert_eq!(
        answered, 1,
        "the cancelled call is answered, so the tool loop is closed"
    );
}

#[tokio::test]
async fn cancelling_a_turn_reaches_an_early_tool() {
    let store = connection();
    let started = Arc::new(Notify::new());
    let observed = Arc::new(AtomicBool::new(false));
    let mut tools = ToolRegistry::new();
    tools
        .register_declarator(&Wait {
            started: Arc::clone(&started),
            observed: Arc::clone(&observed),
        })
        .expect("register wait");

    let call_id = CallId::new();
    let scripted = Arc::new(ScriptedModel::new(model("test/wait")).turn(vec![
        StreamEvent::ToolCallStart {
            index: 0,
            call_id,
            name: "wait".to_owned(),
        },
        StreamEvent::ToolCallDelta {
            index: 0,
            call_id,
            name: None,
            arguments: "{}".to_owned(),
        },
        StreamEvent::ToolCallDone { index: 0 },
        StreamEvent::Finish(FinishReason::ToolCalls),
    ]));
    let engine = HarnessBuilder::new()
        .store(Arc::clone(&store) as Arc<dyn Store>)
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(scripted)
        .tools(tools)
        .config(HarnessConfig {
            early_tool_execution: true,
            ..HarnessConfig::default()
        })
        .build()
        .expect("engine");
    let session = engine.create().await.expect("create");

    let mut stream = engine.send(session.id, Message::user("go"));
    loop {
        if let EngineEvent::ToolCallStarted { .. } =
            stream.next().await.expect("event").expect("ok")
        {
            break;
        }
    }
    started.notified().await;
    stream.cancel();
    let _ = stream.finish().await.expect("turn");

    assert!(
        observed.load(Ordering::SeqCst),
        "an early tool must see the same cancellation token"
    );
}

/// A cancellation that lands after the model streamed a tool call but before
/// any tool ran must still answer that call.
///
/// The session goes idle on an interrupt, not parked, so the tool loop never
/// reaches the call: nothing would run it and nothing would answer it. The call
/// is committed unanswered — the same shape a person hit when they cancelled a
/// turn mid-stream and the session then refused every model switch with
/// `cannot replay vendor continuity across models while a tool result is
/// pending`, or the next turn landed below an open call.
#[tokio::test]
async fn cancelling_mid_stream_answers_the_tool_call_it_committed() {
    let store = connection();
    let call_id = CallId::new();
    // No tool is registered: the call is answered by the engine itself, not run.
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_delayed(
        vec![
            StreamEvent::ToolCallStart {
                index: 0,
                call_id,
                name: "noop".to_owned(),
            },
            StreamEvent::ToolCallDelta {
                index: 0,
                call_id,
                name: None,
                arguments: "{}".to_owned(),
            },
            StreamEvent::ToolCallDone { index: 0 },
            StreamEvent::Finish(FinishReason::ToolCalls),
        ],
        Duration::from_millis(20),
    ));
    let engine = HarnessBuilder::new()
        .store(Arc::clone(&store) as Arc<dyn Store>)
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(scripted)
        .build()
        .expect("engine");
    let session = engine.create().await.expect("create");

    let mut stream = engine.send(session.id, Message::user("go"));
    // Cancel the moment the call is complete, before the stream finishes and
    // before any tool runs.
    loop {
        match stream.next().await.expect("event").expect("ok") {
            EngineEvent::PartCompleted { .. } => break,
            EngineEvent::ToolCallStarted { .. } => {
                panic!("no tool must run before the cancel");
            }
            _ => {}
        }
    }
    stream.cancel();
    let outcome = stream.finish().await.expect("turn");

    assert_eq!(outcome.session.phase, Phase::Idle);
    let head = outcome.session.head.expect("head");
    let history = store.history(Some(head)).await.expect("history");
    let assistant = history
        .iter()
        .find(|message| message.role == Role::Assistant)
        .expect("the assistant message");
    assert!(assistant.has_tool_calls(), "the call is recorded");
    let answered = history
        .iter()
        .flat_map(|message| message.tool_results())
        .filter(|result| result.call_id == call_id)
        .count();
    assert_eq!(
        answered, 1,
        "the interrupted call is answered, not left open"
    );
}

#[tokio::test]
async fn concurrent_writers_commit_exactly_one() {
    let store = connection();
    let session = store
        .commit(session_commit(SessionId::new()))
        .await
        .expect("create");

    let writers = (0..8).map(|index| {
        let store = Arc::clone(&store);
        async move {
            store
                .commit(Commit {
                    session_id: session.id,
                    expected_version: Some(session.version),
                    nodes: vec![Node::message(
                        Message::user(format!("writer {index}")),
                        session.head,
                    )],
                    head: None,
                    phase: Phase::Idle,
                    turn: 1,
                    metadata: None,
                })
                .await
        }
    });
    let results = futures_util::future::join_all(writers).await;

    let successes = results.iter().filter(|result| result.is_ok()).count();
    assert_eq!(successes, 1, "exactly one writer may win");
    for result in &results {
        if let Err(error) = result {
            assert!(matches!(error, Error::StoreConflict { .. }));
        }
    }
}
