use rustychain::prelude::*;

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let llm = LLM::builder()
        .set_authorization("your_api_key_here")
        .set_model("gemini-2.5-flash")
        .set_provider(LLMProvider::Google)
        .set_system_prompt(
            "You are a helpful assistant that always uses tools to answer user queries.",
        )
        .add_tool(
            rustychain_tools_suite::integrations::search::DuckDuckGoSearchTool::new().declare(),
        )
        .build()?;

    let response = llm
        .inference("search what is the capital of france")
        .generate()
        .await?;

    // should return tool call
    println!("{:?}", response);

    Ok(())
}
