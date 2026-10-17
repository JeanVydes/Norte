use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::message::Message;
use crate::id::NodeId;

/// Node kinds the core understands. Persisted kinds evolve additively; a reader
/// that meets an unknown kind reports a typed error instead of guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NodeKind {
    Message,
}

#[must_use]
pub const fn node_schema_version() -> u16 {
    1
}

/// An immutable node in the conversation graph.
///
/// Nodes belong to no session: sessions share ancestors, and branching only
/// adds nodes and moves a pointer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<NodeId>,
    pub kind: NodeKind,
    pub message: Message,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
    #[serde(default = "node_schema_version")]
    pub schema_version: u16,
}

impl Node {
    #[must_use]
    pub fn new(kind: NodeKind, message: Message, parent: Option<NodeId>) -> Self {
        Self {
            id: NodeId::new(),
            parent,
            kind,
            message,
            created_at: Utc::now(),
            metadata: BTreeMap::new(),
            schema_version: node_schema_version(),
        }
    }

    #[must_use]
    pub fn message(message: Message, parent: Option<NodeId>) -> Self {
        Self::new(NodeKind::Message, message, parent)
    }
}
