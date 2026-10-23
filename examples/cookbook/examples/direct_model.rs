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
    let provider = ProviderId::new("openrouter")?;
    let profile = VendorProfile::new(
        provider.clone(),
        WireFamily::new(WIRE_CHAT)?,
        &std::env::var("COOKBOOK_ENDPOINT")?,
    )?
    .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?));
    let model = OpenAiChat::new(
        ModelRef::new(provider, ModelId::new(&std::env::var("COOKBOOK_MODEL")?)?),
        profile,
        Capabilities::builder()
            .input(Modality::Text)
            .output(Modality::Text)
            .streaming(StreamingGranularity::Delta)
            .build(),
    )?;

    let mut request = ModelRequest::new(
        model.reference().clone(),
        vec![
            Message::system("Answer in one sentence."),
            Message::user("What is a prompt cache?"),
        ],
    );
    // Bound the answer; without a cap the vendor charges the full window.
    request.params.max_output_tokens = Some(1024);

    println!(
        "{}",
        serde_json::to_string_pretty(&model.request_body(&request)?)?
    );
    let mut stream = model.stream(request).await?;
    while let Some(event) = stream.next().await {
        println!("{:?}", event?);
    }
    Ok(())
}
