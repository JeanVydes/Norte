use std::sync::Arc;

use rustychain::prelude::*;

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let llm = Arc::new(
        LLM::builder()
            .set_authorization("your_api_key_here")
            .set_model("gemini-embedding-001".to_owned())
            .set_provider(LLMProvider::Google)
            .build()?,
    );

    // The dimensionality depends on the model/provider used
    // And own requirements
    // In this example `gemini-embedding-001` supports up to 3072 dimensions, but we use 1536
    let vector = llm.embedding("Hello, world!", 1536).await?;

    println!("Embedding vector: {:?}", vector);

    Ok(())
}
