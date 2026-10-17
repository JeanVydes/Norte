//! Tests for the array-to-graph bridge (FR-ENG-6): a host-held `Vec<Message>`
//! is committed as durable history without generating.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use rustychain_core::{
    Commit, Engine, EngineBuilder, EngineEvent, Error, Message, Observer, Phase, SessionId, Store,
};
use rustychain_testing::{
    InMemoryBlobStore, InMemoryStore, RecordingObserver, ScriptedModel, model,
};

fn build(
    scripted: Arc<ScriptedModel>,
    store: Arc<InMemoryStore>,
    observers: Vec<Arc<dyn Observer>>,
) -> Engine {
    EngineBuilder::new()
        .store(store)
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(scripted)
        .observers(observers)
        .build()
        .expect("engine")
}

async fn wait_for(mut predicate: impl FnMut() -> bool) {
    for _ in 0..500 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the condition never became true");
}

#[tokio::test]
async fn seeded_history_round_trips_and_keeps_the_prefix() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("ok"));
    let recorder = Arc::new(RecordingObserver::new("history"));
    let store = Arc::new(InMemoryStore::new());
    let engine = build(scripted.clone(), store, vec![recorder.clone()]);

    let session = engine.create().await.expect("create");
    let seeded = vec![
        Message::system("You are terse."),
        Message::user("hola"),
        Message::assistant("hola!"),
    ];

    let after = engine
        .append_history(session.id, seeded.clone())
        .await
        .expect("append");
    assert!(after.head.is_some());
    assert_eq!(engine.history(session.id).await.expect("history"), seeded);

    // The next turn continues the seeded prefix byte-identically (FR-RECON-2).
    let outcome = engine
        .send(session.id, Message::user("sigue"))
        .finish()
        .await
        .expect("send");
    assert_eq!(
        outcome.message.as_ref().map(Message::text).as_deref(),
        Some("ok")
    );

    let requests = scripted.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(&requests[0].messages[..seeded.len()], seeded.as_slice());
    assert_eq!(requests[0].messages.len(), seeded.len() + 1);

    // The append is reported (FR-EVT-1).
    wait_for(|| {
        recorder.events().iter().any(|event| {
            matches!(event, EngineEvent::HistoryAppended { messages, .. } if *messages == 3)
        })
    })
    .await;
}

#[tokio::test]
async fn an_empty_append_is_a_no_op() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("unused"));
    let store = Arc::new(InMemoryStore::new());
    let engine = build(scripted, store, Vec::new());

    let session = engine.create().await.expect("create");
    let before = engine.session(session.id).await.expect("session");
    let after = engine
        .append_history(session.id, Vec::new())
        .await
        .expect("append");
    assert_eq!(after.version, before.version);
    assert!(engine.history(session.id).await.expect("history").is_empty());
}

#[tokio::test]
async fn appending_outside_idle_is_rejected() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("unused"));
    let store = Arc::new(InMemoryStore::new());
    let engine = build(scripted, store.clone(), Vec::new());

    // A session mid-turn: the head belongs to a running generation.
    let session = store
        .commit(Commit {
            session_id: SessionId::new(),
            expected_version: None,
            nodes: Vec::new(),
            head: None,
            phase: Phase::Streaming,
            turn: 0,
            queue: VecDeque::default(),
            grants: Vec::new(),
            metadata: None,
        })
        .await
        .expect("create");

    let error = engine
        .append_history(session.id, vec![Message::user("x")])
        .await
        .expect_err("must be rejected");
    assert!(matches!(error, Error::InvalidInput(_)));
}
