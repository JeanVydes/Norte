//! What the host does with a rate limit.
//!
//! The transport never retries a provider's answer (FR-ERR-4): a `429` reaches
//! the host as a typed error, with `retry_after` intact and `is_transient` to
//! say it may clear. This recipe is the host half of that contract — classify,
//! wait, and resume the same session, which stays `Streaming` (FR-ERR-5).
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! COOKBOOK_ENDPOINT=https://openrouter.ai/api/v1 \
//! COOKBOOK_MODEL=deepseek/deepseek-v4-flash-0731 \
//!   cargo run -p norte-cookbook --example rate_limit
//! ```
//!
//! The first part classifies a `429` and a `401` without touching the network,
//! because a vendor cannot be asked to rate-limit on demand. The second runs a
//! real turn through the same policy: on a transient failure it waits as the
//! provider asked and resumes; on anything else it stops at once.

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use norte_core::{
    Capabilities, Engine, EngineBuilder, Error as NorteError, InMemoryBlobStore, InMemoryStore,
    Message, Modality, ModelId, ModelRef, ProviderError, ProviderErrorKind, ProviderId, SessionId,
    StreamingGranularity, ToolRegistry, TurnOutcome, WireFamily,
};
use norte_http::{AuthScheme, VendorProfile, is_transient};
use norte_openai::{OpenAiChat, WIRE_CHAT};

/// How many times the host is willing to retry one turn.
const MAX_ATTEMPTS: u32 = 3;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    println!("── 1. how a rate limit arrives ──────────────────────");

    // This is the shape the wire produces: kind, status and `Retry-After`
    // parsed into a `Duration`. Nothing here is vendor-specific.
    let rate_limited: NorteError = ProviderError::new(ProviderErrorKind::RateLimited, "slow down")
        .with_status(429)
        .with_retry_after(Duration::from_secs(2))
        .into();
    println!("kind          = {:?}", rate_limited.provider_kind());
    println!("retry_after   = {:?}", rate_limited.retry_after());
    println!("is_transient  = {}", is_transient(&rate_limited));

    // A bad key is a different decision: waiting cannot fix it.
    let unauthorized: NorteError = ProviderError::new(ProviderErrorKind::Authentication, "bad key")
        .with_status(401)
        .into();
    println!("401 transient = {}", is_transient(&unauthorized));

    println!("\n── 2. a real turn, wrapped in the host policy ───────");

    let engine = new_engine()?;
    let session = engine.create().await?;
    let outcome = run_with_retry(
        &engine,
        session.id,
        Message::user("Reply with the single word: ok."),
    )
    .await?;

    println!(
        "answer        = {}",
        outcome
            .message
            .as_ref()
            .map_or_else(|| "(none)".to_owned(), Message::text)
    );

    Ok(())
}

/// Runs one turn; if it fails in a way that may clear on its own, waits as the
/// provider asked and resumes the same session.
///
/// A failed turn leaves the session `Streaming`, so `resume` picks up from the
/// committed head — the user message is not re-sent and the tools already run
/// are not run again (FR-ERR-5). The loop is bounded: a provider that keeps
/// answering `429` eventually surfaces the error to the caller.
async fn run_with_retry(
    engine: &Engine,
    session: SessionId,
    message: Message,
) -> Result<TurnOutcome, Box<dyn Error>> {
    let mut stream = engine.send(session, message);
    let mut attempt = 1;

    loop {
        match stream.finish().await {
            Ok(outcome) => return Ok(outcome),
            Err(error) if is_transient(&error) && attempt < MAX_ATTEMPTS => {
                let wait = error.retry_after().unwrap_or_else(|| backoff(attempt));
                println!("attempt {attempt}: transient failure, waiting {wait:?} then resuming");
                tokio::time::sleep(wait).await;
                attempt += 1;
                stream = engine.resume(session);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// The host's own choice when the provider did not say how long to wait:
/// exponential, capped, so a burst cannot turn into a hot loop.
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(250 * 2_u64.pow(attempt.min(4))).min(Duration::from_secs(5))
}

/// The engine, wired to `OpenRouter` through the `OpenAI` Chat adapter.
fn new_engine() -> Result<Engine, Box<dyn Error>> {
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .build();

    let provider = ProviderId::new("openrouter")?;
    let endpoint = required("COOKBOOK_ENDPOINT", "https://openrouter.ai/api/v1")?;
    let profile =
        VendorProfile::new(provider.clone(), WireFamily::new(WIRE_CHAT)?, &endpoint)?.with_auth(
            AuthScheme::bearer(required("OPENROUTER_API_KEY", "sk-or-...")?),
        );
    let id = required("COOKBOOK_MODEL", "deepseek/deepseek-v4-flash-0731")?;
    let model = OpenAiChat::new(
        ModelRef::new(provider, ModelId::new(&id)?),
        profile,
        capabilities,
    )?;

    Ok(EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(ToolRegistry::new())
        .build()?)
}

/// A value the run has to be given.
fn required(name: &str, example: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("set {name}, for example {name}={example}").into())
}
