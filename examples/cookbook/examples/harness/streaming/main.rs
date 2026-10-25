//! Streaming is the only mode; collecting is draining the same stream.
//!
//! There is no blocking call to switch to. A host that only wants the final
//! message still gets a stream — it hands it to `finish()`, which drains it and
//! returns the committed outcome. Deltas and the final message are the same
//! data, read at different times.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p norte-cookbook --example streaming
//! ```

use std::error::Error;
use std::sync::Arc;

use futures_util::StreamExt;
use norte_core::{Capabilities, Message, Modality, ModelRef, StreamingGranularity, WireFamily};
use norte_harness::{EngineEvent, StreamDelta};
use norte_harness::{Harness, HarnessBuilder, InMemoryBlobStore, InMemoryStore};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};

const PROMPT: &str = "Count from one to five, one number per line.";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let harness = engine()?;

    println!("--- live: the host reads every delta as it lands");
    live(&harness).await?;

    println!("\n--- collected: the host ignores the stream entirely");
    collected(&harness).await?;
    Ok(())
}

/// Reads the stream event by event and prints the text as it arrives.
async fn live(harness: &Harness) -> Result<(), Box<dyn Error>> {
    let session = harness.create().await?;
    let mut stream = harness.send(session.id, Message::user(PROMPT));
    while let Some(event) = stream.next().await {
        if let EngineEvent::StreamDelta {
            delta: StreamDelta::Text { text, .. },
            ..
        } = event?
        {
            print!("{text}");
        }
    }
    let outcome = stream.finish().await?;
    println!(
        "\n(committed {} chars)",
        text(outcome.message.as_ref()).len()
    );
    Ok(())
}

/// The idiomatic "I do not care about partial output": `finish()` drains the
/// same stream and returns the committed outcome. Nothing is skipped.
async fn collected(harness: &Harness) -> Result<(), Box<dyn Error>> {
    let session = harness.create().await?;
    let outcome = harness
        .send(session.id, Message::user(PROMPT))
        .finish()
        .await?;
    println!("{}", text(outcome.message.as_ref()));
    Ok(())
}

fn text(message: Option<&Message>) -> String {
    message.map(Message::text).unwrap_or_default()
}

/// The wiring, once: a real model, its declared capabilities, and the store.
fn engine() -> Result<Harness, Box<dyn Error>> {
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .build();
    let model = openrouter(capabilities)?;
    Ok(HarnessBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .build()?)
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
