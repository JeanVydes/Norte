use crate::{GenerationConfig, providers::LLMProvider};
use futures_core::stream::Stream;
use secrecy::SecretString;
use std::pin::Pin;
use std::sync::Arc;

use crate::llm::{
    builder::LLMBuilder, conversation::Inference, function::AnyFunction,
    inference_task::InferenceTask,
};

/// Struct representing a Large Language Model (LLM) with its configuration and capabilities.
#[derive(Clone, Debug)]
pub struct LLM {
    pub model: String,
    pub system_prompt: String,
    pub provider: LLMProvider,
    pub authorization: Option<SecretString>,
    pub endpoint: Option<String>,
    pub tools: Vec<Arc<dyn AnyFunction>>,
}

#[async_trait::async_trait]
pub trait LLMGeneration {
    /// Performs text generation based on the provided history and inference.
    async fn generation(
        &self,
        history: &[Inference],
        inference: &Inference,
        config: GenerationConfig,
    ) -> crate::Result<Inference>;
}

#[async_trait::async_trait]
pub trait LLMStreaming {
    /// Streams text generation results based on the provided history and inference.
    async fn stream(
        &self,
        history: &[Inference],
        inference: &Inference,
        config: Arc<GenerationConfig>,
    ) -> crate::Result<Pin<Box<dyn Stream<Item = crate::Result<Inference>> + Send + 'static>>>;
}

#[async_trait::async_trait]
pub trait LLMEmbedding {
    /// Generates an embedding for the given text.
    async fn embedding(&self, text: &str, dim: i32) -> crate::Result<Vec<f32>>;
}

impl LLM {
    /// Creates a new inference with this model.
    pub fn inference<'a>(&self, inference: impl Into<Inference> + 'a) -> InferenceTask<'a> {
        InferenceTask::new(inference.into(), Arc::new(self.clone()))
    }

    /// Adds a single tool to the LLM.
    pub fn add_tool(&mut self, tool: Arc<impl AnyFunction + 'static>) {
        self.tools.push(tool);
    }

    /// Adds multiple tools to the LLM.
    pub fn add_tools(&mut self, tools: Vec<Arc<impl AnyFunction + 'static>>) {
        for tool in tools {
            self.tools.push(tool);
        }
    }

    pub fn exists_tool(&self, tool_name: &str) -> bool {
        for tool in &self.tools {
            if tool.name() == tool_name {
                return true;
            }
        }
        false
    }

    pub fn get_tools(&self) -> &Vec<Arc<dyn AnyFunction>> {
        &self.tools
    }

    pub fn get_tool(&self, tool_name: &str) -> Option<Arc<dyn AnyFunction>> {
        for tool in &self.tools {
            if tool.name() == tool_name {
                return Some(tool.clone());
            }
        }
        None
    }

    /// Creates a new LLM builder.
    pub fn builder() -> LLMBuilder {
        LLMBuilder {
            model: None,
            system_prompt: None,
            provider: None,
            authorization: None,
            tools: vec![],
            endpoint: None,
        }
    }
}
