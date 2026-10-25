//! One request to a model, with no engine at all.
//!
//! `Model` is a port, and the engine is only one of its clients. This recipe
//! builds the provider and the model, prints the body the adapter sends, and
//! then prints every `StreamEvent` as it arrives. Nothing is assembled,
//! committed or sent back.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! COOKBOOK_ENDPOINT=https://openrouter.ai/api/v1 \
//! COOKBOOK_MODEL=deepseek/deepseek-v4.1-flash-1017 \
//!   cargo run -p norte-cookbook --example model
//! ```

use std::error::Error;

use futures_util::StreamExt;
use norte_core::{
    Capabilities, Message, Modality, Model, ModelRef, ModelRequest, StreamingGranularity,
    WireFamily,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let model = OpenAiChat::new(
        ModelRef::new(
            "openrouter".try_into()?,
            std::env::var("COOKBOOK_MODEL")?.as_str().try_into()?,
        ),
        VendorProfile::new(
            "openrouter".try_into()?,
            WireFamily::new(WIRE_CHAT)?,
            &std::env::var("COOKBOOK_ENDPOINT")?,
        )?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?)),
        Capabilities::builder()
            .input(Modality::Text)
            .output(Modality::Text)
            .streaming(StreamingGranularity::Delta)
            .build(),
    )?;

    let mut stream = model
        .stream(ModelRequest::new(
            model.reference().clone(),
            vec![
                Message::system("Answer in one sentence."),
                Message::user("What is a prompt cache?"),
            ],
        ))
        .await?;

    while let Some(event) = stream.next().await {
        println!("{:?}", event?);
    }

    Ok(())
}
