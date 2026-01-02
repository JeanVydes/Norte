use rustychain::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
pub struct SumArgs {
    #[schemars(description = "First integer to sum.")]
    pub a: i64,
    #[schemars(description = "Second integer to sum.")]
    pub b: i64,
}

#[declare_function(
    name = "sum_integers",
    description = "Returns the sum of two integers.",
    args = SumArgs,
    result = i64
)]
pub struct SumTool {}

impl SumTool {
    pub async fn execute(&self, args: SumArgs) -> rustychain::Result<i64> {
        Ok(args.a + args.b)
    }
}

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let llm = LLM::builder()
        .set_authorization("your_api_key_here")
        .set_model("gemini-2.5-flash")
        .set_provider(LLMProvider::Google)
        .set_system_prompt("You are a helpful assistant that uses tools to answer user queries.")
        .add_tool(SumTool {}.declare())
        .build()?;

    let response = llm
        .inference("What is the sum of 42 and 58?")
        .generate()
        .await?;

    println!("{:?}", response.function_calls);

    Ok(())
}
