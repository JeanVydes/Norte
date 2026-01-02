use rustychain::prelude::*;

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let llm = LLM::builder()
        .set_authorization("your_api_key_here")
        .set_model("xiaomi/mimo-v2-flash:free")
        .set_provider(LLMProvider::OpenRouter)
        .set_system_prompt("You are a helpful assistant.")
        .build()?;

    let response = llm.inference("Hello, how are you?").generate().await?;

    println!("Response as Inference {:?}", response);

    Ok(())
}
