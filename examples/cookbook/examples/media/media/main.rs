//! Media, end to end, against a real provider: the blob store that keeps the
//! bytes, the adapter that puts them back on the wire, and the tool-result
//! path.
//!
//! Media is the part of the library with the most moving pieces, so this recipe
//! runs one live conversation and then exercises the ports the engine calls
//! around it. The live half needs a key and a vision model:
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... COOKBOOK_MODEL=openai/gpt-6-luna \
//!   cargo run -p norte-cookbook --example media
//! ```
//!
//! # The three places bytes live in this recipe
//!
//! A `MediaPart` points at its bytes through a [`MediaSource`], and the point of
//! the adapter seam is that the *graph* and the *wire* want different ones:
//!
//! | Source | Where the bytes are | On the way out | On the way in |
//! |---|---|---|---|
//! | `Inline(Vec<u8>)` | in the part | base64 on the wire | — |
//! | `Blob(BlobId)` | the blob store | resolved to bytes | written when a part closes |
//! | `Url(String)` | a link the vendor fetches | sent as-is | fetched and stored |
//!
//! A fourth source, `ProviderFile(ref)`, is a file the vendor already holds. It
//! has its own recipe — [`provider_files`](../provider_files.rs) — because it
//! needs a vendor with a file API, which `OpenRouter` does not have.
//!
//! # The two directions, and who triggers them
//!
//! [`MediaAdapter`] is the seam. The engine calls it, never the host:
//!
//! * [`ingest`](MediaAdapter::ingest) runs on **model output** — a `Url` or a
//!   `ProviderFile` the vendor returned is fetched and written to the blob store
//!   before the assistant message is committed, so the graph never depends on a
//!   link that expires (FR-PART-5).
//! * [`resolve`](MediaAdapter::resolve) runs when a **request is built** — a
//!   `Blob` is read back into `Inline` bytes, and an expired `ProviderFile` is
//!   revived from its fallback blob (FR-PART-4, FR-RECON-4). Reading the same
//!   blob always yields the same bytes, which is what keeps a provider's cached
//!   prefix stable.
//!
//! Input media is the host's to store: the host puts the bytes in the blob store
//! and sends a `Blob` reference, exactly as the engine does with output. This
//! recipe does that in step 1.

use std::error::Error;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use norte_core::{
    BlobId, BlobStore, CallId, Capabilities, ContinuityAdapter, MediaAdapter, MediaKind, MediaPart,
    MediaSource, Message, MimeType, Modality, Model, ModelRef, ModelRequest, ModelStream, Part,
    Role, SessionId, StreamingGranularity, ToolCallPart, ToolKind, ToolRegistry, ToolResultPart,
    TurnOptions, WireFamily,
};
use norte_harness::{Harness, HarnessBuilder, HarnessConfig, InMemoryBlobStore, InMemoryStore};
use norte_http::{AuthScheme, HttpClient, HttpMedia, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};
use serde_json::Value;

/// A 64×64 PNG: a red circle on white. Small enough to travel inline, real
/// enough that a vision model describes it.
const CHART_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAA/UlEQVR42u2ayxHDIAxE7Z10kpP7L8Yn10I6wCAJtITlmpnd94w88e8spRwrLxyLLwlIQAISyF2f8MTnuiq/fu87tu6M+iOrc48z8Qr0coebIJfeH2LcgRD0kK0ACb05FiT05nDw0NsqQEVvKAIbfW8dCOm7Sje4mEs5/O3VoKVvBND9ALNA+vy0YGiEaAVI5ucVRiMkAQlIQAIS+E+B8KewnlWB0QgxC5BMUR1DI0QukD5FrwDwRyTSb3MOpGxCYyli4ybT943QNIeuIoyLnkBvOYmHOhjCMadmXKzrTX3Uwy/PEUFWcVTI9h979Jrwfm6jy2kJSEACElhy/QAUBG9jEIUtJQAAAABJRU5ErkJggg==";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let png = base64::engine::general_purpose::STANDARD.decode(CHART_PNG)?;
    let blobs = Arc::new(InMemoryBlobStore::new());
    let media = Arc::new(HttpMedia::new(&HttpClient::new()?));
    let (engine, captured) = new_engine(Arc::clone(&blobs), Arc::clone(&media))?;
    let options = options();

    println!("--- 1. an image the host stored as a blob");
    let stored = blobs.put(&png).await?;
    let session = engine.create().await?;
    let outcome = engine
        .turn(session.id)
        .input(ask(
            "In one short sentence: what colour is the shape in this image?",
            MediaSource::Blob(stored),
        ))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    println!("answer: {}", answer(outcome.message.as_ref()));
    println!(
        "the graph holds: {}",
        graph_media(&engine, session.id).await?.join(", ")
    );
    println!("the wire carried: {}", wire_media(&captured));

    println!("\n--- 2. the same image, sent inline");
    let outcome = engine
        .turn(session.id)
        .input(ask(
            "And in one short sentence: what is the background colour?",
            MediaSource::Inline(png.clone()),
        ))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    println!("answer: {}", answer(outcome.message.as_ref()));
    println!("the wire carried: {}", wire_media(&captured));

    println!("\n--- 3. media inside a tool result");
    println!(
        "{}",
        tool_result_media(&engine, Arc::clone(&blobs), &options).await?
    );

    println!("\n--- 4. ingest: a URL the model returned becomes a blob");
    let url = serve(png.clone())?;
    let part = MediaPart::new(MediaKind::Image, MediaSource::Url(url.clone()), png_mime());
    let ingested = media.ingest(part, &*blobs).await?;
    let MediaSource::Blob(id) = ingested.source else {
        unreachable!("ingest always stores");
    };
    println!(
        "fetched {url} and stored {} bytes as {}",
        blobs.get(id).await?.len(),
        describe(&ingested.source),
    );

    println!("\n--- 5. and the failures are typed, never silent");
    let missing = MediaPart::new(
        MediaKind::Image,
        MediaSource::Blob(BlobId::of(b"never stored")),
        png_mime(),
    );
    match media.resolve(missing, &*blobs).await {
        Err(error) => println!("a blob that is gone: {error}"),
        Ok(_) => unreachable!("the blob was never stored"),
    }

    Ok(())
}

