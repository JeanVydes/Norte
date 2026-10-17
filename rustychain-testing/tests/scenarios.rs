//! Dedicated integration scenarios (FR-TEST-4): cancellation, concurrent writers
//! and process death are exercised on their own, not buried in unit tests.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rustychain_core::{
    Commit, EngineBuilder, Error, FinishReason, Message, MessageStatus, Node, Phase, SessionId,
    Store, StreamEvent,
};
use rustychain_testing::{InMemoryBlobStore, InMemoryStore, ScriptedModel, model};

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
        queue: VecDeque::default(),
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
                    queue: VecDeque::default(),
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
