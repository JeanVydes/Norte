use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::media::MediaPart;
use crate::domain::part::{Part, ReasoningPart, TextPart, ToolCallPart, ToolResultPart};
use crate::domain::role::Role;
use crate::id::{Digest, GenerationId, ModelRef};

/// Canonical reason a generation stopped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FinishReason {
    /// The model finished its answer.
    Stop,
    /// The model asked for tool calls.
    ToolCalls,
    /// The output hit the token limit.
    Length,
    /// The provider's content filter stopped it.
    ContentFilter,
    /// The model refused.
    Refusal,
    /// The caller cancelled.
    Cancelled,
    /// A reason with no neutral name.
    Other(String),
}

/// Whether a message is whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MessageStatus {
    /// Whole.
    Complete,
    /// Cut short by a cancellation or a failure.
    Interrupted,
    /// Cut short by a limit.
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
    /// Part of the cached prefix.
    #[default]
    Critical,
    /// Materialized after the cached prefix.
    Ephemeral,
}

impl CacheClass {
    /// Whether the message is part of the cached prefix.
    #[must_use]
    pub fn is_critical(&self) -> bool {
        matches!(self, CacheClass::Critical)
    }

    /// Whether the message is materialized after the cached prefix.
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
    /// Prompt tokens. The cache counters below are part of this count, so a
    /// cache share never exceeds the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Tokens the model produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Reasoning tokens, where the provider separates them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// Tokens served from the provider's cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    /// Tokens written into the provider's cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// Audio input tokens, where the provider counts them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_input_tokens: Option<u64>,
    /// Audio output tokens, where the provider counts them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_output_tokens: Option<u64>,
    /// A counter with no neutral field, kept under its own key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extension: BTreeMap<String, u64>,
    /// What the vendor reported and this vocabulary does not model: the price a
    /// request cost, a flag, a breakdown with no neutral field — and whatever a
    /// vendor ships that this crate has never heard of. Kept verbatim, so the
    /// parser is not where an unknown field dies.
    ///
    /// An adapter fills it through [`VendorUsage`], which also keeps the two
    /// sides honest: claiming is what removes a field from here, so a field an
    /// adapter forgets to map is visible rather than lost.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vendor: BTreeMap<String, Value>,
}

impl Usage {
    /// Input plus output tokens, as far as the provider reported them.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.unwrap_or(0) + self.output_tokens.unwrap_or(0)
    }
}

/// Reads one vendor usage object into the neutral counters.
///
/// A vendor reports more than this vocabulary names, and the vocabulary grows
/// slower than the vendors do: an `OpenAI`-compatible payload carries `cost` and
/// `is_byok`, Anthropic a `cache_creation` breakdown, Gemini token detail
/// arrays. An adapter claims here what it models — [`take`](Self::take) removes a
/// counter from the object — and [`remainder`](Self::remainder) hands back
/// everything no claim touched, which lands in [`Usage::vendor`].
#[derive(Debug)]
pub struct VendorUsage {
    left: Value,
}

impl VendorUsage {
    /// Starts a read of one vendor usage object.
    #[must_use]
    pub fn new(vendor: &Value) -> Self {
        Self {
            left: vendor.clone(),
        }
    }

    /// Claims the counter at `path`, returning it and removing it from the
    /// remainder.
    ///
    /// A value that is not an unsigned integer stays where it is: a vendor that
    /// reports a count as a string or a float belongs in `vendor`, not in a
    /// deprecated, coerced neutral field.
    pub fn take(&mut self, path: &[&str]) -> Option<u64> {
        let (last, parents) = path.split_last()?;
        let mut cursor = &mut self.left;
        for step in parents {
            cursor = cursor.get_mut(*step)?;
        }
        let claimed = cursor.get(*last)?.as_u64()?;
        cursor.as_object_mut()?.remove(*last);
        Some(claimed)
    }

    /// Everything no claim touched, with the objects the claims emptied pruned
    /// away: a payload that is fully modelled leaves no map at all.
    #[must_use]
    pub fn remainder(self) -> BTreeMap<String, Value> {
        prune(self.left)
    }
}

