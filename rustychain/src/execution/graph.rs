// graph.rs

use crate::{
    Inference,
    execution::{
        environment::ExecutionEnvironment,
        node::{ExecutionNode, NodeId, NodeType},
    },
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

#[derive(Debug, Default, Clone)]
pub struct ExecutionGraph {
    pub root: NodeId,
    pub nodes: Arc<RwLock<HashMap<NodeId, ExecutionNode>>>,
}

impl ExecutionGraph {
    pub fn new() -> Self {
        Self {
            root: NodeId::new(),
            nodes: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait::async_trait]
impl ExecutionEnvironment for ExecutionGraph {
    async fn get_nodes(&self) -> Arc<RwLock<HashMap<NodeId, ExecutionNode>>> {
        self.nodes.clone()
    }

    async fn add_node(&self, node: ExecutionNode) -> crate::Result<NodeId> {
        let node_id = node.id.clone();
        let mut nodes = self.nodes.write().await;
        if let Some(parent_id) = &node.parent
            && let Some(parent_node) = nodes.get_mut(parent_id)
        {
            parent_node.children.push(node_id.clone());
        }
        nodes.insert(node_id.clone(), node);
        Ok(node_id)
    }

    async fn get_root_path(&self, node_id: &NodeId) -> crate::Result<Vec<NodeId>> {
        let mut path = Vec::new();
        let mut current = Some(node_id.clone());
        let nodes = self.nodes.read().await;
        while let Some(id) = current {
            path.push(id.clone());
            current = nodes.get(&id).and_then(|n| n.parent.clone());
        }
        path.reverse();
        Ok(path)
    }

    async fn get_branch(&self, leaf_id: &NodeId) -> crate::Result<Vec<NodeId>> {
        self.get_root_path(leaf_id).await
    }

    async fn to_history(&self, node_id: &NodeId) -> crate::Result<Vec<Inference>> {
        let path = self.get_root_path(node_id).await?;
        let nodes = self.nodes.read().await;
        let mut history = Vec::new();

        for id in path {
            if let Some(node) = nodes.get(&id) {
                match &node.node_type {
                    NodeType::Inference(inf) => {
                        if inf.content.text.is_some() || !inf.function_calls.is_empty() {
                            history.push(inf.clone());
                        }
                    }
                    NodeType::InferenceResult(inf) => history.push(inf.clone()),
                    NodeType::ToolResults(results) => {
                        if !results.is_empty() {
                            history.push(Inference::with_function_results(results.clone()));
                        }
                    }
                    NodeType::Correction { correction, .. } => history.push(correction.clone()),
                    _ => {}
                }
            }
        }

        let mut clean_history: Vec<Inference> = Vec::new();
        for inf in history {
            if let Some(last) = clean_history.last()
                && last.content.role == crate::Role::Tool
                && inf.content.role == crate::Role::Tool
                && last.function_results.first().map(|r| &r.context)
                    == inf.function_results.first().map(|r| &r.context)
            {
                continue;
            }

            clean_history.push(inf);
        }

        Ok(clean_history)
    }

    async fn clear(&self) -> crate::Result<()> {
        self.nodes.write().await.clear();
        Ok(())
    }
}
