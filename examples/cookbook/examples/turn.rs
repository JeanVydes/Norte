//! The engine's turn loop, reduced to its bones.
//!
//! A host that writes its own loop still gets the hard parts from the library:
//! `Model` streams neutral events and `StreamAccumulator` turns them into
//! `Part`s. What the host owns is the history — a `Vec<Message>` it grows and
//! sends again. This recipe runs three turns and prints each answer as it lands;
//! `direct_tools` adds the calls a model can ask for.
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
    Capabilities, Message, Modality, Model, ModelRef, ModelRequest, ProviderId, Role,
    StreamAccumulator, StreamEvent, StreamLimits, StreamingGranularity, WireFamily,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let provider: ProviderId = "openrouter".try_into()?;
    let profile = VendorProfile::new(
        provider.clone(),
        WireFamily::new(WIRE_CHAT)?,
        &std::env::var("COOKBOOK_ENDPOINT")?,
    )?
    .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?));
    let model = OpenAiChat::new(
        ModelRef::new(
            provider,
            std::env::var("COOKBOOK_MODEL")?.as_str().try_into()?,
        ),
        profile,
        Capabilities::builder()
            .input(Modality::Text)
            .output(Modality::Text)
            .streaming(StreamingGranularity::Delta)
            .build(),
    )?;

    let mut history = vec![Message::system("Answer in one sentence.")];

    for question in [
        "What is a prompt cache?",
        "And what is a cache miss?",
        "And what breaks a prefix?",
    ] {
        history.push(Message::user(question));

        let mut request = ModelRequest::new(model.reference().clone(), history.clone());
        // Bound the answer; without a cap the vendor charges the full window.
        request.params.max_output_tokens = Some(1024);

        let mut stream = model.stream(request).await?;
        let mut accumulator = StreamAccumulator::new(StreamLimits::default());
        while let Some(event) = stream.next().await {
            let event = event?;
            if let StreamEvent::TextDelta { text, .. } = &event {
                print!("{text}");
            }
            accumulator.apply(event)?;
        }
        println!();

        let answer = Message::new(Role::Assistant, accumulator.finish()?.parts);
        history.push(answer);
    }
    Ok(())
}
