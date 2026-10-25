//! Observers and stream transforms: the side-channel.
//!
//! Observers are notified on their own task, through their own bounded channel:
//! they never block a turn, and under back-pressure their events are dropped.
//! Transforms shape the deltas the host reads; they cannot change what is
//! committed, because the committed message is built from the raw model events.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p norte-cookbook --example observers
//! ```

use std::error::Error;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::StreamExt;
use norte_core::{
    Capabilities, EngineEvent, Message, Modality, ModelRef, Observer, StreamDelta, StreamTransform,
    StreamingGranularity, WireFamily,
};
use norte_harness::{HarnessBuilder, InMemoryBlobStore, InMemoryStore};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};

/// Records the kind of every event it is handed. Observers run on a task of their
/// own, so the state travels behind a mutex.
struct Recorder {
    seen: Arc<Mutex<Vec<&'static str>>>,
}

impl Observer for Recorder {
    fn name(&self) -> &'static str {
        "recorder"
    }

    fn observe(&self, event: &EngineEvent) {
        guard(&self.seen).push(kind(event));
    }
}

/// Uppercases text deltas on their way to the host.
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

/// `EngineEvent` is `#[non_exhaustive]`, so the wildcard is part of the contract.
fn kind(event: &EngineEvent) -> &'static str {
    match event {
        EngineEvent::SessionCreated { .. } => "SessionCreated",
        EngineEvent::PhaseChanged { .. } => "PhaseChanged",
        EngineEvent::StreamDelta { .. } => "StreamDelta",
        EngineEvent::PartCompleted { .. } => "PartCompleted",
        EngineEvent::CommitSucceeded { .. } => "CommitSucceeded",
        _ => "other",
    }
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .build();
    let model = openrouter(capabilities)?;
    let harness = HarnessBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .observers(vec![Arc::new(Recorder {
            seen: Arc::clone(&seen),
        })])
        .transform(Arc::new(Shout))
        .build()?;

    let session = harness.create().await?;
    let mut stream = harness.send(
        session.id,
        Message::user("Say hello in exactly five words."),
    );
    while let Some(event) = stream.next().await {
        if let EngineEvent::StreamDelta {
            delta: StreamDelta::Text { text, .. },
            ..
        } = event?
        {
            // Uppercase: the transform shaped this delta on its way here.
            print!("{text}");
        }
    }
    let outcome = stream.finish().await?;

    // The committed message came from the raw events, so it is untouched.
    println!(
        "\ncommitted: {}",
        outcome
            .message
            .as_ref()
            .map(Message::text)
            .unwrap_or_default()
    );

    // Observers consume on their own task: give them a moment before reading.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let seen = guard(&seen);
    println!(
        "observed {} events, first {:?}",
        seen.len(),
        &seen[..seen.len().min(6)]
    );
    Ok(())
}

/// A value the run has to be given: nothing external is defaulted in the
/// source, because a model id or an endpoint written here is a pointer that
/// rots, and a run that quietly picks one is a run nobody chose.
fn required(name: &str, example: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("set {name}, for example {name}={example}").into())
}

/// Wires `OpenRouter`. An `OpenAI`-compatible vendor is a profile, not a preset.
fn openrouter(capabilities: Capabilities) -> Result<OpenAiChat, Box<dyn Error>> {
    Ok(OpenAiChat::new(
        ModelRef::new(
            "openrouter".try_into()?,
            required("COOKBOOK_MODEL", "deepseek/deepseek-v4.1-flash-1017")?
                .as_str()
                .try_into()?,
        ),
        VendorProfile::new(
            "openrouter".try_into()?,
            WireFamily::new(WIRE_CHAT)?,
            &required("COOKBOOK_ENDPOINT", "https://openrouter.ai/api/v1")?,
        )?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?)),
        capabilities,
    )?)
}
