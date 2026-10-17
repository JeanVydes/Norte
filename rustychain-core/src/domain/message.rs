use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::media::MediaPart;
use crate::domain::part::{Part, ReasoningPart, TextPart, ToolCallPart, ToolResultPart};
use crate::domain::role::Role;
use crate::id::{Digest, GenerationId, ModelRef};

/// Canonical reason a generation stopped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
    Refusal,
    Cancelled,
    Other(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MessageStatus {
    Complete,
    Interrupted,
    Truncated,
}

/// Whether a message participates in the provider's cached prompt prefix.
///
/// `Critical` is the default. `Ephemeral` messages (live run ids, volatile tool
/// scratch, transient hints) are materialized strictly after every critical
/// message so they never shift the cached prefix. Marking a message ephemeral is
/// an explicit, persisted host decision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CacheClass {
    #[default]
    Critical,
    Ephemeral,
}

impl CacheClass {
    #[must_use]
    pub fn is_critical(&self) -> bool {
        matches!(self, CacheClass::Critical)
    }

    #[must_use]
    pub fn is_ephemeral(&self) -> bool {
        matches!(self, CacheClass::Ephemeral)
    }
}

/// Token accounting, with an extension map so new vendor counters survive a
/// round-trip instead of being lost.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extension: BTreeMap<String, u64>,
}

impl Usage {
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.unwrap_or(0) + self.output_tokens.unwrap_or(0)
    }
}

/// Model-side provenance of an assistant message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MessageOrigin {
    pub model: ModelRef,
    pub generation: GenerationId,
    pub finish_reason: FinishReason,
    pub usage: Usage,
    pub status: MessageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_digest: Option<Digest>,
}

/// One turn of a conversation: a semantic role, an ordered list of parts and,
/// for assistant messages, the model-side origin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<MessageOrigin>,
    #[serde(default, skip_serializing_if = "CacheClass::is_critical")]
    pub cache: CacheClass,
}

impl Message {
    #[must_use]
    pub fn new(role: Role, parts: Vec<Part>) -> Self {
        Self {
            role,
            parts,
            origin: None,
            cache: CacheClass::Critical,
        }
    }

    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, vec![Part::Text(TextPart::new(text))])
    }

    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::new(Role::Assistant, vec![Part::Text(TextPart::new(text))])
    }

    #[must_use]
    pub fn system(text: impl Into<String>) -> Self {
        Self::new(Role::System, vec![Part::Text(TextPart::new(text))])
    }

    #[must_use]
    pub fn developer(text: impl Into<String>) -> Self {
        Self::new(Role::Developer, vec![Part::Text(TextPart::new(text))])
    }

    /// The single message that carries every result of an assistant turn, in
    /// call order.
    #[must_use]
    pub fn tool(results: Vec<ToolResultPart>) -> Self {
        Self::new(
            Role::Tool,
            results.into_iter().map(Part::ToolResult).collect(),
        )
    }

    #[must_use]
    pub fn with_origin(mut self, origin: MessageOrigin) -> Self {
        self.origin = Some(origin);
        self
    }

    /// Marks this message's cache class. The cache-critical default is kept
    /// unless the host explicitly opts a message out.
    #[must_use]
    pub fn with_cache(mut self, cache: CacheClass) -> Self {
        self.cache = cache;
        self
    }

    #[must_use]
    pub fn is_ephemeral(&self) -> bool {
        self.cache.is_ephemeral()
    }

    #[must_use]
    pub fn push(mut self, part: Part) -> Self {
        self.parts.push(part);
        self
    }

    /// Concatenated text of every text part, in order.
    #[must_use]
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|part| match part {
                Part::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[must_use]
    pub fn reasoning(&self) -> Vec<&ReasoningPart> {
        self.parts
            .iter()
            .filter_map(|part| match part {
                Part::Reasoning(reasoning) => Some(reasoning),
                _ => None,
            })
            .collect()
    }

    #[must_use]
    pub fn tool_calls(&self) -> Vec<&ToolCallPart> {
        self.parts.iter().filter_map(Part::as_tool_call).collect()
    }

    #[must_use]
    pub fn tool_results(&self) -> Vec<&ToolResultPart> {
        self.parts.iter().filter_map(Part::as_tool_result).collect()
    }

    #[must_use]
    pub fn media(&self) -> Vec<&MediaPart> {
        self.parts.iter().filter_map(Part::as_media).collect()
    }

    #[must_use]
    pub fn has_tool_calls(&self) -> bool {
        self.parts.iter().any(|part| part.as_tool_call().is_some())
    }

    #[must_use]
    pub fn finish_reason(&self) -> Option<&FinishReason> {
        self.origin.as_ref().map(|origin| &origin.finish_reason)
    }
}

impl From<&str> for Message {
    fn from(text: &str) -> Self {
        Self::user(text)
    }
}

impl From<String> for Message {
    fn from(text: String) -> Self {
        Self::user(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{CallId, ModelId, ProviderId};

    #[test]
    fn text_is_the_concatenation_of_text_parts() {
        let message = Message::new(
            Role::Assistant,
            vec![Part::text("hello "), Part::text("world")],
        );
        assert_eq!(message.text(), "hello world");
    }

    #[test]
    fn tool_message_keeps_call_order() {
        let first = CallId::new();
        let second = CallId::new();
        let message = Message::tool(vec![
            ToolResultPart::text(first, "a"),
            ToolResultPart::text(second, "b"),
        ]);
        let results = message.tool_results();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].call_id, first);
        assert_eq!(results[1].call_id, second);
    }

    #[test]
    fn origin_round_trips_through_serde() {
        let origin = MessageOrigin {
            model: ModelRef::new(
                ProviderId::new("openai").expect("provider"),
                ModelId::new("gpt-5").expect("model"),
            ),
            generation: GenerationId::new(),
            finish_reason: FinishReason::ToolCalls,
            usage: Usage::default(),
            status: MessageStatus::Complete,
            request_digest: None,
        };
        let message = Message::assistant("x").with_origin(origin);
        let json = serde_json::to_string(&message).expect("serialize");
        let decoded: Message = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, message);
    }
}