/// The fields of a vendor object that no claim touched. An object whose every
/// field was claimed is dropped; a field that is not an object is kept whole.
fn prune(value: Value) -> BTreeMap<String, Value> {
    let Value::Object(map) = value else {
        // A usage payload that is not an object has no fields to keep.
        return BTreeMap::new();
    };
    map.into_iter()
        .filter_map(|(key, field)| {
            if !field.is_object() {
                return Some((key, field));
            }
            let kept = prune(field);
            (!kept.is_empty()).then(|| (key, Value::Object(kept.into_iter().collect())))
        })
        .collect()
}

/// Model-side provenance of an assistant message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MessageOrigin {
    /// The model that produced the message.
    pub model: ModelRef,
    /// The generation this message came from.
    pub generation: GenerationId,
    /// Why the model stopped.
    pub finish_reason: FinishReason,
    /// What the request cost in tokens, as the provider reported it.
    pub usage: Usage,
    /// Whether the message is whole.
    pub status: MessageStatus,
    /// A content hash of the request that produced it, when one was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_digest: Option<Digest>,
}

impl MessageOrigin {
    /// The provenance of a message the given model produced, with no request
    /// digest recorded.
    ///
    /// The type is `#[non_exhaustive]`, so this is how a harness outside the
    /// core builds one: fields may be added later without breaking a caller
    /// that spells out only what it knows.
    #[must_use]
    pub fn new(
        model: ModelRef,
        generation: GenerationId,
        finish_reason: FinishReason,
        usage: Usage,
        status: MessageStatus,
    ) -> Self {
        Self {
            model,
            generation,
            finish_reason,
            usage,
            status,
            request_digest: None,
        }
    }

    /// The same origin with the digest of the request that produced it.
    #[must_use]
    pub fn with_digest(mut self, request_digest: Digest) -> Self {
        self.request_digest = Some(request_digest);
        self
    }
}

/// One turn of a conversation: a semantic role, an ordered list of parts and,
/// for assistant messages, the model-side origin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Who spoke.
    pub role: Role,
    /// The ordered content.
    #[serde(default)]
    pub parts: Vec<Part>,
    /// The model-side provenance, for assistant messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<MessageOrigin>,
    /// Whether the message participates in the cached prefix.
    #[serde(default, skip_serializing_if = "CacheClass::is_critical")]
    pub cache: CacheClass,
}

impl Message {
    /// A message with the given role and parts.
    #[must_use]
    pub fn new(role: Role, parts: Vec<Part>) -> Self {
        Self {
            role,
            parts,
            origin: None,
            cache: CacheClass::Critical,
        }
    }

