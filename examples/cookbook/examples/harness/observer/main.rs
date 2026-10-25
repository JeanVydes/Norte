//! An observer: watching what the loop reports, without changing it.
//!
//! An observer is a side channel. It is handed every `EngineEvent` the harness
//! emits — a turn started, a request reconstructed, a part committed, a tool
//! call parked — and it may not block the turn to do anything about it. The set
//! gives each observer a bounded channel and drops what does not fit, so a slow
//! observer costs you events, never a stalled turn.
//!
//! A `StreamTransform` is the same idea one level down: it projects the deltas
//! the host reads. It cannot drop or reorder what the harness commits, so it is
//! a display concern, not a durability one.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! COOKBOOK_ENDPOINT=https://openrouter.ai/api/v1 \
//! COOKBOOK_MODEL=deepseek/deepseek-v4.1-flash-1017 \
//!   cargo run -p norte-cookbook --example observer
//! ```

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::StreamExt;
use norte_core::{Capabilities, Message, Modality, ModelRef, StreamingGranularity, WireFamily};
use norte_harness::{
    EngineEvent, HarnessBuilder, InMemoryBlobStore, InMemoryStore, Observer, StreamDelta,
    StreamTransform,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};

/// Counts what the loop reported, by the kind of thing it was.
#[derive(Default)]
struct Counter {
    seen: Mutex<BTreeMap<&'static str, u64>>,
}

impl Observer for Counter {
    fn name(&self) -> &'static str {
        "counter"
    }

    fn observe(&self, event: &EngineEvent) {
        // `observe` runs on its own task and may not await: anything slow here
        // becomes a dropped event, not a slow turn.
        let kind = match event {
            EngineEvent::SessionCreated { .. } => "session created",
            EngineEvent::PhaseChanged { .. } => "phase changed",
            EngineEvent::RequestReconstructed { .. } => "request reconstructed",
            EngineEvent::PartCompleted { .. } => "part committed",
            EngineEvent::ToolCallStarted { .. } => "tool started",
            EngineEvent::ToolCallFinished { .. } => "tool finished",
            EngineEvent::CommitSucceeded { .. } => "commit",
            EngineEvent::StreamDelta { .. } => "delta",
            _ => "other",
        };
        *self
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(kind)
            .or_default() += 1;
    }
}

/// Upper-cases the text a host reads, and counts what it touched.
///
/// A transform sees the deltas that go *out* to the caller. What the harness
/// commits is built from the raw events, so this cannot change the history.
struct Shout {
    touched: AtomicU64,
}

impl StreamTransform for Shout {
    fn name(&self) -> &'static str {
        "shout"
    }

    fn transform(&self, delta: StreamDelta) -> Vec<StreamDelta> {
        match delta {
            StreamDelta::Text { index, text } => {
                self.touched.fetch_add(1, Ordering::Relaxed);
                vec![StreamDelta::Text {
                    index,
                    text: text.to_uppercase(),
                }]
            }
            other => vec![other],
        }
    }
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let counter = Arc::new(Counter::default());
    let shout = Arc::new(Shout {
        touched: AtomicU64::new(0),
    });

    let harness = HarnessBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(openrouter(
            Capabilities::builder()
                .input(Modality::Text)
                .output(Modality::Text)
                .streaming(StreamingGranularity::Delta)
                .build(),
        )?))
        .observers(vec![Arc::clone(&counter) as Arc<dyn Observer>])
        .transforms(vec![Arc::clone(&shout) as Arc<dyn StreamTransform>])
        .build()?;

    let session = harness.create().await?;
    let mut stream = harness.send(
        session.id,
        Message::user("Say hello in exactly three words."),
    );

    // The deltas here have been through `Shout`: they read upper case. The
    // committed message below does not, because a transform shapes the view and
    // not the history.
    while let Some(event) = stream.next().await {
        let event = event?;
        if let norte_harness::EngineEvent::StreamDelta {
            delta: StreamDelta::Text { text, .. },
            ..
        } = &event
        {
            print!("{text}");
        }
    }
    println!();

    let session = harness.session(session.id).await?;
    for message in harness.history(session.id).await? {
        if message.role == norte_core::Role::Assistant {
            println!("committed: {}", message.text());
        }
    }

    println!("\nwhat the loop reported:");
    for (kind, count) in counter
        .seen
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
    {
        println!("  {kind:<24} {count}");
    }
    println!(
        "\nthe transform touched {} delta(s); phase is {:?}",
        shout.touched.load(Ordering::Relaxed),
        session.phase
    );

    Ok(())
}

/// A value the run has to be given.
fn required(name: &str, example: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("set {name}, for example {name}={example}").into())
}
