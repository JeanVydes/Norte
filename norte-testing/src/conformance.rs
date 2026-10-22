use chrono::Duration;

use norte_core::{
    ApprovalRecord, CallId, Commit, ContinuityFragment, ContinuityScope, ContinuitySemantics,
    Error, Message, Model, ModelRef, Node, NodeId, Phase, ReplayDecision, SessionId, Store,
    ToolName,
};

/// Runs the [`Model`] contract every adapter must satisfy (FR-TEST-1).
///
/// The suite is socket-free: it checks the trait surface an adapter reports and
/// the determinism of its [`norte_core::ContinuityAdapter`], so it can run
/// in the hermetic test suite against every wire.
///
/// # Panics
///
/// Panics when the model violates the contract.
pub fn model_conformance(model: &dyn Model, expected: &ModelRef) {
    assert_eq!(
        model.reference(),
        expected,
        "the model must report the reference it was built with"
    );

    let capabilities = model.capabilities();
    assert!(
        !capabilities.modalities.input.is_empty(),
        "a model must accept at least one input modality"
    );
    assert!(
        !capabilities.modalities.output.is_empty(),
        "a model must produce at least one output modality"
    );

    let continuity = model.continuity();
    let wire = serde_json::json!({ "opaque": "state" });
    let token = continuity.build(&wire).expect("build a continuity token");
    assert_eq!(
        token,
        continuity.build(&wire).expect("build a continuity token"),
        "building a token from the same wire state must be deterministic"
    );
    assert_eq!(
        continuity.replay(expected, &[], &token),
        ReplayDecision::Replay,
        "a token built for this model must replay for it"
    );

    let fragment = ContinuityFragment::new(
        token.family.clone(),
        ContinuityScope::Exact(expected.clone()),
        token.schema_version,
        ContinuitySemantics::Append,
        b"tail".to_vec(),
    );
    let merged = continuity
        .merge(Some(token.clone()), fragment.clone())
        .expect("merge a fragment");
    assert_eq!(
        merged,
        continuity
            .merge(Some(token), fragment)
            .expect("merge a fragment"),
        "merging the same fragment must be deterministic"
    );
    assert!(merged.payload().ends_with(b"tail"));
}

/// Runs the full [`Store`] contract against `store`.
///
/// Every backend in the workspace calls this from its own test module, so a
/// durable store cannot silently drift from the reference semantics.
///
/// # Panics
///
/// Panics when the store violates the contract.
pub async fn store_conformance(store: &dyn Store) {
    let session_id = SessionId::new();
    let created = store
        .commit(create_commit(session_id))
        .await
        .expect("create session");
    assert_eq!(created.version, 1, "a new session starts at version 1");

    match store.commit(create_commit(session_id)).await {
        Err(Error::StoreConflict {
            expected: None,
            actual,
            ..
        }) => assert_eq!(actual, 1),
        other => panic!("recreating a session must conflict, got {other:?}"),
    }

    let user = Node::message(Message::user("hello"), None);
    let assistant = Node::message(Message::assistant("hi there"), Some(user.id));
    let session = store
        .commit(Commit {
            session_id,
            expected_version: Some(created.version),
            nodes: vec![user.clone(), assistant.clone()],
            head: Some(assistant.id),
            phase: Phase::Idle,
            turn: 1,
            grants: Vec::new(),
            metadata: None,
        })
        .await
        .expect("append turn");
    assert_eq!(session.version, 2);
    assert_eq!(session.head, Some(assistant.id));

    let loaded = store.load(session_id).await.expect("load session");
    assert_eq!(loaded, session);

    let history = store.history(session.head).await.expect("history");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].text(), "hello");
    assert_eq!(history[1].text(), "hi there");

    let ancestors = store.ancestors(assistant.id).await.expect("ancestors");
    assert_eq!(ancestors, vec![user.id, assistant.id]);

    let children = store.children(user.id).await.expect("children");
    assert_eq!(children, vec![assistant.id]);

    match store
        .commit(Commit {
            session_id,
            expected_version: Some(created.version),
            nodes: Vec::new(),
            head: None,
            phase: Phase::Idle,
            turn: 1,
            grants: Vec::new(),
            metadata: None,
        })
        .await
    {
        Err(Error::StoreConflict {
            expected: Some(1),
            actual: 2,
            ..
        }) => {}
        other => panic!("stale version must conflict, got {other:?}"),
    }

    match store.node(NodeId::new()).await {
        Err(Error::NodeNotFound(_)) => {}
        other => panic!("missing node must be a typed error, got {other:?}"),
    }

    let record = ApprovalRecord::new(
        session_id,
        CallId::new(),
        ToolName::new("read_file").expect("tool name"),
        &serde_json::json!({ "path": "/tmp/a" }),
        Duration::minutes(5),
    );
    store
        .save_approval(record.clone())
        .await
        .expect("save approval");
    let fetched = store
        .approval(record.id)
        .await
        .expect("load approval")
        .expect("approval exists");
    assert_eq!(fetched, record);
    let pending = store.approvals(session_id).await.expect("approvals");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, record.id);
}

fn create_commit(session_id: SessionId) -> Commit {
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
