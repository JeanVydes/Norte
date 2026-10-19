//! Compile-checks the wiring the guide prints, so `guide/` cannot drift from the
//! public API (NFR-2). Every item below is a snippet from a page; the test only
//! has to build and run, and it never touches a network.

#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use norte_core::{
    Capabilities, Continuation, Decision, Engine, EngineBuilder, FnExecutor, InMemoryBlobStore,
    InMemoryStore, Layer, LayerContext, Message, Modality, ModelId, ModelRef, ModelRequest,
    ModelStream, Policy, PolicySet, PolicyView, ProviderId, Result, ToolOutput, ToolRegistry,
    WireFamily,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT, presets::openai};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `getting-started.md`: the three native presets and the engine wiring.
fn getting_started_engine() -> Result<Engine> {
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .build();
    let model = openai("gpt-5", "sk-test", capabilities)?;

    EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(ToolRegistry::new())
        .build()
}

/// `getting-started.md`: any `OpenAI`-compatible endpoint is a profile.
fn compatible_endpoint() -> Result<Engine> {
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .build();
    let provider = ProviderId::new("openrouter")?;
    let profile = VendorProfile::new(
        provider.clone(),
        WireFamily::new(WIRE_CHAT)?,
        "https://openrouter.ai/api/v1",
    )?
    .with_auth(AuthScheme::bearer("sk-test"));
    let model = OpenAiChat::new(
        ModelRef::new(provider, ModelId::new("deepseek/deepseek-v4-flash-0731")?),
        profile,
        capabilities,
    )?;

    EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(ToolRegistry::new())
        .build()
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct WordCountArgs {
    /// The text to count. Words are separated by whitespace.
    text: String,
}

#[norte_core::tool(
    name = "word_count",
    description = "Count the words in text, for example {\"text\": \"the sky is blue\"}."
)]
struct WordCount;

#[async_trait]
impl FnExecutor<WordCountArgs, ToolOutput> for WordCount {
    async fn call(&self, args: WordCountArgs) -> Result<ToolOutput> {
        Ok(ToolOutput::json(serde_json::json!({
            "words": args.text.split_whitespace().count(),
        })))
    }
}

/// `guides/add-a-tool.md`: the macro plus the blanket declarator.
fn register_a_tool() -> Result<ToolRegistry> {
    let mut tools = ToolRegistry::new();
    tools.register_tool(&WordCount)?;
    Ok(tools)
}

struct WordCountIsHarmless;

impl Policy for WordCountIsHarmless {
    fn name(&self) -> &'static str {
        "word-count-is-harmless"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        if view.call.name.as_str() == "word_count" && view.call.arguments["text"].as_str().is_none()
        {
            return Decision::Deny(
                "`text` must be a string, for example {\"text\": \"the sky is blue\"}".to_owned(),
            );
        }
        Decision::Allow
    }
}

struct PinRoute;

#[async_trait]
impl Layer for PinRoute {
    fn name(&self) -> &'static str {
        "pin-route"
    }

    async fn wrap(
        &self,
        context: &LayerContext<'_>,
        request: ModelRequest,
        next: &dyn Continuation,
    ) -> Result<ModelStream> {
        let mut request = request;
        request.extensions.insert(
            "openai.chat:provider".to_owned(),
            serde_json::json!({ "order": ["DeepInfra"], "allow_fallbacks": false }),
        );
        next.call(context, request).await
    }
}

#[test]
fn the_guide_wiring_compiles() {
    let _ = getting_started_engine().expect("the quickstart wiring");
    let _ = compatible_endpoint().expect("the profile wiring");
    let _ = register_a_tool().expect("the tool registration");
    let _ = PolicySet::new(vec![Arc::new(WordCountIsHarmless)]);
    let _ = PinRoute;
    let _ = Message::system("You are a coding agent.");
}
