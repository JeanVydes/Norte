use std::sync::Arc;
use crate::{GenerationConfig, context::manager::ContextManager};

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub max_iterations: usize,
    pub context_management: Option<Arc<ContextManager>>,
    pub generation_config: GenerationConfig,
    pub iteration_warning_threshold: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            context_management: None,
            generation_config: GenerationConfig::default(),
            iteration_warning_threshold: 5,
        }
    }
}
