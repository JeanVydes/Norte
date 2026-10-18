//! Streaming is the only mode; collecting is draining the same stream.
//!
//! There is no blocking call to switch to. A host that only wants the final
//! message still gets a stream — it hands it to `finish()`, which drains it and
//! returns the committed outcome. Deltas and the final message are the same
//! data, read at different times.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p rustychain-cookbook --example streaming
//! ```

use std::error::Error;
use std::sync::Arc;

use futures_util::StreamExt;
use rustychain_core::{
    Capabilities, Engine, EngineBuilder, EngineEvent, InMemoryBlobStore, InMemoryStore, Message,
    Modality, ModelId, ModelRef, ProviderId, StreamDelta, StreamingGranularity, WireFamily,
};
use rustychain_http::{AuthScheme, VendorProfile};
use rustychain_openai::{OpenAiChat, WIRE_CHAT};

const PROMPT: &str = "Count from one to five, one number per line.";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let engine = engine()?;

    println!("--- live: the host reads every delta as it lands");
    live(&engine).await?;

    println!("\n--- collected: the host ignores the stream entirely");
    collected(&engine).await?;
    Ok(())
}

/// Reads the stream event by event and prints the text as it arrives.
async fn live(engine: &Engine) -> Result<(), Box<dyn Error>> {
    let session = engine.create().await?;
    let mut stream = engine.send(session.id, Message::user(PROMPT));
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
async fn collected(engine: &Engine) -> Result<(), Box<dyn Error>> {
    let session = engine.create().await?;
    let outcome = engine
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
fn engine() -> Result<Engine, Box<dyn Error>> {
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .build();
    let model = openrouter(capabilities)?;
    Ok(EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .build()?)
}

/// The model every recipe runs on unless `COOKBOOK_MODEL` says otherwise.
const DEFAULT_MODEL: &str = "nvidia/nemotron-3.5-super-vl:free";
/// `OpenRouter`, spoken over the `OpenAI`-compatible Chat Completions wire.
const ENDPOINT: &str = "https://openrouter.ai/api/v1";

/// Wires `OpenRouter`. An `OpenAI`-compatible vendor is a profile, not a preset.
fn openrouter(capabilities: Capabilities) -> Result<OpenAiChat, Box<dyn Error>> {
    let provider = ProviderId::new("openrouter")?;
    let profile = VendorProfile::new(provider.clone(), WireFamily::new(WIRE_CHAT)?, ENDPOINT)?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?));
    let id = std::env::var("COOKBOOK_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    Ok(OpenAiChat::new(
        ModelRef::new(provider, ModelId::new(&id)?),
        profile,
        capabilities,
    )?)
}