// ── the live flow ───────────────────────────────────────────────────────────

/// The engine, over `OpenRouter`, with the media adapter installed. The blob
/// store and the adapter are held outside the engine too, so the port steps
/// below use the same instances the engine runs with.
fn new_engine(
    blobs: Arc<InMemoryBlobStore>,
    media: Arc<HttpMedia>,
) -> Result<(Harness, Captured), Box<dyn Error>> {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let model = Probe {
        inner: openrouter(vision_capabilities())?,
        captured: Arc::clone(&captured),
    };
    let engine = HarnessBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(blobs)
        .model(Arc::new(model))
        .tools(ToolRegistry::new())
        .media_adapter(media)
        .config(HarnessConfig {
            // A normal host leaves this at 64 KiB; a comment here and there is
            // fine to keep the transcript readable.
            inline_threshold: 64 * 1024,
            ..HarnessConfig::default()
        })
        .build()?;
    Ok((engine, captured))
}

/// Regression, live: a tool returns an image, and the `Chat` wire — whose `tool`
/// message is text-only — refuses it with a typed error instead of sending the
/// result with the media dropped. Responses, Anthropic and Gemini carry the same
/// part; on those wires the model would see the image (FR-TOOL-3).
async fn tool_result_media(
    engine: &Harness,
    blobs: Arc<InMemoryBlobStore>,
    options: &TurnOptions,
) -> Result<String, Box<dyn Error>> {
    let stored = blobs.put(b"the chart the tool rendered").await?;
    let call_id = CallId::new();
    let call = Message::new(
        Role::Assistant,
        vec![Part::ToolCall(ToolCallPart {
            call_id,
            name: "render_chart".to_owned(),
            arguments: serde_json::json!({ "kind": "chart" }),
            continuity: None,
        })],
    );
    let result = Message::new(
        Role::Tool,
        vec![Part::ToolResult(ToolResultPart {
            call_id,
            parts: vec![Part::Media(MediaPart::new(
                MediaKind::Image,
                MediaSource::Blob(stored),
                png_mime(),
            ))],
            structured: None,
            is_error: false,
        })],
    );

    let session = engine.create().await?;
    engine
        .append_history(session.id, vec![call, result])
        .await?;
    match engine
        .turn(session.id)
        .input(Message::user("Describe what the tool returned."))
        .options(options.clone())
        .start()
        .finish()
        .await
    {
        Err(error) => Ok(format!("the engine refused it, spelled out: {error}")),
        Ok(outcome) => Ok(format!(
            "unexpectedly carried by this wire: {}",
            answer(outcome.message.as_ref())
        )),
    }
}

/// The wire bodies the model was asked for, in call order.
type Captured = Arc<Mutex<Vec<Value>>>;

/// A model that records the exact body of every request and then makes the call
/// anyway, so what is shown is what was sent.
struct Probe {
    inner: OpenAiChat,
    captured: Captured,
}

