use crate::prelude::*;
use crate::providers::ProviderAbstractionLayer;
use std::sync::Arc;

#[async_trait::async_trait]
impl LLMGeneration for LLM {
    /// Performs text generation based on the provided history and message.
    #[allow(unreachable_patterns)]
    async fn generation(
        &self,
        history: &[Inference],
        inference: &Inference,
        config: GenerationConfig,
    ) -> crate::Result<Inference> {
        let config = Arc::new(config);
        log::trace!("[LLM_GENERATION][REQUEST]");
        log::trace!("[LLM_GENERATION][HISTORY] {:?}", history);
        log::trace!("[LLM_STREAMING][PROVIDER] {:?}", self.provider);
        log::trace!("[LLM_GENERATION][INFERENCE] {:?}", inference);
        log::trace!("[LLM_GENERATION][GENERATION_CONFIG] {:?}", config);

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

                let res = req.execute().await?;
                log::trace!("[LLM_GENERATION/GOOGLE][RAW_RESPONSE] {:?}", res);
                Ok(GoogleProvider::to_inference_from_response(
                    &res,
                    Some(config),
                ))
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

                let res = provider
                    .client()?
                    .chat()
                    .create(req.build().map_err(|e| {
                        crate::Error::Generic(format!("OpenAI request build failed: {}", e))
                    })?)
                    .await
                    .map_err(|e| crate::Error::Generic(format!("OpenAI request failed: {}", e)))?;

                log::trace!("[LLM_GENERATION/OPENAI][RAW_RESPONSE] {:?}", res);

                Ok(OpenAICompatibleProvider::to_inference_from_response(
                    &res,
                    Some(config),
                ))
            }
            #[cfg(feature = "anthropic")]
            LLMProvider::Anthropic => Err(crate::Error::Generic(
                "Anthropic provider not yet implemented".to_owned(),
            )),
            _ => Err(crate::Error::Unsupported(
                "Generation not supported for this provider".to_owned(),
            )),
        }
    }
}
