//! Compile-checks the wiring shown in `README.md`, so the documentation cannot
//! drift from the public API (NFR-2).

use std::sync::Arc;

use rustychain_core::{
    Capabilities, Conversation, Engine, EngineBuilder, FnExecutor, Message, Modality,
    ReasoningEffort, ReasoningRequest, Result, ToolName, ToolRegistry,
};
use rustychain_openai::presets::openai;
use rustychain_testing::{InMemoryBlobStore, InMemoryStore};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EchoArgs {
    text: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct EchoResult {
    echoed: String,
}

#[rustychain_core::tool(name = "echo", description = "Echo the provided text")]
struct Echo;

#[async_trait::async_trait]
impl FnExecutor<EchoArgs, EchoResult> for Echo {
    async fn call(&self, args: EchoArgs) -> Result<EchoResult> {
        Ok(EchoResult { echoed: args.text })
    }
}

fn capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .build()
}

fn readme_engine() -> Engine {
    let model = openai("gpt-5", "sk-test", capabilities()).expect("model");
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");
    EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(tools)
        .build()
        .expect("build engine")
}

#[test]
fn the_readme_engine_wiring_compiles() {
    let _ = readme_engine();
}

#[tokio::test]
async fn the_readme_turn_control_wiring_compiles() {
    let engine = readme_engine();
    let session = engine.create().await.expect("create");
    let _turn = engine
        .turn(session.id)
        .input(Message::user("Summarise this."))
        .temperature(0.2)
        .top_k(40)
        .reasoning(ReasoningRequest::Effort(ReasoningEffort::High))
        .tools([ToolName::new("echo").expect("name")]);
}

#[tokio::test]
async fn the_readme_history_wiring_compiles() {
    let engine = readme_engine();
    let session = engine.create().await.expect("create");
    let conversation = engine.export(session.id).await.expect("export");
    let json = conversation.to_json().expect("json");
    let decoded = Conversation::from_json(&json).expect("decode");

    let second = readme_engine();
    let _imported = second.import(decoded).await.expect("import");
}

#[test]
fn the_readme_vendor_wiring_compiles() {
    use rustychain_core::{ModelRef, ProviderId, WireFamily};
    use rustychain_http::{AuthScheme, VendorProfile};
    use rustychain_openai::OpenAiChat;

    let profile = VendorProfile::new(
        ProviderId::new("openrouter").expect("provider"),
        WireFamily::new("openai.chat").expect("wire"),
        "https://openrouter.ai/api/v1",
    )
    .expect("profile")
    .with_auth(AuthScheme::bearer("sk-test"));
    let reference = ModelRef::new(
        profile.provider.clone(),
        "anthropic/claude-sonnet-4".parse().expect("model"),
    );
    let chat = OpenAiChat::new(reference, profile, capabilities()).expect("chat");
    let _ = chat;
}
