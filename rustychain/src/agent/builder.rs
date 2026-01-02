// builder.rs

use crate::{
    LLM,
    agent::{config::AgentConfig, definitions::Agent},
    execution::{
        graph::ExecutionGraph,
        listeners::{ArcEventListener, EventListener, InterceptorEntry},
    },
};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct AgentBuilder<S, T>
where
    S: Into<String> + Clone + Send + Sync + 'static,
    T: Send + Sync + 'static + Default + Clone,
{
    name: Option<String>,
    execution_id: Option<S>,
    llm: Option<Arc<LLM>>,
    config: Option<AgentConfig>,
    state: Option<Arc<Mutex<T>>>,
    observers: Vec<ArcEventListener<S, ExecutionGraph, T>>,
    interceptors: Vec<InterceptorEntry<S, ExecutionGraph, T>>,
}

impl<S, T> Default for AgentBuilder<S, T>
where
    S: Into<String> + Clone + Send + Sync + 'static,
    T: Send + Sync + 'static + Default + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<S, T> AgentBuilder<S, T>
where
    S: Into<String> + Clone + Send + Sync + 'static,
    T: Send + Sync + 'static + Default + Clone,
{
    pub fn new() -> Self {
        Self {
            name: None,
            execution_id: None,
            llm: None,
            config: None,
            state: None,
            observers: Vec::new(),
            interceptors: Vec::new(),
        }
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn execution_id(mut self, id: S) -> Self {
        self.execution_id = Some(id);
        self
    }

    pub fn llm(mut self, llm: Arc<LLM>) -> Self {
        self.llm = Some(llm);
        self
    }

    pub fn config(mut self, config: AgentConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn state(mut self, state: Arc<Mutex<T>>) -> Self {
        self.state = Some(state);
        self
    }

    /// Add an observer listener (non-blocking, for logging/metrics/UI)
    pub fn add_observer(mut self, listener: Arc<dyn EventListener<S, ExecutionGraph, T>>) -> Self {
        self.observers.push(listener);
        self
    }

    /// Add an interceptor listener (blocking, for control/approval)
    pub fn add_interceptor(
        mut self,
        listener: Arc<dyn EventListener<S, ExecutionGraph, T>>,
        priority: i32,
    ) -> Self {
        self.interceptors.push((priority, listener));
        self
    }

    pub async fn build(mut self) -> crate::Result<Agent<S, ExecutionGraph, T>> {
        let name = self.name.clone().unwrap_or_else(|| "Agent".to_string());
        let execution_id = self.execution_id.ok_or_else(|| {
            crate::Error::Internal("Execution ID is required to build Agent".into())
        })?;
        let llm = self.llm.ok_or_else(|| {
            crate::Error::Internal("LLM instance is required to build Agent".into())
        })?;
        let runtime = ExecutionGraph::new();
        let state = self
            .state
            .unwrap_or_else(|| Arc::new(Mutex::new(T::default())));

        let mut agent = Agent::new(name, execution_id, llm, runtime, state);

        if let Some(config) = self.config {
            agent = agent.with_config(config);
        }

        // Register all observers
        for observer in self.observers {
            agent.on_event(observer).await;
        }

        // Sort interceptors by priority (higher = runs first)
        self.interceptors.sort_by(|a, b| b.0.cmp(&a.0));
        for (priority, interceptor) in self.interceptors {
            agent.intercept(interceptor, priority).await;
        }

        Ok(agent)
    }
}
