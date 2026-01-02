use futures_util::stream::StreamExt;
use rustychain::prelude::*;

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let llm = LLM::builder()
        .set_authorization("your_api_key_here")
        .set_model("gemini-2.5-flash")
        .set_provider(LLMProvider::Google)
        .set_system_prompt("You are a helpful assistant.")
        .build()?;

    let mut stream = llm
        .inference("Tell me a joke about computers.")
        .stream()
        .await?;

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(part) => {
                if let Some(text) = part.content.text {
                    print!("{}", text);
                }
            }
            Err(_) => break,
        }
    }

    Ok(())
}
