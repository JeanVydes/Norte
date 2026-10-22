//! The engine's turn loop, reduced to its bones.
//!
//! A host that writes its own loop still gets the hard parts from the library:
//! `Model` streams neutral events and `StreamAccumulator` turns them into
//! `Part`s. What the host owns is the transcript — a `Vec<Message>` it builds,
//! appends to and sends again. This recipe runs two turns and prints each answer
//! as it lands.
//!
//! Tools are the other half of the loop: fill `request.tools`, and when the
//! answer carries a `Part::ToolCall`, append one `Message::tool(results)` and
//! ask again. What the engine adds around that step is policies, approvals and
//! the registry; what it adds around the loop is persistence, reconstruction
//! (continuity replay and cache boundaries) and resuming a parked turn.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! COOKBOOK_ENDPOINT=https://openrouter.ai/api/v1 \
//! COOKBOOK_MODEL=deepseek/deepseek-v4.1-flash-1017 \
//!   cargo run -p norte-cookbook --example direct_turn
//! ```

use std::error::Error;

use futures_util::StreamExt;
use norte_core::{
    Capabilities, Message, Modality, Model, ModelId, ModelRef, ModelRequest, ProviderId, Role,
    StreamAccumulator, StreamEvent, StreamLimits, StreamingGranularity, WireFamily,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let model = openrouter()?;
    let mut transcript = vec![
        Message::system("Answer in one sentence."),
        Message::user("What is a prompt cache?"),
    ];

    for question in ["And what is a cache miss?", "And what breaks a prefix?"] {
        transcript.push(Message::user(question));
        let answer = answer(&model, &transcript).await?;
        transcript.push(answer);
        println!();
    }

    println!("--- transcript: {} messages", transcript.len());
    for message in &transcript {
        println!("{:?}: {}", message.role, message.text());
    }
    Ok(())
}

/// One request, drained into the single message it produces.
async fn answer(model: &dyn Model, transcript: &[Message]) -> Result<Message, Box<dyn Error>> {
    let mut request = ModelRequest::new(model.reference().clone(), transcript.to_vec());
    // A cap keeps the recipe cheap and the answer bounded; without one the
    // vendor charges against the model's full output window.
    request.params.max_output_tokens = Some(1024);
    let mut stream = model.stream(request).await?;
    let mut accumulator = StreamAccumulator::new(StreamLimits::default());
    while let Some(event) = stream.next().await {
        let event = event?;
        // A UI renders deltas as they land; the message returned below holds
        // the same text, so this prints it once.
        if let StreamEvent::TextDelta { text, .. } = &event {
            print!("{text}");
        }
        accumulator.apply(event)?;
    }
    let accumulated = accumulator.finish()?;
    println!(
        "\n(finish: {:?}, {} in / {} out)",
        accumulated.finish_reason,
        accumulated.usage.input_tokens.unwrap_or(0),
        accumulated.usage.output_tokens.unwrap_or(0),
    );
    Ok(Message::new(Role::Assistant, accumulated.parts))
}

/// A value the run has to be given: nothing external is defaulted in the
/// source, because a model id or an endpoint written here is a pointer that
/// rots, and a run that quietly picks one is a run nobody chose.
fn required(name: &str, example: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("set {name}, for example {name}={example}").into())
}

/// Wires `OpenRouter`. An `OpenAI`-compatible vendor is a profile, not a preset.
fn openrouter() -> Result<OpenAiChat, Box<dyn Error>> {
    let provider = ProviderId::new("openrouter")?;
    let endpoint = required("COOKBOOK_ENDPOINT", "https://openrouter.ai/api/v1")?;
    let profile = VendorProfile::new(provider.clone(), WireFamily::new(WIRE_CHAT)?, &endpoint)?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?));
    let id = required("COOKBOOK_MODEL", "deepseek/deepseek-v4.1-flash-1017")?;
    Ok(OpenAiChat::new(
        ModelRef::new(provider, ModelId::new(&id)?),
        profile,
        capabilities(),
    )?)
}

fn capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .build()
}
