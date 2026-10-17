use serde::{Deserialize, Serialize};

/// Semantic role in the conversation graph.
///
/// Wire role names never reach this type: adapters map their own vocabulary at
/// the boundary, and system/developer messages may appear anywhere in a
/// conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Role {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

impl Role {
    #[must_use]
    pub fn is_instruction(self) -> bool {
        matches!(self, Role::System | Role::Developer)
    }
}
