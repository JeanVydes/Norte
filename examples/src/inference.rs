use rustychain::prelude::*;

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let llm = LLM::builder()
        .set_authorization("your_api_key_here")
        .set_model("gemini-2.5-flash")
        .set_provider(LLMProvider::Google)
        .set_system_prompt("You are a helpful assistant.")
        .build()?;

    // Across example you will find with inference as a text, here is the basic usage
    let response = llm.inference("Hello, how are you?").generate().await?;

    // But its just an abstration of Inference struct, here ways to create it
    let _: Inference = "Hello, how are you?".into();
    let _: Inference = (Role::User, "Hello, how are you?").into();
    let _ = Inference::as_user("Hello, how are you?");
    let _: Inference = InferenceContent {
        role: Role::User,
        text: Some("Hello, how are you?".to_string()),
        ..Default::default()
    }
    .into();
    let _ = Inference::new("Hello, how are you?");
    let _ = Inference::new(InferenceContent {
        text: Some("Hello, how are you?".to_string()),
        ..Default::default()
    });
    let _: Inference = Inference {
        content: InferenceContent {
            role: Role::User,
            text: Some("Hello, how are you?".to_string()),
            // as a slice of bytes
            audio: None,
            images: None,
        },
        // the rest of the fields are set internally as responses are processed
        // settings it as an input will have no effect
        ..Default::default()
    };

    println!("{}", response.content.text.unwrap_or_default());

    Ok(())
}
