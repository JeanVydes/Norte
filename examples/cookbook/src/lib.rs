//! Recipes for specific `Norte` tasks. Each one is a runnable example and
//! holds everything it needs, including its own model wiring:
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p norte-cookbook --example approval
//! ```
//!
//! There is no shared code here on purpose: the only thing a recipe reuses is
//! the library. `tools` needs no key; the rest talk to a real model.

/// The toy weather tool shared by the `tool_loop` recipe.
pub mod weather_tool {
    use std::sync::Arc;

    use norte_core::{FnExecutor, Result, ToolDeclaration, ToolContext, ToolOutput};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    /// The weather tool: fake data, real shape.
    pub struct Weather;

    #[derive(Debug, Deserialize, Serialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct WeatherArgs {
        /// The city to report the weather for.
        #[schemars(example = &"Lisbon")]
        pub city: String,
    }

    #[async_trait::async_trait]
    impl FnExecutor<WeatherArgs, ToolOutput> for Weather {
        async fn call(&self, args: WeatherArgs) -> Result<ToolOutput> {
            Ok(ToolOutput::json(serde_json::json!({
                "city": args.city,
                "celsius": 22,
                "condition": "clear",
            })))
        }
    }

    impl Weather {
        /// The declaration the registry executes.
        pub fn declaration() -> ToolDeclaration<WeatherArgs> {
            ToolDeclaration::new(
                "weather",
                "Report the current weather for a city. Use it when the user asks about weather.",
                Arc::new(Self),
            )
        }
    }

    /// Executes a tool call by name, for recipes that drive the registry by hand.
    pub async fn execute(
        registry: &norte_core::ToolRegistry,
        name: &str,
        call_id: norte_core::CallId,
        arguments: &serde_json::Value,
    ) -> Result<ToolOutput> {
        let tool = registry
            .get(&norte_core::ToolName::new(name)?)
            .ok_or_else(|| norte_core::Error::InvalidInput(format!("no tool named {name}")))?;
        tool.execute(&ToolContext::new().with_call(call_id), arguments)
            .await
    }
}
