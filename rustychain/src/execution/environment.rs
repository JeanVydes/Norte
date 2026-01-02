use std::{collections::HashMap, sync::Arc};

use tokio::sync::RwLock;

use crate::{
    Inference,
    execution::node::{ExecutionNode, NodeId},
};

#[async_trait::async_trait]
pub trait ExecutionEnvironment: Send + Sync {
    async fn get_nodes(&self) -> Arc<RwLock<HashMap<NodeId, ExecutionNode>>>;
    async fn add_node(&self, node: ExecutionNode) -> crate::Result<NodeId>;
    async fn get_root_path(&self, node_id: &NodeId) -> crate::Result<Vec<NodeId>>;
    async fn get_branch(&self, leaf_id: &NodeId) -> crate::Result<Vec<NodeId>>;
    async fn to_history(&self, node_id: &NodeId) -> crate::Result<Vec<Inference>>;
    async fn clear(&self) -> crate::Result<()>;
}
