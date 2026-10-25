//! Layer tests: what a layer may change and what the digest describes.

use std::sync::{Arc, Mutex, PoisonError};

use norte_core::{Continuation, Digest, EngineEvent, InMemoryBlobStore, InMemoryStore, Layer, LayerContext, Message, ModelRequest, ModelStream, Observer, Result};
use norte_harness::{EngineBuilder};
use norte_testing::{ScriptedModel, model};

/// A layer that changes the request before it reaches the model.
struct Rewriting {
    /// The request the layer received, before it changed anything.
    seen: Arc<Mutex<Option<ModelRequest>>>,
}

#[async_trait::async_trait]
impl Layer for Rewriting {
    fn name(&self) -> &'static str {
        "rewriting"
    }

    async fn wrap(
        &self,
        context: &LayerContext<'_>,
        mut request: ModelRequest,
        next: &dyn Continuation,
    ) -> Result<ModelStream> {
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(request.clone());
        request.params.temperature = Some(0.1);
        next.call(context, request).await
    }
}

/// An observer that records the digests the engine reported.
struct Digests(Arc<Mutex<Vec<Digest>>>);

impl Observer for Digests {
    fn name(&self) -> &'static str {
        "digests"
    }

    fn observe(&self, event: &EngineEvent) {
        if let EngineEvent::RequestReconstructed { digest, .. } = event {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(*digest);
        }
    }
}

fn digest_of(request: &ModelRequest) -> Digest {
    Digest::of_json(&serde_json::to_value(request).unwrap_or(serde_json::Value::Null))
}

#[tokio::test]
async fn the_digest_describes_the_request_the_model_received() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("hello"));
    let store = Arc::new(InMemoryStore::new());
    let seen: Arc<Mutex<Option<ModelRequest>>> = Arc::new(Mutex::new(None));
    let digests: Arc<Mutex<Vec<Digest>>> = Arc::new(Mutex::new(Vec::new()));
    let engine = EngineBuilder::new()
        .store(store.clone())
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(scripted.clone())
        .layers(norte_core::LayerStack::new(vec![Arc::new(Rewriting {
            seen: Arc::clone(&seen),
        })]))
        .observers(vec![
            Arc::new(Digests(Arc::clone(&digests))) as Arc<dyn Observer>
        ])
        .build()
        .expect("build engine");

    let session = engine.create().await.expect("create");
    engine
        .send(session.id, Message::user("ping"))
        .finish()
        .await
        .expect("turn");

    let reported = digests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(reported.len(), 1, "the engine reported one request");

    let requests = scripted.requests();
    assert_eq!(requests.len(), 1);
    let wire = &requests[0];
    assert_eq!(
        wire.params.temperature,
        Some(0.1),
        "the layer changed what the model received"
    );

    assert_eq!(
        reported[0],
        digest_of(wire),
        "the digest describes the request the model received"
    );

    let reconstructed = seen
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .expect("the layer saw the request");
    assert_ne!(
        reported[0],
        digest_of(&reconstructed),
        "and not the request as it was reconstructed"
    );
}