#[async_trait::async_trait]
impl Model for Probe {
    fn reference(&self) -> &ModelRef {
        self.inner.reference()
    }

    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }

    fn continuity(&self) -> &dyn ContinuityAdapter {
        self.inner.continuity()
    }

    async fn stream(&self, request: ModelRequest) -> norte_core::Result<ModelStream> {
        // `request_body` is the projection the live path uses, so recording it
        // records the bytes, not an approximation of them.
        let body = self.inner.request_body(&request)?;
        self.captured
            .lock()
            .expect("the probe lock is never held across an await")
            .push(body);
        self.inner.stream(request).await
    }
}

fn vision_capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .input(Modality::Image)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .build()
}

/// A one-line answer per turn keeps the run cheap.
fn options() -> TurnOptions {
    let mut options = TurnOptions::default();
    options.params.max_output_tokens = Some(256);
    options
}

/// Wires `OpenRouter`. An `OpenAI`-compatible vendor is a profile, not a preset.
fn openrouter(capabilities: Capabilities) -> Result<OpenAiChat, Box<dyn Error>> {
    Ok(OpenAiChat::new(
        ModelRef::new(
            "openrouter".try_into()?,
            required("COOKBOOK_MODEL", "openai/gpt-6-luna")?
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

/// A value the run has to be given.
fn required(name: &str, example: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("set {name}, for example {name}={example}").into())
}

/// A one-file HTTP server, so the URL-ingest step runs offline. It stands in for
/// the vendor's CDN: `HttpMedia::ingest` does a real `GET` against it.
fn serve(bytes: Vec<u8>) -> Result<String, Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                bytes.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&bytes);
            let _ = stream.flush();
        }
    });
    Ok(format!("http://{address}/chart.png"))
}

// ── reading the run back ────────────────────────────────────────────────────

fn ask(text: &str, source: MediaSource) -> Message {
    let mut message = Message::user(text);
    message.parts.push(Part::Media(MediaPart::new(
        MediaKind::Image,
        source,
        png_mime(),
    )));
    message
}

fn answer(message: Option<&Message>) -> String {
    message.map_or_else(|| "(no message)".to_owned(), Message::text)
}

/// Every media part in the session's history, in the form the graph stores it —
/// descending into tool results, which is the whole of the FR-TOOL-3 fix.
async fn graph_media(engine: &Harness, session: SessionId) -> Result<Vec<String>, Box<dyn Error>> {
    let mut out = Vec::new();
    for message in engine.history(session).await? {
        for part in &message.parts {
            match part {
                Part::Media(media) => out.push(describe(&media.source)),
                Part::ToolResult(result) => {
                    for inner in &result.parts {
                        if let Part::Media(media) = inner {
                            out.push(format!("in a tool result, {}", describe(&media.source)));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(out)
}

/// The media in the last request the model was asked for, in the form the wire
/// carries it.
fn wire_media(captured: &Captured) -> String {
    let requests = captured
        .lock()
        .expect("the probe lock is never held across an await");
    let Some(last) = requests.last() else {
        return "(no request yet)".to_owned();
    };
    let mut urls = Vec::new();
    collect_image_urls(last, &mut urls);
    if urls.is_empty() {
        return "(no media on this request)".to_owned();
    }
    urls.iter()
        .map(|url| describe_url(url))
        .collect::<Vec<_>>()
        .join(", ")
}

fn collect_image_urls(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(url) = map
                .get("image_url")
                .and_then(|image| image.get("url"))
                .and_then(Value::as_str)
            {
                out.push(url.to_owned());
            } else {
                for nested in map.values() {
                    collect_image_urls(nested, out);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_image_urls(item, out);
            }
        }
        _ => {}
    }
}

/// A short, readable view of a source: a blob id's debug form is 64 hex chars
/// and buries the point of each step.
fn describe(source: &MediaSource) -> String {
    match source {
        MediaSource::Inline(bytes) => format!("inline, {} bytes", bytes.len()),
        MediaSource::Blob(id) => format!("blob {}", &id.to_string()[..12]),
        MediaSource::Url(url) => format!("url {url}"),
        MediaSource::ProviderFile(reference) => format!(
            "provider file {}/{} ({})",
            reference.provider,
            reference.file_id,
            match reference.expires_at {
                Some(when) => format!("expires {when}"),
                None => "no expiry".to_owned(),
            }
        ),
        _ => format!("{source:?}"),
    }
}

fn describe_url(url: &str) -> String {
    match url.strip_prefix("data:") {
        Some(rest) => {
            let (meta, data) = rest.split_once(',').unwrap_or((rest, ""));
            format!("a data URL ({meta}, {} inline chars)", data.len())
        }
        None => format!("the URL itself ({url})"),
    }
}

fn png_mime() -> MimeType {
    MimeType::new("image/png").expect("mime")
}
