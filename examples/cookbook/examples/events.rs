//! Reading a turn's event stream.
//!
//! A turn is a typed stream: the engine reports what it has already committed,
//! in order, and only then moves on. A UI renders from these events.
//!
//! The answer arrives as text deltas and is printed as it lands, once: the
//! committed message holds that same text, so the recipe never prints it a
//! second time. Deltas that are not text — a reasoning model sends hundreds of
//! them — are counted rather than dumped, or the console would be unreadable.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p norte-cookbook --example events
//! ```

use std::error::Error;
use std::sync::Arc;

use futures_util::StreamExt;
use norte_core::{Capabilities, EngineEvent, InMemoryBlobStore, InMemoryStore, Message, Modality, ModelId, ModelRef, ProviderId, StreamDelta, StreamingGranularity, WireFamily};
use norte_harness::{EngineBuilder};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .build();
    let model = openrouter(capabilities)?;
    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .build()?;

    let session = engine.create().await?;
    let mut stream = engine.send(
        session.id,
        Message::user("Name three uses for a paperclip."),
    );

    let mut counted = 0_usize;
    let mut streaming = false;
    while let Some(event) = stream.next().await {
        match &event? {
            EngineEvent::StreamDelta {
                delta: StreamDelta::Text { text, .. },
                ..
            } => {
                print!("{text}");
                streaming = true;
            }
            EngineEvent::StreamDelta { .. } => counted += 1,
            other => {
                // A marker always starts on a line of its own, even when it
                // lands in the middle of a streamed answer.
                if streaming {
                    println!();
                    streaming = false;
                }
                report(other);
            }
        }
    }
    let outcome = stream.finish().await?;
    if streaming {
        println!();
    }

    // The text above came from the deltas, and the committed message carries the
    // same text: only its shape is reported here, never its content again.
    println!(
        "{counted} non-text delta(s); committed {} part(s); phase {:?}",
        outcome
            .message
            .as_ref()
            .map_or(0, |message| message.parts.len()),
        outcome.session.phase
    );
    Ok(())
}

/// Reports the events that are not deltas: what the engine committed, and when.
///
/// `EngineEvent` is `#[non_exhaustive]`, so the wildcard arm is part of the
/// contract: a new variant must not break a reader. `PartCompleted` is reported
/// without its part, for the same reason the answer is printed only once — the
/// part carries the text the deltas already showed.
fn report(event: &EngineEvent) {
    match event {
        EngineEvent::SessionCreated { phase, .. } => println!("[session created in {phase:?}]"),
        EngineEvent::PhaseChanged { from, to, .. } => println!("[phase {from:?} -> {to:?}]"),
        EngineEvent::PartCompleted { .. } => println!("\n[one part finished]"),
        EngineEvent::CommitSucceeded { version, .. } => {
            println!("[committed as version {version}]");
        }
        EngineEvent::RequestReconstructed {
            digest,
            tools: exposed,
            ..
        } => println!("[request {digest} exposing {} tool(s)]", exposed.len()),
        EngineEvent::Error { message, .. } => println!("[error: {message}]"),
        other => println!("[{other:?}]"),
    }
}

/// A value the run has to be given: nothing external is defaulted in the
/// source, because a model id or an endpoint written here is a pointer that
/// rots, and a run that quietly picks one is a run nobody chose.
fn required(name: &str, example: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("set {name}, for example {name}={example}").into())
}

/// Wires `OpenRouter`. An `OpenAI`-compatible vendor is a profile, not a preset.
fn openrouter(capabilities: Capabilities) -> Result<OpenAiChat, Box<dyn Error>> {
    let provider = ProviderId::new("openrouter")?;
    let endpoint = required("COOKBOOK_ENDPOINT", "https://openrouter.ai/api/v1")?;
    let profile = VendorProfile::new(provider.clone(), WireFamily::new(WIRE_CHAT)?, &endpoint)?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?));
    let id = required("COOKBOOK_MODEL", "deepseek/deepseek-v4.1-flash-1017")?;
    Ok(OpenAiChat::new(
        ModelRef::new(provider, ModelId::new(&id)?),
        profile,
        capabilities,
    )?)
}
