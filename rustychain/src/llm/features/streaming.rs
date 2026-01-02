use crate::prelude::*;
use crate::providers::ProviderAbstractionLayer;
use futures_core::Stream;
use futures_util::stream::{StreamExt, TryStreamExt};
use std::{pin::Pin, sync::Arc};

#[async_trait::async_trait]
impl LLMStreaming for LLM {
    /// Streams text generation results based on the provided history and inference.
    #[allow(unreachable_patterns)]
    async fn stream(
        &self,
        history: &[Inference],
        inference: &Inference,
        config: Arc<GenerationConfig>,
    ) -> crate::Result<Pin<Box<dyn Stream<Item = crate::Result<Inference>> + Send + 'static>>> {
        log::trace!("[LLM_STREAMING][REQUEST]");
        log::trace!("[LLM_STREAMING][PROVIDER] {:?}", self.provider);
        log::trace!("[LLM_STREAMING][INFERENCE] {:?}", inference);
        log::trace!("[LLM_STREAMING][GENERATION_CONFIG] {:?}", config);

        match self.provider {
            #[cfg(feature = "google")]
            LLMProvider::Google => {
                use crate::providers::google::GoogleProvider;

                let provider = GoogleProvider::new(
                    self.endpoint
                        .clone()
                        .unwrap_or(self.provider.default_api_base().to_string()),
                    self.authorization.clone(),
                );

                let chat_completion_req = crate::providers::ChatCompletionRequest {
                    model: &self.model,
                    messages: history,
                    inference,
                    config: &config,
                    system_prompt: &self.system_prompt,
                    tools: &self.tools,
                };

                let req = provider.new_chat_request(chat_completion_req)?;
                let stream = req.execute_stream().await?;
                let mapped = stream.into_stream().map(move |res| match res {
                    Ok(gen_resp) => Ok(GoogleProvider::to_inference_from_response(
                        &gen_resp,
                        Some(config.clone()),
                    )),
                    Err(e) => Err(crate::Error::from(e)),
                });

                Ok(Box::pin(mapped))
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
                use crate::providers::openai::OpenAICompatibleProvider;

                let provider = OpenAICompatibleProvider::new(
                    self.endpoint
                        .clone()
                        .unwrap_or(self.provider.default_api_base().to_string()),
                    self.authorization.clone(),
                );

                let chat_completion_req = crate::providers::ChatCompletionRequest {
                    model: &self.model,
                    messages: history,
                    inference,
                    config: &config,
                    system_prompt: &self.system_prompt,
                    tools: &self.tools,
                };

                let req = provider.new_chat_request(chat_completion_req)?;

                let stream = provider
                    .client()?
                    .chat()
                    .create_stream(req.build().map_err(|e| {
                        crate::Error::Generic(format!("OpenAI request build failed: {}", e))
                    })?)
                    .await
                    .map_err(|e| crate::Error::Generic(format!("OpenAI request failed: {}", e)))?;

                let mapped = stream.map(move |res| match res {
                    Ok(chunk) => {
                        let inference = OpenAICompatibleProvider::to_inference_from_stream_response(
                            &chunk,
                            Some(config.clone()),
                        );
                        Ok(inference)
                    }
                    Err(e) => Err(crate::Error::Generic(format!(
                        "OpenAI streaming error: {}",
                        e
                    ))),
                });

                Ok(Box::pin(mapped))
            }
            _ => Err(crate::Error::Unsupported(
                "Provider not supported for streaming".to_owned(),
            )),
        }
    }
}
