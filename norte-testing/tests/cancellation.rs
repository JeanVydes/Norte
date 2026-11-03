//! Cancellation tests: a host token stops a turn before the provider answers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use norte_core::{
    Capabilities, ContinuityAdapter, Message, MessageStatus, Model, ModelRef, ModelRequest,
    ModelStream,
};
use norte_harness::{HarnessBuilder, HarnessConfig, Phase, Store};
use norte_testing::{InMemoryBlobStore, InMemoryStore, ScriptedModel, model};
use tokio_util::sync::CancellationToken;

/// A model whose `stream()` never resolves, so the turn is stuck before the
/// provider answers. The probe records that the request future was dropped.
struct StallingModel {
    inner: Arc<ScriptedModel>,
    started: Arc<tokio::sync::Notify>,
    dropped: Arc<AtomicBool>,
}

struct DropProbe(Arc<AtomicBool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl Model for StallingModel {
    fn reference(&self) -> &ModelRef {
        self.inner.reference()
    }

    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }

    fn continuity(&self) -> &dyn ContinuityAdapter {
        self.inner.continuity()
    }

    async fn stream(&self, _request: ModelRequest) -> norte_core::Result<ModelStream> {
        let _probe = DropProbe(Arc::clone(&self.dropped));
        self.started.notify_one();
        std::future::pending::<()>().await;
        unreachable!("a pending future never resolves")
    }
}

#[tokio::test]
async fn cancelling_before_the_first_token_returns_at_once() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")));
    let started = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let stalling = Arc::new(StallingModel {
        inner: Arc::clone(&scripted),
        started: Arc::clone(&started),
        dropped: Arc::clone(&dropped),
    });
    let store = Arc::new(InMemoryStore::new());
    let engine = HarnessBuilder::new()
        .store(store.clone())
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(stalling)
        .config(HarnessConfig::default())
        .build()
        .expect("build engine");

    let session = engine.create().await.expect("create");
    let token = CancellationToken::new();
    let mut turn = engine
        .turn(session.id)
        .input(Message::user("go"))
        .cancellation(token.clone())
        .start_stepped();

    let canceller = tokio::spawn(async move {
        started.notified().await;
        token.cancel();
    });
    let outcome = tokio::time::timeout(Duration::from_secs(1), turn.finish())
        .await
        .expect("a cancel before the first token must not wait for the provider")
        .expect("turn");
    canceller.await.expect("canceller");

    assert_eq!(outcome.session.phase, Phase::Idle);
    let head = outcome.session.head.expect("head");
    let message = store.node(head).await.expect("node").message;
    assert_eq!(message.role, norte_core::Role::Assistant);
    assert_eq!(
        message.origin.as_ref().map(|origin| origin.status),
        Some(MessageStatus::Interrupted)
    );
    assert!(message.parts.is_empty(), "nothing was streamed");
    assert!(
        dropped.load(Ordering::SeqCst),
        "the provider request future was dropped"
    );
}
