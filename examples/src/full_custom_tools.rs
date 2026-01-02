/// This is the full implementation of a custom tool in RustyChain.
/// The `custom_tool.rs` example has been expanded here to show
/// how to manually implement the necessary traits without using macros.
use rustychain::{Inference, prelude::*};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
pub struct SumArgs {
    #[schemars(description = "First integer to sum.")]
    pub a: i64,
    #[schemars(description = "Second integer to sum.")]
    pub b: i64,
}

#[derive(Clone)]
pub struct SumTool {}

#[async_trait::async_trait]
impl FnExecutor<SumArgs, i64> for SumTool {
    async fn call(&self, args: SumArgs) -> rustychain::Result<i64> {
        Ok(args.a + args.b)
    }
}

impl FnDeclarator<SumArgs, i64> for SumTool {
    fn declare(&self) -> FunctionDeclaration<SumArgs, i64> {
        FunctionDeclaration {
            name: "sum_integers",
            description: "Returns the sum of two integers.",
            parameters: schemars::schema_for!(SumArgs),
            executor: std::sync::Arc::new(self.clone()),
        }
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
        .inference(Inference::as_user("What is the sum of 42 and 58?"))
        .generate()
        .await?;

    // should return tool call
    println!("{:?}", response.function_calls);

    Ok(())
}
