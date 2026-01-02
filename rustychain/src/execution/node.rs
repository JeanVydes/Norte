use crate::{FunctionResult, Inference};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}
impl Default for NodeId {
    fn default() -> Self {
        Self::new()
    }
}
impl From<String> for NodeId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

#[derive(Clone)]
pub enum NodeType {
    Inference(Inference),
    InferenceResult(Inference),
    ToolResults(Vec<FunctionResult>),

    // Nodos de control de flujo
    Branching {
        condition: Arc<dyn Fn(&Inference) -> bool + Send + Sync>,
        branch_a_id: NodeId,
        branch_b_id: NodeId,
    },
    Checkpoint {
        message: String,
        requested_by: String,
        require_approval: bool,
        next_id: Option<NodeId>,
    },
    Correction {
        correction: Inference,
        reason: String,
        original: NodeId,
        next: Option<NodeId>,
    },
}

// Implementación manual de Debug para evitar ruido
impl std::fmt::Debug for NodeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeType::Inference(_) => write!(f, "Inference"),
            NodeType::InferenceResult(_) => write!(f, "InferenceResult"),
            NodeType::ToolResults(res) => write!(f, "ToolResults(count: {})", res.len()),
            NodeType::Branching { .. } => write!(f, "Branching"),
            NodeType::Checkpoint { message, .. } => write!(f, "Checkpoint({})", message),
            NodeType::Correction { reason, .. } => write!(f, "Correction({})", reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum NodeStatus {
    Pending,
    Running,
    Completed,
    Failed(String),
    Cancelled,
    Paused,
}

#[derive(Debug, Clone)]
pub struct ExecutionNode {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub node_type: NodeType,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub metadata: HashMap<String, Value>,
    pub status: NodeStatus,
}

impl ExecutionNode {
    pub fn new(node_type: NodeType, parent: Option<NodeId>) -> Self {
        Self {
            id: NodeId::new(),
            parent,
            children: Vec::new(),
            node_type,
            timestamp: chrono::Utc::now(),
            metadata: HashMap::new(),
            status: NodeStatus::Pending,
        }
    }
}

// Implementación simple de Serialize para evitar boilerplate innecesario
impl Serialize for ExecutionNode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("ExecutionNode", 6)?;
        state.serialize_field("id", &self.id.0)?;
        state.serialize_field("type", &format!("{:?}", self.node_type))?;
        state.serialize_field("status", &format!("{:?}", self.status))?;
        state.serialize_field("timestamp", &self.timestamp.to_rfc3339())?;
        state.end()
    }
}
