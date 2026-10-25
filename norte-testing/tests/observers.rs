//! The observer and stream-transform wiring, end to end (FR-OBS-1..3).

use std::sync::Arc;
use std::time::Duration;

use norte_core::{EngineEvent, Message, StreamDelta, StreamTransform, ToolRegistry};
use norte_harness::InMemoryStore;
use norte_harness::{Harness, HarnessBuilder, InMemoryBlobStore};
use norte_testing::{RecordingObserver, ScriptedModel, model};

/// Upper-cases text deltas, the canonical "annotate for the UI" transform.
struct Shout;

impl StreamTransform for Shout {
    fn name(&self) -> &'static str {
        "shout"
    }

    fn transform(&self, delta: StreamDelta) -> Vec<StreamDelta> {
        match delta {
            StreamDelta::Text { index, text } => vec![StreamDelta::Text {
                index,
                text: text.to_uppercase(),
            }],
            other => vec![other],
        }
    }
}

fn engine(scripted: Arc<ScriptedModel>, recorder: Arc<RecordingObserver>) -> Harness {
    HarnessBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(scripted)
        .tools(ToolRegistry::new())
        .observers(vec![recorder])
        .transform(Arc::new(Shout))
        .build()
        .expect("build engine")
}

async fn wait_for(recorder: &RecordingObserver, predicate: impl Fn(&EngineEvent) -> bool) -> bool {
    for _ in 0..500 {
        if recorder.events().iter().any(&predicate) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    false
}

#[tokio::test]
async fn a_transform_projects_the_deltas_without_touching_the_commit() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("hello"));
    let recorder = Arc::new(RecordingObserver::new("audit"));
    let engine = engine(scripted, recorder.clone());

    let session = engine.create().await.expect("create");
    assert_eq!(
        session.metadata.get("transforms"),
        Some(&serde_json::json!(["shout"])),
        "the transform name is recorded on the session (FR-OBS-3)"
    );

    let outcome = engine
        .send(session.id, Message::user("ping"))
        .finish()
        .await
        .expect("turn");
    assert_eq!(
        outcome.message.as_ref().map(Message::text).as_deref(),
        Some("hello"),
        "a transform never reaches the accumulator the engine commits"
    );

    assert!(
        wait_for(&recorder, |event| matches!(
            event,
            EngineEvent::StreamDelta {
                delta: StreamDelta::Text { text, .. },
                ..
            } if text == "HELLO"
        ))
        .await,
        "the observer sees the projected delta"
    );
}
