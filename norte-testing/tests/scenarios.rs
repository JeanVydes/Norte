//! Dedicated integration scenarios (FR-TEST-4): cancellation, concurrent writers
//! and process death are exercised on their own, not buried in unit tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use norte_core::{
    CallId, Commit, EngineBuilder, EngineConfig, EngineEvent, Error, FinishReason, FnExecutor,
    Message, MessageStatus, Node, Phase, Result, Role, SessionId, Store, StreamEvent, ToolContext,
    ToolRegistry,
};
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
        grants: Vec::new(),
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
    let engine = EngineBuilder::new()
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
    let engine = EngineBuilder::new()
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
    assert_eq!(outcome.session.phase, Phase::AwaitingTool);

    let head = outcome.session.head.expect("head");
    let message = store.node(head).await.expect("node").message;
    assert_eq!(message.role, Role::Assistant);
    assert!(message.has_tool_calls());
    assert!(
        message.tool_results().is_empty(),
        "a cancelled tool leaves no committed result"
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
    let engine = EngineBuilder::new()
        .store(Arc::clone(&store) as Arc<dyn Store>)
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(scripted)
        .tools(tools)
        .config(EngineConfig {
            early_tool_execution: true,
            ..EngineConfig::default()
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
                    grants: Vec::new(),
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
