use futures_core::stream::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::Arc;

use crate::{
    llm::{GenerationConfig, LLM, conversation::Inference},
    prelude::*,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum InferenceMode {
    OneShot,
    Streaming,
}

#[derive(Clone, Debug)]
pub struct InferenceTask<'a> {
    pub history: &'a [Inference],
    pub inference: Inference,
    pub config: GenerationConfig,
    pub llm: Arc<LLM>,
}

impl<'a> InferenceTask<'a> {
    pub fn new(inference: Inference, llm: Arc<LLM>) -> Self {
        Self {
            history: &[],
            inference,
            config: GenerationConfig::default(),
            llm,
        }
    }

    pub fn with_history(mut self, history: &'a [Inference]) -> Self {
        self.history = history;
        self
    }

    pub fn with_config(mut self, config: GenerationConfig) -> Self {
        self.config = config;
        self
    }

    pub async fn generate(&mut self) -> crate::Result<Inference> {
        return self
            .llm
            .generation(self.history, &self.inference, self.config.clone())
            .await;
    }

    pub async fn stream(
        &mut self,
    ) -> crate::Result<Pin<Box<dyn Stream<Item = crate::Result<Inference>> + Send + 'static>>> {
        let config = Arc::new(self.config.clone());
        return self.llm.stream(self.history, &self.inference, config).await;
    }
}
