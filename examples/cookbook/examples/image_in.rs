//! A picture in: bytes on the wire, with no engine and no session.
//!
//! Media travels as a `Part::Media` inside a message. `Model` takes the whole
//! request, so sending an image is building the right part and letting the
//! adapter translate it: an `OpenAI`-compatible wire inlines the bytes as a data
//! `URL`, the native vendors upload them and send a reference.
//!
//! The declaration advertises the modality first. A model that says it takes
//! text only is refused with a typed error rather than a silent drop.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! COOKBOOK_ENDPOINT=https://openrouter.ai/api/v1 \
//! COOKBOOK_MODEL=openai/gpt-6-luna \
//!   cargo run -p norte-cookbook --example image-in
//! ```

use std::error::Error;

use base64::Engine as _;
use futures_util::StreamExt;
use norte_core::{
    Capabilities, MediaKind, MediaPart, MediaSource, Message, MimeType, Modality, Model, ModelRef,
    ModelRequest, Part, Role, StreamEvent, StreamingGranularity, TextPart, Usage, WireFamily,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};

/// A 1×1 PNG: the smallest thing that is still a real image.
const ONE_PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(ONE_PIXEL_PNG)?;

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
            // Both directions: this recipe sends an image and asks about it.
            .input(Modality::Text)
            .input(Modality::Image)
            .output(Modality::Text)
            .streaming(StreamingGranularity::Delta)
            .build(),
    )?;

    // `Inline` keeps the bytes in the request. `Blob` would name a
    // content-addressed store instead, `Url` a location the provider fetches,
    // and `ProviderFile` one it already holds.
    let picture = Part::Media(MediaPart::new(
        MediaKind::Image,
        MediaSource::Inline(bytes),
        MimeType::new("image/png")?,
    ));

    let mut stream = model
        .stream(ModelRequest::new(
            model.reference().clone(),
            vec![
                Message::system("Answer in one short sentence."),
                Message::new(
                    Role::User,
                    vec![
                        picture,
                        Part::Text(TextPart::new("What colour is this pixel?")),
                    ],
                ),
            ],
        ))
        .await?;

    // The provider counts the tokens; the recipe only reads what it reports.
    let mut usage = Usage::default();
    while let Some(event) = stream.next().await {
        match event? {
            // `StreamEvent` is what the wire produced. There is no session, no
            // commit and no `EngineEvent` here: those belong to a runtime.
            StreamEvent::TextDelta { text, .. } => print!("{text}"),
            StreamEvent::Usage(reported) => usage = reported,
            _ => {}
        }
    }
    // A provider reports what it counts, and a field it omits stays `None`
    // rather than becoming a zero nobody measured.
    println!(
        "\n\ninput {} · output {}",
        usage.input_tokens.unwrap_or(0),
        usage.output_tokens.unwrap_or(0)
    );

    Ok(())
}
