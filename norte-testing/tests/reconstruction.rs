//! Reconstruction at the edges of the cache prefix: a head that moves to
//! another branch (FR-RECON-8) and content the host keeps out of the cached
//! prefix (FR-RECON-5, FR-RECON-6).
//!
//! Both invariants are about money. A branch must not re-bill its ancestors, and
//! a volatile message must not shift a prefix the provider already cached, so
//! the assertions compare serialized bytes rather than modelled values.

use std::sync::Arc;

use norte_core::{CacheClass, Commit, Message, ModelRequest, Node, Phase, Store};
use norte_harness::{Engine, EngineBuilder};
use norte_testing::{InMemoryBlobStore, InMemoryStore, ScriptedModel, model};

fn engine(scripted: Arc<ScriptedModel>, store: Arc<InMemoryStore>) -> Engine {
    EngineBuilder::new()
        .store(store)
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(scripted)
        .build()
        .expect("engine")
}

fn json(messages: &[Message]) -> Vec<u8> {
    serde_json::to_vec(messages).expect("json")
}

/// Seeds one session with a fixed history and generates from its head, so the
/// recorded request is the reconstruction of exactly those messages.
async fn resumed_request(messages: Vec<Message>) -> ModelRequest {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("ok"));
    let engine = engine(scripted.clone(), Arc::new(InMemoryStore::new()));
    let session = engine.create().await.expect("create");
    engine
        .append_history(session.id, messages)
        .await
        .expect("seed");
    engine.resume(session.id).finish().await.expect("turn");
    scripted
        .requests()
        .into_iter()
        .next()
        .expect("the engine reconstructed a request")
}

#[tokio::test]
async fn a_branch_from_an_ancestor_reproduces_the_shared_prefix_byte_for_byte() {
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_text("first answer")
            .turn_text("second answer")
            .turn_text("branch answer"),
    );
    let store = Arc::new(InMemoryStore::new());
    let engine = engine(scripted.clone(), store.clone());
    let session = engine.create().await.expect("create");

    engine
        .send(session.id, Message::user("one"))
        .finish()
        .await
        .expect("first turn");
    let after_first = engine.session(session.id).await.expect("session");
    let fork_point = after_first.head.expect("head after the first turn");

    engine
        .send(session.id, Message::user("two"))
        .finish()
        .await
        .expect("second turn");
    let after_second = engine.session(session.id).await.expect("session");

    // The store is the branching seam: a node whose parent is an ancestor of the
    // active branch becomes the head. There is no separate rewind call, and the
    // ancestor itself is never rewritten (FR-GRAPH-2, FR-RECON-8).
    let branch = Node::message(Message::user("branch"), Some(fork_point));
    let branch_head = branch.id;
    store
        .commit(Commit {
            session_id: session.id,
            expected_version: Some(after_second.version),
            nodes: vec![branch],
            head: Some(branch_head),
            phase: Phase::Idle,
            turn: after_second.turn,
            grants: after_second.grants.clone(),
            metadata: None,
        })
        .await
        .expect("branch commit");

    // Resuming an idle session generates from the new head.
    engine
        .resume(session.id)
        .finish()
        .await
        .expect("branch turn");

    let requests = scripted.requests();
    assert_eq!(requests.len(), 3);
    let first = &requests[0];
    let second = &requests[1];
    let branch_turn = &requests[2];
    assert_eq!(first.messages.len(), 1);
    assert_eq!(second.messages.len(), 3);
    assert_eq!(branch_turn.messages.len(), 3);

    // The shared ancestors are reproduced byte for byte, not merely modelled
    // again: the provider sees the exact prefix it already cached.
    assert_eq!(
        json(&branch_turn.messages[..2]),
        json(&second.messages[..2]),
        "the common ancestor prefix must be byte-identical"
    );
    assert_eq!(
        json(&branch_turn.messages[..1]),
        json(&first.messages),
        "the prefix is stable all the way down to the root"
    );
    assert_eq!(branch_turn.messages[2].text(), "branch");
    assert_ne!(
        branch_turn.messages[2], second.messages[2],
        "only the divergent suffix differs"
    );

    // The fork is durable graph state: one ancestor, two children.
    let children = store.children(fork_point).await.expect("children");
    assert_eq!(children.len(), 2);
    assert!(children.contains(&branch_head));
    let head = engine.session(session.id).await.expect("session").head;
    let ancestors = store
        .ancestors(head.expect("active head"))
        .await
        .expect("ancestors");
    assert!(
        ancestors.contains(&fork_point),
        "the active branch still descends from the fork point"
    );
}

#[tokio::test]
async fn an_ephemeral_message_is_materialized_after_the_critical_prefix() {
    let critical = resumed_request(vec![Message::system("sys"), Message::user("hi")]).await;
    let volatile = Message::system("run=42").with_cache(CacheClass::Ephemeral);
    let with_tail = resumed_request(vec![
        Message::system("sys"),
        Message::user("hi"),
        volatile.clone(),
    ])
    .await;

    assert_eq!(with_tail.messages.len(), 3);
    assert_eq!(
        with_tail.messages.last(),
        Some(&volatile),
        "the ephemeral message is materialized last, never interleaved"
    );
    assert_eq!(
        json(&with_tail.messages[..2]),
        json(&critical.messages),
        "the critical prefix is exactly the request without the tail"
    );
    assert_eq!(
        with_tail.cache_boundaries, critical.cache_boundaries,
        "a boundary never depends on the tail"
    );
}

#[tokio::test]
async fn changing_the_ephemeral_tail_does_not_move_the_critical_prefix() {
    let one = resumed_request(vec![
        Message::system("sys"),
        Message::user("hi"),
        Message::system("run=1").with_cache(CacheClass::Ephemeral),
    ])
    .await;
    let two = resumed_request(vec![
        Message::system("sys"),
        Message::user("hi"),
        Message::system("run=2").with_cache(CacheClass::Ephemeral),
    ])
    .await;

    assert_eq!(
        json(&one.messages[..2]),
        json(&two.messages[..2]),
        "volatile content must not shift the cached prefix"
    );
    assert_ne!(one.messages[2], two.messages[2]);
    assert_eq!(one.cache_boundaries, two.cache_boundaries);
}
