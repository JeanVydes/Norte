//! A picture out: what a model produces, and where those bytes go.
//!
//! A generation that is not text arrives as `Part::Media` on the folded result —
//! the stream carries it as `MediaStart`/`MediaChunk` first, and the accumulator
//! assembles those into one part. No engine is involved: this is the `Model`
//! port and the accumulator it feeds.
//!
//! The bytes come back as `MediaSource::Inline` unless a blob store was given,
//! in which case the adapter writes them there and the part names a `BlobId`.
//! Either way the recipe decides what lands on disk.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! COOKBOOK_ENDPOINT=https://openrouter.ai/api/v1 \
//! COOKBOOK_MODEL=google/gemini-2.5-flash-image \
//!   cargo run -p norte-cookbook --example image-out
//! ```

use std::error::Error;
use std::fs;

use futures_util::StreamExt;
use norte_core::{
    Capabilities, MediaSource, Message, Modality, Model, ModelRef, ModelRequest, Part,
    StreamAccumulator, StreamEvent, StreamLimits, StreamingGranularity, WireFamily,
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
            // The declaration has to say the model produces images, or the
            // adapter refuses the request instead of dropping the part.
            .output(Modality::Text)
            .output(Modality::Image)
            .streaming(StreamingGranularity::Delta)
            .build(),
    )?;

    let mut stream = model
        .stream(ModelRequest::new(
            model.reference().clone(),
            vec![Message::user(
                "Draw a 64 by 64 image: a red circle on a white background.",
            )],
        ))
        .await?;

    // Folds the deltas into parts, the way a runtime does. Media arrives in
    // chunks, so it is assembled rather than printed as it lands.
    let mut accumulator = StreamAccumulator::new(StreamLimits::default());
    while let Some(event) = stream.next().await {
        let event: StreamEvent = event?;
        if let StreamEvent::TextDelta { text, .. } = &event {
            print!("{text}");
        }
        // `apply` returns the part an event completed, if it completed one.
        accumulator.apply(event)?;
    }
    let folded = accumulator.finish()?;

    let mut saved = 0;
    for part in &folded.parts {
        let Part::Media(media) = part else { continue };
        match &media.source {
            MediaSource::Inline(bytes) => {
                // `MimeType` has no extension helper: the shape of a name is the
                // host's business, so the recipe picks its own suffix.
                let suffix = if media.mime.to_string().ends_with("png") {
                    "png"
                } else {
                    "img"
                };
                let path = format!("out-{saved}.{suffix}");
                fs::write(&path, bytes)?;
                println!("wrote {path} ({} bytes, {})", bytes.len(), media.mime);
                saved += 1;
            }
            // With a blob store wired in, the part names an id instead and the
            // bytes are read back from it. Where media is stored is the host's
            // decision, not the recipe's.
            other => println!("media held elsewhere: {other:?}"),
        }
    }

    if saved == 0 {
        println!("the model produced no image this time");
    }
    println!(
        "stopped: {:?} · {} in · {} out",
        folded.finish_reason,
        folded.usage.input_tokens.unwrap_or(0),
        folded.usage.output_tokens.unwrap_or(0)
    );

    Ok(())
}
