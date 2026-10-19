//! Live provider smoke tests (FR-TEST-2).
//!
//! These are `#[ignore]`d so the default `cargo test` run stays hermetic. The
//! scheduled CI job runs them with `--ignored` and provider keys in the
//! environment. Each test returns early when its key is absent, so the suite is
//! also safe to run locally without secrets.

use std::sync::Arc;

use norte_core::{EngineBuilder, Message, Model};
use norte_testing::{InMemoryBlobStore, InMemoryStore, text_capabilities};

fn live_key(variable: &str) -> Option<String> {
    std::env::var(variable)
        .ok()
        .filter(|value| !value.is_empty())
}

/// Runs one real turn through the full engine and returns the assistant text.
async fn reply(model: Arc<dyn Model>) -> String {
    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(model)
        .build()
        .expect("engine");
    let session = engine.create().await.expect("create");
    let outcome = engine
        .send(
            session.id,
            Message::user("Reply with exactly the single word: pong"),
        )
        .finish()
        .await
        .expect("turn");
    outcome.message.expect("assistant message").text()
}

#[tokio::test]
#[ignore = "live provider smoke test; run with --ignored and a key set"]
async fn live_openai_responses() {
    let Some(key) = live_key("OPENAI_API_KEY") else {
        return;
    };
    let model = norte_openai::presets::openai("gpt-5", key, text_capabilities()).expect("model");
    let text = reply(Arc::new(model)).await;
    assert!(!text.is_empty(), "the live model returned no text");
}

#[tokio::test]
#[ignore = "live provider smoke test; run with --ignored and a key set"]
async fn live_anthropic_messages() {
    let Some(key) = live_key("ANTHROPIC_API_KEY") else {
        return;
    };
    let model = norte_anthropic::presets::anthropic("claude-sonnet-4", key, text_capabilities())
        .expect("model");
    let text = reply(Arc::new(model)).await;
    assert!(!text.is_empty(), "the live model returned no text");
}

#[tokio::test]
#[ignore = "live provider smoke test; run with --ignored and a key set"]
async fn live_gemini_generate() {
    let Some(key) = live_key("GEMINI_API_KEY") else {
        return;
    };
    let model =
        norte_gemini::presets::gemini("gemini-2.5-pro", key, text_capabilities()).expect("model");
    let text = reply(Arc::new(model)).await;
    assert!(!text.is_empty(), "the live model returned no text");
}

#[tokio::test]
#[ignore = "live provider smoke test; run with --ignored and a key set"]
async fn live_openrouter_chat() {
    let Some(key) = live_key("OPENROUTER_API_KEY") else {
        return;
    };
    let provider = norte_core::ProviderId::new("openrouter").expect("provider");
    let reference = norte_core::ModelRef::new(
        provider.clone(),
        "nvidia/nemotron-3.5-super-vl:free".parse().expect("model"),
    );
    let profile = norte_http::VendorProfile::new(
        provider,
        norte_core::WireFamily::new(norte_openai::WIRE_CHAT).expect("wire"),
        "https://openrouter.ai/api/v1",
    )
    .expect("profile")
    .with_auth(norte_http::AuthScheme::bearer(key))
    .with_header("X-Title", "Norte");
    let model =
        norte_openai::OpenAiChat::new(reference, profile, text_capabilities()).expect("chat");
    let text = reply(Arc::new(model)).await;
    assert!(!text.is_empty(), "the live model returned no text");
}
