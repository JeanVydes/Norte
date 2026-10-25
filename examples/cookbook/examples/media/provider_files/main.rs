//! The vendor-file lifecycle, alone: upload bytes to a provider, keep the
//! reference, read it back, revive it when it expires, and send it in a turn.
//!
//! A `MediaSource::ProviderFile` is the fourth way to point at media. Instead of
//! carrying the bytes (`Inline`), a blob id (`Blob`) or a link (`Url`), it names
//! a file the provider already holds. The engine can only do two things with
//! such a file, and both go through the [`ProviderFiles`] port:
//!
//! * `download` — read the bytes, so an output reference can be copied into the
//!   blob store before the vendor drops it (FR-PART-5).
//! * `upload` — store bytes and return a fresh reference, so an expired one is
//!   revived from its fallback blob (FR-PART-4).
//!
//! The port ships with the three adapters that have a file API:
//! [`OpenAiFiles`], `norte_anthropic::AnthropicFiles` and
//! `norte_gemini::GeminiFiles`. This recipe uses the `OpenAI` one over the
//! Responses wire, whose `input_image` takes a `file_id` directly:
//!
//! ```sh
//! OPENAI_API_KEY=sk-... COOKBOOK_MODEL=gpt-6-luna \
//!   cargo run -p norte-cookbook --example provider_files
//! ```
//!
//! # What the host owns
//!
//! The port is attached to the media adapter, and the host keeps its own handle
//! to call `upload`/`download` itself. The engine never persists a reference:
//! it re-derives it from the blob on every request, so a revoked id is replaced
//! before the turn goes out and the cached prefix stays stable (FR-RECON-3).
//!
//! [`ProviderFiles`]: norte_core::ProviderFiles

use std::error::Error;
use std::sync::Arc;

use base64::Engine as _;
use chrono::DateTime;
use norte_core::{
    BlobStore, Capabilities, MediaAdapter, MediaKind, MediaPart, MediaSource, Message, MimeType,
    Modality, Part, ProviderFileRef, ProviderFiles, StreamingGranularity, ToolRegistry,
    TurnOptions,
};
use norte_harness::{HarnessBuilder, InMemoryBlobStore, InMemoryStore};
use norte_http::{HttpClient, HttpMedia};
use norte_openai::presets::openai;
use norte_openai::{OpenAiFiles, presets::openai_profile};

/// A 64×64 PNG: a red circle on white.
const CHART_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAA/UlEQVR42u2ayxHDIAxE7Z10kpP7L8Yn10I6wCAJtITlmpnd94w88e8spRwrLxyLLwlIQAISyF2f8MTnuiq/fu87tu6M+iOrc48z8Qr0coebIJfeH2LcgRD0kK0ACb05FiT05nDw0NsqQEVvKAIbfW8dCOm7Sje4mEs5/O3VoKVvBND9ALNA+vy0YGiEaAVI5ucVRiMkAQlIQAIS+E+B8KewnlWB0QgxC5BMUR1DI0QukD5FrwDwRyTSb3MOpGxCYyli4ybT943QNIeuIoyLnkBvOYmHOhjCMadmXKzrTX3Uwy/PEUFWcVTI9h979Jrwfm6jy2kJSEACElhy/QAUBG9jEIUtJQAAAABJRU5ErkJggg==";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let api_key = required("OPENAI_API_KEY", "sk-...")?;
    let model = required("COOKBOOK_MODEL", "gpt-6-luna")?;
    let png = base64::engine::general_purpose::STANDARD.decode(CHART_PNG)?;
    let mime = MimeType::new("image/png")?;

    println!("--- 1. upload the bytes to the vendor");
    let profile = openai_profile(api_key.as_str())?;
    let vendor = Arc::new(OpenAiFiles::new(profile)?);
    let reference = vendor.upload(MediaKind::Image, &mime, &png).await?;
    println!(
        "the provider minted {} ({} bytes fall back to blob {})",
        reference.file_id,
        png.len(),
        reference
            .blob
            .map_or_else(|| "none".to_owned(), |blob| blob.to_string()),
    );
    println!(
        "expires: {}",
        reference.expires_at.map_or_else(
            || "never, as far as the host knows".to_owned(),
            |at| at.to_string()
        )
    );

    println!("\n--- 2. read the file back");
    let downloaded = vendor.download(&reference).await?;
    println!("downloaded {} bytes", downloaded.len());

    println!("\n--- 3. revive an expired reference from its blob");
    let blobs = Arc::new(InMemoryBlobStore::new());
    let blob = blobs.put(&png).await?;
    let expired = ProviderFileRef {
        // A timestamp before now: what the engine sees once the vendor's id
        // lapses.
        expires_at: Some(DateTime::from_timestamp(0, 0).expect("a time before now")),
        blob: Some(blob),
        ..reference.clone()
    };
    let media = Arc::new(HttpMedia::new(&HttpClient::new()?).with_provider_files(vendor.clone()));
    let part = MediaPart::new(
        MediaKind::Image,
        MediaSource::ProviderFile(expired.clone()),
        mime.clone(),
    );
    let resolved = media.resolve(part, &*blobs).await?;
    let MediaSource::ProviderFile(fresh) = resolved.source else {
        unreachable!("a provider file stays a provider file");
    };
    println!("the engine re-uploaded it as {}", fresh.file_id);

    println!("\n--- 4. when the vendor has no file API");
    // `OpenRouter` and the like have no Files API, so a host wires no port. A
    // stale reference then fails closed with a typed error naming the part,
    // rather than a request that silently drops the media (FR-PART-4). Calling
    // `upload` against such a vendor would surface the vendor's own error — a
    // `404` mapped to `NotFound` — which the adapter tests cover.
    let bare = HttpMedia::new(&HttpClient::new()?);
    let stale = MediaPart::new(
        MediaKind::Image,
        MediaSource::ProviderFile(expired),
        mime.clone(),
    );
    match bare.resolve(stale, &*blobs).await {
        Err(error) => println!("without a port the engine reports: {error}"),
        Ok(_) => unreachable!("no port means no revive"),
    }

    println!("\n--- 5. send the file in a conversation");
    let harness = HarnessBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(blobs)
        .model(Arc::new(openai(&model, api_key.as_str(), capabilities())?))
        .tools(ToolRegistry::new())
        .media_adapter(media)
        .build()?;
    let session = harness.create().await?;
    let mut message = Message::user("Describe the image in one sentence.");
    message.parts.push(Part::Media(MediaPart::new(
        MediaKind::Image,
        MediaSource::ProviderFile(fresh),
        mime,
    )));
    let mut options = TurnOptions::default();
    options.params.max_output_tokens = Some(128);
    let outcome = harness
        .turn(session.id)
        .input(message)
        .options(options)
        .start()
        .finish()
        .await?;
    println!(
        "answer: {}",
        outcome
            .message
            .map_or_else(|| "(no message)".to_owned(), |message| message.text())
    );

    Ok(())
}

fn capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .input(Modality::Image)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .build()
}

/// A value the run has to be given.
fn required(name: &str, example: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("set {name}, for example {name}={example}").into())
}
