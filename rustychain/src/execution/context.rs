use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, RwLock};

use crate::{
    Inference,
    execution::{
        environment::ExecutionEnvironment,
        node::{ExecutionNode, NodeId, NodeType},
    },
};

/// Usage statistics and performance counters for an execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionMetrics {
    pub total_tokens: usize,
    pub tool_calls: usize,
    pub llm_calls: usize,
    pub errors: usize,
    pub corrections: usize,
    pub duration_ms: u128,
    /// Extensible map for custom user-defined metrics.
    pub custom: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
/// Shared state container for a single Agent execution.
///
/// The context provides synchronized access to the execution environment,
/// real-time metrics, and administrative controls like tool approvals.
pub struct Context<S, EV, T>
where
    S: Into<String> + Send + Sync + 'static,
    EV: ExecutionEnvironment + Send + Sync + 'static,
    T: Send + Sync + 'static,
{
    pub execution_id: S,
    pub runtime: Arc<RwLock<EV>>,
    pub current_node: Arc<RwLock<Option<NodeId>>>,
    pub metrics: Arc<RwLock<ExecutionMetrics>>,
    pub state: Arc<Mutex<T>>,
}
impl<S, EV, T> Context<S, EV, T>
where
    S: Into<String> + Send + Sync + 'static,
    EV: ExecutionEnvironment + 'static,
    T: Send + Sync + 'static,
{
    pub fn new(execution_id: S, runtime: EV, state: Arc<Mutex<T>>) -> Self {
        Self {
            execution_id,
            runtime: Arc::new(RwLock::new(runtime)),
            current_node: Arc::new(RwLock::new(None)),
            metrics: Arc::new(RwLock::new(ExecutionMetrics::default())),
            state,
        }
    }

    pub async fn get_current_history(&self) -> crate::Result<Vec<Inference>> {
        let node_ptr = self.current_node.read().await;
        if let Some(node_id) = &*node_ptr {
            let runtime = self.runtime.read().await;
            runtime.to_history(node_id).await
        } else {
            Ok(vec![])
        }
    }

    pub async fn fork_branch(&self, from_node: &NodeId) -> crate::Result<NodeId> {
        let fork_node = ExecutionNode::new(
            NodeType::Checkpoint {
                message: format!("Branch fork from {:?}", from_node),
                requested_by: "system".to_string(),
                require_approval: false,
                next_id: None,
            },
            Some(from_node.clone()),
        );

        let runtime = self.runtime.read().await;
        runtime.add_node(fork_node).await
    }

    /// Get a mutable reference to the state
    pub async fn with_state<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        let mut state = self.state.lock().await;
        f(&mut *state)
    }

    /// Get current node data
    pub async fn get_current_node(&self) -> crate::Result<Option<ExecutionNode>> {
        let node_id = self.current_node.read().await;
        if let Some(id) = &*node_id {
            let runtime = self.runtime.read().await;
            let nodes = runtime.get_nodes().await;
            Ok(nodes.read().await.get(id).cloned())
        } else {
            Ok(None)
        }
    }

    /// Update node in the graph
    pub async fn update_node<F>(&self, node_id: &NodeId, f: F) -> crate::Result<()>
    where
        F: FnOnce(&mut ExecutionNode),
    {
        let runtime = self.runtime.read().await;
        let nodes = runtime.get_nodes().await;
        if let Some(node) = nodes.write().await.get_mut(node_id) {
            f(node);
        }
        Ok(())
    }
}
