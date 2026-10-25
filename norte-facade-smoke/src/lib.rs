//! The `norte` facade, with no other dependency.
//!
//! Every path and every macro in this crate starts at `norte::`, so the workspace
//! builds a package that uses the facade exactly as an application would.

#![forbid(unsafe_code)]

use norte::{FnExecutor, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The argument type of [`Echo`].
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct EchoArgs {
    /// The text to repeat.
    pub text: String,
}

/// A tool declared with the attribute the facade re-exports.
#[norte::tool(name = "echo", description = "Repeat the text back.")]
pub struct Echo;

#[async_trait::async_trait]
impl FnExecutor<EchoArgs, ToolOutput> for Echo {
    async fn call(&self, args: EchoArgs) -> norte::Result<ToolOutput> {
        Ok(ToolOutput::text(args.text))
    }
}

/// The same attribute with an explicit anchor, the form `crate = <path>` takes.
#[norte::tool(name = "echo_explicit", description = "Repeat the text back.", crate = ::norte)]
pub struct EchoExplicit;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use norte::prelude::*;

    /// The README quickstart, without the calls that need a network.
    fn quickstart_engine() -> Result<Harness, Box<dyn std::error::Error>> {
        let capabilities = Capabilities::builder()
            .input(Modality::Text)
            .output(Modality::Text)
            .build();
        let model = norte::presets::openai("gpt-5", "sk-test", capabilities)?;

        let engine = HarnessBuilder::new()
            .store(Arc::new(InMemoryStore::new()))
            .blobs(Arc::new(InMemoryBlobStore::new()))
            .model(Arc::new(model))
            .tools(ToolRegistry::new())
            .build()?;
        Ok(engine)
    }

    #[test]
    fn the_quickstart_compiles_against_one_dependency() {
        let _engine = quickstart_engine().expect("an engine");
        let _ = Message::user("Hello!");
    }

    #[test]
    fn the_facade_registers_a_tool_and_builds_a_model() {
        let capabilities = Capabilities::builder()
            .input(Modality::Text)
            .output(Modality::Text)
            .build();
        let model = norte::presets::openai("gpt-5", "sk-test", capabilities).expect("a model");
        let _: &dyn Model = &model;

        let mut tools = ToolRegistry::new();
        tools.register_tool(&Echo).expect("the tool registers");
        assert_eq!(tools.len(), 1);

        // `tools` is an opt-in feature: naming the suite checks it is wired.
        let _ = std::any::type_name::<ShellTool>();
    }
}
