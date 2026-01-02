use serde::{Deserialize, Serialize};

/// Reason for finishing the generation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FinishReason {
    Stop,
    MaxTokens,
    Safety,
    Recitation,
    Blocklist,
    ProhibitedContent,
    Spii,
    MalformedFunctionCall,
    ImageSafety,
    UnexpectedToolCall,
    TooManyToolCalls,
    ToolCall,
    Other(String),
    Unspecified,
}
