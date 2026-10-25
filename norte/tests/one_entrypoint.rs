//! The facade as an application uses it: every path, and the attribute macro,
//! starts at `norte::`.

#![cfg(all(feature = "openai", feature = "harness"))]

use std::sync::Arc;

use norte::harness::{HarnessBuilder, InMemoryStore};
use norte::{
    Capabilities, FnExecutor, InMemoryBlobStore, Modality, Result, ToolOutput, ToolRegistry,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct EchoArgs {
    /// The text to repeat.
    text: String,
}

#[norte::tool(name = "echo", description = "Repeat the text back.")]
struct Echo;

#[async_trait::async_trait]
impl FnExecutor<EchoArgs, ToolOutput> for Echo {
    async fn call(&self, args: EchoArgs) -> Result<ToolOutput> {
        Ok(ToolOutput::text(args.text))
    }
}

#[test]
fn one_dependency_is_enough() {
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .build();

    // The adapter through the facade: the preset, the type, the wire.
    let model = norte::presets::openai("gpt-5", "sk-test", capabilities).expect("a model");
    let _: &dyn norte::Model = &model;
    let _ = std::any::type_name::<norte::OpenAiChat>();
    assert_eq!(norte::openai::WIRE_CHAT, "openai.chat");

    let mut tools = ToolRegistry::new();
    tools.register_tool(&Echo).expect("the tool registers");
    assert_eq!(tools.len(), 1);

    let engine = HarnessBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(tools)
        .build()
        .expect("an engine");
    assert_eq!(engine.tools().len(), 1);
}
