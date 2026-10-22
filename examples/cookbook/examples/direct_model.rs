//! One request to a model, with no engine at all.
//!
//! `Model` is a port, and the engine is only one of its clients. This recipe
//! calls an adapter directly: it builds a `ModelRequest`, prints the body the
//! adapter puts on the wire, and then prints every `StreamEvent` as it arrives.
//! Nothing is assembled, committed or sent back — what the adapter emits is the
//! whole output.
//!
//! That is the seam a host sits on when it wants its own loop. See
//! `direct_turn` for the next step: the same stream, accumulated into a message
//! and carried across turns; `direct_tools` adds the calls the model asks for.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! COOKBOOK_ENDPOINT=https://openrouter.ai/api/v1 \
//! COOKBOOK_MODEL=deepseek/deepseek-v4.1-flash-1017 \
//!   cargo run -p norte-cookbook --example direct_model
//! ```

use std::error::Error;

use futures_util::StreamExt;
use norte_core::{
    Capabilities, Message, Modality, Model, ModelId, ModelRef, ModelRequest, ProviderId,
    StreamingGranularity, WireFamily,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let model = openrouter()?;

    let mut request = ModelRequest::new(
        model.reference().clone(),
        vec![
            Message::system("Answer in one sentence."),
            Message::user("What is a prompt cache?"),
        ],
    );
    // A cap keeps the recipe cheap and the answer bounded; without one the
    // vendor charges against the model's full output window.
    request.params.max_output_tokens = Some(1024);

    // The exact JSON the adapter will send. This is how a host checks what a
    // turn looks like on the wire without an engine.
    println!("--- request body");
    println!(
        "{}",
        serde_json::to_string_pretty(&model.request_body(&request)?)?
    );

    // Nothing is processed: every event is printed in the shape the adapter
    // produced it. Text arrives as deltas, tool arguments as fragments, and
    // `Usage` plus `Finish` close the stream.
    println!("\n--- events");
    let mut stream = model.stream(request).await?;
    while let Some(event) = stream.next().await {
        println!("{:?}", event?);
    }
    Ok(())
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