    /// A user message carrying one text part.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, vec![Part::Text(TextPart::new(text))])
    }

    /// An assistant message carrying one text part.
    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::new(Role::Assistant, vec![Part::Text(TextPart::new(text))])
    }

    /// A system message carrying one text part.
    #[must_use]
    pub fn system(text: impl Into<String>) -> Self {
        Self::new(Role::System, vec![Part::Text(TextPart::new(text))])
    }

    /// A developer message carrying one text part.
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

    /// Attaches the model-side provenance.
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

    /// Whether the message is materialized after the cached prefix.
    #[must_use]
    pub fn is_ephemeral(&self) -> bool {
        self.cache.is_ephemeral()
    }

    /// Appends one part.
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

    /// Every reasoning part, in order.
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

    /// Every tool call, in order.
    #[must_use]
    pub fn tool_calls(&self) -> Vec<&ToolCallPart> {
        self.parts.iter().filter_map(Part::as_tool_call).collect()
    }

    /// Every tool result, in order.
    #[must_use]
    pub fn tool_results(&self) -> Vec<&ToolResultPart> {
        self.parts.iter().filter_map(Part::as_tool_result).collect()
    }

    /// Every media part, in order.
    #[must_use]
    pub fn media(&self) -> Vec<&MediaPart> {
        self.parts.iter().filter_map(Part::as_media).collect()
    }

    /// Whether the message asks for at least one tool call.
    #[must_use]
    pub fn has_tool_calls(&self) -> bool {
        self.parts.iter().any(|part| part.as_tool_call().is_some())
    }

    /// Why the model stopped, when the message has an origin.
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
    fn a_claimed_counter_leaves_the_rest_of_the_payload_alone() {
        // The payload is one Relace answered with: the neutral counters are
        // claimed, and the price and the flags — which the neutral vocabulary
        // does not name — survive instead of dying at the parser.
        let payload = serde_json::json!({
            "prompt_tokens": 86,
            "completion_tokens": 8,
            "total_tokens": 94,
            "cost": 0.000_004_339_6,
            "is_byok": false,
            "prompt_tokens_details": {
                "cached_tokens": 0,
                "cache_write_tokens": 0,
                "audio_tokens": 0,
                "video_tokens": 0
            },
            "cost_details": { "upstream_inference_cost": 0.000_004_339_6 },
            "completion_tokens_details": { "reasoning_tokens": 8, "image_tokens": 0 }
        });
        let mut vendor = VendorUsage::new(&payload);
        let usage = Usage {
            input_tokens: vendor.take(&["prompt_tokens"]),
            output_tokens: vendor.take(&["completion_tokens"]),
            reasoning_tokens: vendor.take(&["completion_tokens_details", "reasoning_tokens"]),
            cache_read_tokens: vendor.take(&["prompt_tokens_details", "cached_tokens"]),
            cache_write_tokens: vendor.take(&["prompt_tokens_details", "cache_write_tokens"]),
            audio_input_tokens: vendor.take(&["prompt_tokens_details", "audio_tokens"]),
            vendor: vendor.remainder(),
            ..Usage::default()
        };

        assert_eq!(usage.input_tokens, Some(86));
        assert_eq!(usage.output_tokens, Some(8));
        assert_eq!(usage.reasoning_tokens, Some(8));
        assert_eq!(usage.cache_read_tokens, Some(0));
        assert_eq!(usage.cache_write_tokens, Some(0));
        assert_eq!(usage.audio_input_tokens, Some(0));
        // Claimed fields are not duplicated, and a details object is pruned only
        // where every one of its fields was claimed.
        assert!(!usage.vendor.contains_key("prompt_tokens"));
        assert_eq!(
            usage.vendor.get("cost").and_then(Value::as_f64),
            Some(0.000_004_339_6)
        );
        assert_eq!(
            usage.vendor.get("is_byok").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            usage.vendor.get("total_tokens").and_then(Value::as_u64),
            Some(94)
        );
        assert!(usage.vendor.contains_key("cost_details"));
        // `prompt_tokens_details` keeps the `video_tokens` no field claims, and
        // the completion side keeps `image_tokens`, so both objects stay.
        assert_eq!(
            usage
                .vendor
                .get("prompt_tokens_details")
                .and_then(|details| details.get("video_tokens"))
                .and_then(Value::as_u64),
            Some(0)
        );
        assert!(usage.vendor.contains_key("completion_tokens_details"));
    }

    #[test]
    fn a_fully_claimed_payload_leaves_no_vendor_map() {
        let payload = serde_json::json!({
            "prompt_tokens": 3,
            "prompt_tokens_details": { "cached_tokens": 1 }
        });
        let mut vendor = VendorUsage::new(&payload);
        let usage = Usage {
            input_tokens: vendor.take(&["prompt_tokens"]),
            cache_read_tokens: vendor.take(&["prompt_tokens_details", "cached_tokens"]),
            vendor: vendor.remainder(),
            ..Usage::default()
        };
        assert!(usage.vendor.is_empty());
        assert_eq!(
            usage,
            Usage {
                input_tokens: Some(3),
                cache_read_tokens: Some(1),
                ..Usage::default()
            }
        );
    }

    #[test]
    fn a_counter_that_is_not_a_number_is_not_coerced_away() {
        let payload = serde_json::json!({ "prompt_tokens": "86" });
        let mut vendor = VendorUsage::new(&payload);
        let usage = Usage {
            input_tokens: vendor.take(&["prompt_tokens"]),
            vendor: vendor.remainder(),
            ..Usage::default()
        };
        assert_eq!(usage.input_tokens, None);
        assert_eq!(
            usage.vendor.get("prompt_tokens").and_then(Value::as_str),
            Some("86")
        );
    }

    #[test]
    fn usage_round_trips_with_its_vendor_payload() {
        let payload = serde_json::json!({ "cost": 0.000_004_339_6, "is_byok": true });
        let usage = Usage {
            vendor: VendorUsage::new(&payload).remainder(),
            ..Usage::default()
        };
        let json = serde_json::to_string(&usage).expect("serialize");
        let decoded: Usage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, usage);
        // And an empty map adds no bytes at all.
        let empty = serde_json::to_string(&Usage::default()).expect("serialize");
        assert_eq!(empty, "{}");
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
