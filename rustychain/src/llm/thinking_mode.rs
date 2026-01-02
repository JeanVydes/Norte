use serde::{Deserialize, Serialize};

/// Different modes for LLM "thinking" or planning.
///
/// - `None`: No additional thinking time.
/// - `Dynamic`: Let the LLM decide how much time to spend thinking.
/// - `Sized(i32)`: Allocate a specific amount of time or budget for thinking.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Eq, Hash, PartialOrd, Ord)]
pub enum ThinkingMode {
    None,
    Dynamic,
    Effort(ThinkingEffort),
    Sized(i32),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Eq, Hash, PartialOrd, Ord)]
pub enum ThinkingEffort {
    Low,
    Medium,
    High,
}
