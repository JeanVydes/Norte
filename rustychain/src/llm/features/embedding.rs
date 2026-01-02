use crate::prelude::*;

#[async_trait::async_trait]
impl LLMEmbedding for LLM {
    /// Generates an embedding for the given text.
    async fn embedding(&self, text: &str, dim: i32) -> crate::Result<Vec<f32>> {
        log::trace!("[LLM_EMBEDDING][REQUEST]");
        log::trace!("[LLM_EMBEDDING][PROVIDER] {:?}", self.provider);
        log::trace!("[LLM_EMBEDDING][TEXT] {}", text);
        log::trace!("[LLM_EMBEDDING][DIM] {}", dim);

        match self.provider {
            #[cfg(feature = "google")]
            LLMProvider::Google => {
                use crate::providers::{ProviderAbstractionLayer, google::GoogleProvider};

                let client = GoogleProvider::new(
                    self.endpoint
                        .clone()
                        .unwrap_or(self.provider.default_api_base().to_string()),
                    self.authorization.clone(),
                )
                .client()?;

                let res = client
                    .embed_content()
                    .with_output_dimensionality(dim)
                    .with_text(text)
                    .execute()
                    .await?;

                Ok(res.embedding.values)
            }
            #[cfg(feature = "openai")]
            LLMProvider::OpenAI
            | LLMProvider::Ollama
            | LLMProvider::OpenRouter
            | LLMProvider::DeepSeek
            | LLMProvider::Groq
            | LLMProvider::Together
            | LLMProvider::Mistral
            | LLMProvider::Perplexity
            | LLMProvider::Fireworks
            | LLMProvider::XAI
            | LLMProvider::LMStudio
            | LLMProvider::Poe => {
                use crate::providers::{
                    ProviderAbstractionLayer, openai::OpenAICompatibleProvider,
                };

                let provider = OpenAICompatibleProvider::new(
                    self.endpoint
                        .clone()
                        .unwrap_or(self.provider.default_api_base().to_string()),
                    self.authorization.clone(),
                );

                let req = OpenAICompatibleProvider::new_embedding_request(
                    &self.model,
                    dim as u32,
                    vec![text],
                )?;

                let res = provider
                    .client()?
                    .embeddings()
                    .create(req)
                    .await
                    .map_err(|e| {
                        crate::Error::Generic(format!("OpenAI embedding request failed: {}", e))
                    })?;

                if let Some(data) = res.data.first() {
                    Ok(data.embedding.clone())
                } else {
                    Err(crate::Error::Generic(
                        "No embedding data returned from OpenAI".to_owned(),
                    ))
                }
            }
            _ => Err(crate::Error::Unsupported(
                "Embedding not supported for this provider".to_owned(),
            )),
        }
    }
}
