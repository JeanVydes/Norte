use std::collections::BTreeMap;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::domain::continuity::ContinuityFragment;
use crate::domain::logprob::TokenLogprob;
use crate::domain::media::{MediaKind, MediaMetadata, MediaPart, MediaSource, MimeType};
use crate::domain::message::{FinishReason, Message, MessageOrigin, MessageStatus, Usage};
use crate::domain::moderation::SafetyRating;
use crate::domain::part::{
    Citation, Part, ProviderTool, ProviderToolCallPart, ProviderToolResultPart, ReasoningPart,
    ReasoningVisibility, RefusalPart, TextPart, ToolCallPart,
};
use crate::error::{Error, Result};
use crate::id::{CallId, GenerationId};
use crate::model::{ModelRequest, StreamLimits};

/// One event of the model stream. Text, reasoning, tool arguments and media
/// arrive as deltas; continuity fragments remain opaque to core.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
#[non_exhaustive]
pub enum StreamEvent {
    /// A text part started.
    TextStart {
        /// The index of the part in the message.
        index: u32,
    },
    /// Text arrived.
    TextDelta {
        /// The index of the part in the message.
        index: u32,
        /// The text that arrived.
        text: String,
    },
    /// A reasoning part started.
    ReasoningStart {
        /// The index of the part in the message.
        index: u32,
        /// How much of the reasoning the provider will make available.
        visibility: ReasoningVisibility,
    },
    /// Reasoning arrived.
    ReasoningDelta {
        /// The index of the part in the message.
        index: u32,
        /// The reasoning that arrived.
        text: String,
    },
    /// A partial refusal. The vendor streams it the way it streams text, so it
    /// accumulates into one [`Part::Refusal`] at the part index.
    RefusalDelta {
        /// The index of the part in the message.
        index: u32,
        /// The refusal that arrived.
        text: String,
    },
    /// A tool call started.
    ToolCallStart {
        /// The index of the part in the message.
        index: u32,
        /// The call.
        call_id: CallId,
        /// The tool's name.
        name: String,
    },
    /// A fragment of a tool call's arguments arrived.
    ToolCallDelta {
        /// The index of the part in the message.
        index: u32,
        /// The call the arguments belong to.
        call_id: CallId,
        /// The tool's name, on the deltas that carry it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// The fragment of the arguments document.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        arguments: String,
    },
    /// The tool call at `index` has received its complete arguments.
    ///
    /// Adapters emit it at the wire's tool-block boundary so a host that enables
    /// early tool execution can start the call before the message finishes
    /// (FR-STREAM-4).
    ToolCallDone {
        /// The index of the part in the message.
        index: u32,
    },
    /// A media part started.
    MediaStart {
        /// The index of the part in the message.
        index: u32,
        /// The kind of media.
        kind: MediaKind,
        /// The media's MIME type.
        mime: MimeType,
        /// The descriptive fields the part is created from.
        #[serde(default)]
        metadata: MediaMetadata,
    },
    /// Media bytes arrived.
    MediaChunk {
        /// The index of the part in the message.
        index: u32,
        /// The bytes that arrived in this chunk.
        #[serde(with = "crate::codec::bytes_base64")]
        bytes: Vec<u8>,
        /// Whether the chunk ends the part.
        last: bool,
    },
    /// Replaces the metadata of the in-flight media part at `index`.
    ///
    /// A vendor that streams media also streams the descriptive fields the
    /// container is created from (an audio transcript, a vendor media id) after
    /// the bytes have started, so the adapter settles them here before closing
    /// the part.
    MediaMetadata {
        /// The index of the part in the message.
        index: u32,
        /// The metadata that replaces what the part carried.
        metadata: MediaMetadata,
    },
    /// A tool the provider executed itself was called.
    ProviderToolCall {
        /// The index of the part in the message.
        index: u32,
        /// The call.
        call_id: CallId,
        /// The tool that ran.
        tool: ProviderTool,
        /// The input the vendor sent, kept verbatim.
        input: serde_json::Value,
    },
    /// A tool the provider executed itself returned.
    ProviderToolResult {
        /// The index of the part in the message.
        index: u32,
        /// The call this is the result of.
        call_id: CallId,
        /// The tool that ran.
        tool: ProviderTool,
        /// The output the vendor produced, kept verbatim.
        output: serde_json::Value,
        /// References the provider attached to the result.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<Citation>,
    },
    /// A citation the vendor attached to the text part at `index`.
    Citation {
        /// The index of the part in the message.
        index: u32,
        /// The citation.
        citation: Citation,
    },
    /// A part the adapter could not express as deltas.
    Part {
        /// The index of the part in the message.
        index: u32,
        /// The part.
        part: Part,
    },
    /// Continuity material for the part at `index`.
    Continuity {
        /// The index of the part in the message.
        index: u32,
        /// The fragment, opaque to core.
        fragment: ContinuityFragment,
    },
    /// The vendor's moderation labels for a generated candidate. Informational:
    /// the neutral model never gates on them (see [`SafetyRating`]).
    SafetyRatings {
        /// The ratings, as the provider reported them.
        ratings: Vec<SafetyRating>,
    },
    /// The log probabilities of the tokens streamed for the part at `index`.
    /// Informational: the accumulator drops the event, so the committed parts
    /// and the reconstructed prefix are the same whether or not the turn asked
    /// for them; only the request digest records that it did (FR-LP-2). See
    /// [`TokenLogprob`].
    Logprobs {
        /// The index of the part in the message.
        index: u32,
        /// One entry per token, in the order the provider reported them.
        entries: Vec<TokenLogprob>,
    },
    /// The usage the provider reported.
    Usage(Usage),
    /// Why the provider stopped generating.
    Finish(FinishReason),
}

#[derive(Clone, Debug)]
enum Partial {
    Text {
        text: String,
        citations: Vec<Citation>,
    },
    Reasoning {
        visibility: ReasoningVisibility,
        text: String,
    },
    Refusal {
        text: String,
    },
    ToolCall {
        call_id: CallId,
        name: String,
        arguments: String,
    },
    Media {
        kind: MediaKind,
        mime: MimeType,
        metadata: MediaMetadata,
        bytes: Vec<u8>,
    },
}

/// The result of folding a whole stream: the parts, the finish reason and the
/// usage the provider reported.
#[derive(Clone, Debug, Default)]
pub struct Accumulated {
    /// The parts, in index order.
    pub parts: Vec<Part>,
    /// Why the provider stopped generating.
    pub finish_reason: Option<FinishReason>,
    /// The usage the provider reported.
    pub usage: Usage,
}

/// Reassembles a stream into message parts under a fixed memory budget.
///
/// Partial content never reaches the store: the engine forwards completed
/// parts as they close and keeps only the in-flight buffer here.
#[derive(Clone, Debug)]
pub struct StreamAccumulator {
    limits: StreamLimits,
    partials: BTreeMap<u32, Partial>,
    complete: BTreeMap<u32, Part>,
    continuity: BTreeMap<u32, crate::domain::continuity::ContinuityToken>,
    finish_reason: Option<FinishReason>,
    usage: Usage,
}

impl StreamAccumulator {
    /// An accumulator bounded by the given stream limits.
    #[must_use]
    pub fn new(limits: StreamLimits) -> Self {
        Self {
            limits,
            partials: BTreeMap::new(),
            complete: BTreeMap::new(),
            continuity: BTreeMap::new(),
            finish_reason: None,
            usage: Usage::default(),
        }
    }

    /// Applies one event, returning the part it completed, if any.
    ///
    /// # Errors
    ///
    /// Returns a codec error for an event that does not suit the part it targets,
    /// restarts an open part or overwrites a closed part, and
    /// [`Error::LimitExceeded`] once a partial part exceeds its byte budget.
    pub fn apply(&mut self, event: StreamEvent) -> Result<Option<Part>> {
        self.check_index(&event)?;
        match event {
            StreamEvent::TextStart { index } => self.begin(
                index,
                Partial::Text {
                    text: String::new(),
                    citations: Vec::new(),
                },
            ),
            StreamEvent::TextDelta { index, text } => self.grow_text(index, &text),
            StreamEvent::ReasoningStart { index, visibility } => self.begin(
                index,
                Partial::Reasoning {
                    visibility,
                    text: String::new(),
                },
            ),
            StreamEvent::ReasoningDelta { index, text } => self.grow_reasoning(index, &text),
            StreamEvent::RefusalDelta { index, text } => self.grow_refusal(index, &text),
            StreamEvent::ToolCallStart {
                index,
                call_id,
                name,
            } => self.begin(
                index,
                Partial::ToolCall {
                    call_id,
                    name,
                    arguments: String::new(),
                },
            ),
            StreamEvent::ToolCallDelta {
                index,
                call_id,
                name,
                arguments,
            } => self.grow_tool_call(index, call_id, name, &arguments),
            StreamEvent::ToolCallDone { index } => self.close_tool_call(index),
            StreamEvent::MediaStart {
                index,
                kind,
                mime,
                metadata,
            } => self.begin(
                index,
                Partial::Media {
                    kind,
                    mime,
                    metadata,
                    bytes: Vec::new(),
                },
            ),
            StreamEvent::MediaChunk { index, bytes, last } => self.grow_media(index, &bytes, last),
            StreamEvent::MediaMetadata { index, metadata } => {
                self.set_media_metadata(index, metadata)
            }
            StreamEvent::ProviderToolCall {
                index,
                call_id,
                tool,
                input,
            } => Ok(Some(self.complete(
                index,
                Part::ProviderToolCall(ProviderToolCallPart {
                    call_id,
                    tool,
                    input,
                    continuity: None,
                }),
            ))),
            StreamEvent::ProviderToolResult {
                index,
                call_id,
                tool,
                output,
                citations,
            } => Ok(Some(self.complete(
                index,
                Part::ProviderToolResult(ProviderToolResultPart {
                    call_id,
                    tool,
                    output,
                    citations,
                    continuity: None,
                }),
            ))),
            StreamEvent::Citation { index, citation } => self.add_citation(index, citation),
            StreamEvent::Part { index, part } => Ok(Some(self.complete(index, part))),
            StreamEvent::Continuity { .. }
            | StreamEvent::SafetyRatings { .. }
            | StreamEvent::Logprobs { .. } => Ok(None),
            StreamEvent::Usage(usage) => Ok(self.set_usage(usage)),
            StreamEvent::Finish(reason) => {
                self.finish_reason = Some(reason);
                Ok(None)
            }
        }
    }

    fn check_index(&self, event: &StreamEvent) -> Result<()> {
        match event {
            StreamEvent::TextStart { index }
            | StreamEvent::TextDelta { index, .. }
            | StreamEvent::ReasoningStart { index, .. }
            | StreamEvent::ReasoningDelta { index, .. }
            | StreamEvent::RefusalDelta { index, .. }
            | StreamEvent::ToolCallStart { index, .. }
            | StreamEvent::ToolCallDelta { index, .. }
            | StreamEvent::MediaStart { index, .. }
            | StreamEvent::MediaChunk { index, .. }
            | StreamEvent::MediaMetadata { index, .. }
            | StreamEvent::ProviderToolCall { index, .. }
            | StreamEvent::ProviderToolResult { index, .. }
            | StreamEvent::Citation { index, .. }
            | StreamEvent::Part { index, .. }
                if self.complete.contains_key(index) =>
            {
                Err(Error::Codec(format!(
                    "event for a closed part at index {index}"
                )))
            }
            _ => Ok(()),
        }
    }

    fn begin(&mut self, index: u32, partial: Partial) -> Result<Option<Part>> {
        if self.partials.contains_key(&index) {
            return Err(Error::Codec(format!(
                "part already started at index {index}"
            )));
        }
        self.partials.insert(index, partial);
        Ok(None)
    }

    fn add_citation(&mut self, index: u32, citation: Citation) -> Result<Option<Part>> {
        match self.partials.entry(index).or_insert_with(|| Partial::Text {
            text: String::new(),
            citations: Vec::new(),
        }) {
            Partial::Text { citations, .. } => {
                citations.push(citation);
                Ok(None)
            }
            other => Err(Error::Codec(format!(
                "citation for a non-text part: {other:?}"
            ))),
        }
    }

    fn grow_text(&mut self, index: u32, text: &str) -> Result<Option<Part>> {
        let grown = match self.partials.entry(index).or_insert_with(|| Partial::Text {
            text: String::new(),
            citations: Vec::new(),
        }) {
            Partial::Text { text: held, .. } => {
                held.push_str(text);
                held.len()
            }
            other => {
                return Err(Error::Codec(format!(
                    "text delta for a non-text part: {other:?}"
                )));
            }
        };
        self.check(grown)?;
        Ok(None)
    }

    fn grow_reasoning(&mut self, index: u32, text: &str) -> Result<Option<Part>> {
        let grown = match self
            .partials
            .entry(index)
            .or_insert_with(|| Partial::Reasoning {
                visibility: ReasoningVisibility::Full,
                text: String::new(),
            }) {
            Partial::Reasoning { text: held, .. } => {
                held.push_str(text);
                held.len()
            }
            other => {
                return Err(Error::Codec(format!(
                    "reasoning delta for a non-reasoning part: {other:?}"
                )));
            }
        };
        self.check(grown)?;
        Ok(None)
    }

    fn grow_refusal(&mut self, index: u32, text: &str) -> Result<Option<Part>> {
        let grown = match self
            .partials
            .entry(index)
            .or_insert_with(|| Partial::Refusal {
                text: String::new(),
            }) {
            Partial::Refusal { text: held } => {
                held.push_str(text);
                held.len()
            }
            other => {
                return Err(Error::Codec(format!(
                    "refusal delta for a non-refusal part: {other:?}"
                )));
            }
        };
        self.check(grown)?;
        Ok(None)
    }

    fn grow_tool_call(
        &mut self,
        index: u32,
        call_id: CallId,
        name: Option<String>,
        arguments: &str,
    ) -> Result<Option<Part>> {
        let grown = match self
            .partials
            .entry(index)
            .or_insert_with(|| Partial::ToolCall {
                call_id,
                name: String::new(),
                arguments: String::new(),
            }) {
            Partial::ToolCall {
                call_id: held_id,
                name: held_name,
                arguments: held_arguments,
            } => {
                if *held_id != call_id {
                    return Err(Error::Codec(format!(
                        "tool call id changed mid-stream at index {index}"
                    )));
                }
                if let Some(name) = name {
                    held_name.push_str(&name);
                }
                held_arguments.push_str(arguments);
                held_arguments.len()
            }
            other => {
                return Err(Error::Codec(format!(
                    "tool call delta for a non-tool part: {other:?}"
                )));
            }
        };
        self.check(grown)?;
        Ok(None)
    }

    /// Closes the tool call at `index`, parsing its accumulated arguments, and
    /// returns the completed part (FR-STREAM-4).
    ///
    /// # Errors
    ///
    /// Returns a codec error when the index holds a non-tool part or the
    /// accumulated arguments are not valid JSON.
    fn close_tool_call(&mut self, index: u32) -> Result<Option<Part>> {
        let Some(partial) = self.partials.remove(&index) else {
            return Ok(None);
        };
        let Partial::ToolCall {
            call_id,
            name,
            arguments,
        } = partial
        else {
            self.partials.insert(index, partial);
            return Err(Error::Codec(format!(
                "tool call done for a non-tool part at index {index}"
            )));
        };
        let part = Part::ToolCall(ToolCallPart {
            call_id,
            name,
            arguments: Self::parse_tool_arguments(&arguments)?,
            continuity: None,
        });
        Ok(Some(self.complete(index, part)))
    }

    /// Replaces the metadata of the media partial at `index` (FR-STREAM-1).
    ///
    /// # Errors
    ///
    /// Returns a codec error when no media partial is open at `index`.
    fn set_media_metadata(&mut self, index: u32, metadata: MediaMetadata) -> Result<Option<Part>> {
        match self.partials.get_mut(&index) {
            Some(Partial::Media { metadata: held, .. }) => {
                *held = metadata;
                Ok(None)
            }
            Some(other) => Err(Error::Codec(format!(
                "media metadata for a non-media part: {other:?}"
            ))),
            None => Err(Error::Codec(format!(
                "media metadata for a closed part at index {index}"
            ))),
        }
    }

    fn grow_media(&mut self, index: u32, bytes: &[u8], last: bool) -> Result<Option<Part>> {
        match self.partials.entry(index).or_insert(Partial::Media {
            kind: MediaKind::Document,
            mime: MimeType::new("application/octet-stream")?,
            metadata: MediaMetadata::default(),
            bytes: Vec::new(),
        }) {
            Partial::Media {
                kind,
                mime,
                metadata,
                bytes: held,
            } => {
                held.extend_from_slice(bytes);
                if held.len() > self.limits.max_part_bytes {
                    return Err(Error::LimitExceeded {
                        what: "streamed media".to_owned(),
                        limit: self.limits.max_part_bytes,
                    });
                }
                if last {
                    let part = Part::Media(MediaPart {
                        kind: *kind,
                        source: MediaSource::Inline(std::mem::take(held)),
                        mime: mime.clone(),
                        metadata: metadata.clone(),
                    });
                    return Ok(Some(self.complete(index, part)));
                }
                Ok(None)
            }
            other => Err(Error::Codec(format!(
                "media chunk for a non-media part: {other:?}"
            ))),
        }
    }

    /// Attaches the continuity token for the part at `index`, including a part
    /// that has already completed.
    pub fn attach_continuity(
        &mut self,
        index: u32,
        token: crate::domain::continuity::ContinuityToken,
    ) {
        if let Some(slot) = self
            .complete
            .get_mut(&index)
            .and_then(Self::continuity_slot)
        {
            *slot = Some(token);
        } else {
            self.continuity.insert(index, token);
        }
    }

    /// Closes every open part and returns the assembled content in index order.
    ///
    /// Does not require a [`StreamEvent::Finish`], so a host can recover partial
    /// content after cancellation or a stream error.
    ///
    /// # Errors
    ///
    /// Returns a codec error when accumulated tool arguments are not valid JSON.
    pub fn finish(mut self) -> Result<Accumulated> {
        let mut parts: BTreeMap<u32, Part> = std::mem::take(&mut self.complete);
        let partials = std::mem::take(&mut self.partials);
        for (index, partial) in partials {
            let part = match partial {
                Partial::Text { text, citations } => Part::Text(TextPart {
                    text,
                    citations,
                    continuity: self.continuity.remove(&index),
                }),
                Partial::Reasoning { visibility, text } => Part::Reasoning(ReasoningPart {
                    visibility,
                    text,
                    continuity: self.continuity.remove(&index),
                }),
                Partial::Refusal { text } => Part::Refusal(RefusalPart { text }),
                Partial::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => Part::ToolCall(ToolCallPart {
                    call_id,
                    name,
                    arguments: Self::parse_tool_arguments(&arguments)?,
                    continuity: self.continuity.remove(&index),
                }),
                Partial::Media {
                    kind,
                    mime,
                    metadata,
                    bytes,
                } => Part::Media(MediaPart {
                    kind,
                    source: MediaSource::Inline(bytes),
                    mime,
                    metadata,
                }),
            };
            parts.insert(index, part);
        }
        Ok(Accumulated {
            parts: parts.into_values().collect(),
            finish_reason: self.finish_reason,
            usage: self.usage,
        })
    }

    fn set_usage(&mut self, usage: Usage) -> Option<Part> {
        self.usage = usage;
        None
    }

    fn parse_tool_arguments(arguments: &str) -> Result<serde_json::Value> {
        if arguments.trim().is_empty() {
            Ok(serde_json::json!({}))
        } else {
            serde_json::from_str(arguments)
                .map_err(|error| Error::Codec(format!("tool arguments: {error}")))
        }
    }

    fn continuity_slot(
        part: &mut Part,
    ) -> Option<&mut Option<crate::domain::continuity::ContinuityToken>> {
        match part {
            Part::Text(part) => Some(&mut part.continuity),
            Part::Reasoning(part) => Some(&mut part.continuity),
            Part::ToolCall(part) => Some(&mut part.continuity),
            Part::ProviderToolCall(part) => Some(&mut part.continuity),
            Part::ProviderToolResult(part) => Some(&mut part.continuity),
            Part::Media(_) | Part::ToolResult(_) | Part::Refusal(_) | Part::Extension(_) => None,
        }
    }

    fn complete(&mut self, index: u32, mut part: Part) -> Part {
        if let Some(token) = self.continuity.remove(&index) {
            if let Some(slot) = Self::continuity_slot(&mut part) {
                *slot = Some(token);
            }
        }
        self.partials.remove(&index);
        self.complete.insert(index, part.clone());
        part
    }

    fn check(&self, bytes: usize) -> Result<()> {
        if bytes > self.limits.max_part_bytes {
            return Err(Error::LimitExceeded {
                what: "streamed part".to_owned(),
                limit: self.limits.max_part_bytes,
            });
        }
        Ok(())
    }
}

/// The blocking form of the only primitive: collect a model stream into one
/// assistant message.
///
/// # Errors
///
/// Propagates model stream errors and any codec error from the accumulator.
/// Returns a codec error if the stream ends without a [`StreamEvent::Finish`].
pub async fn collect(model: &dyn crate::model::Model, request: ModelRequest) -> Result<Message> {
    let limits = request.limits;
    let mut stream = model.stream(request).await?;
    let mut accumulator = StreamAccumulator::new(limits);
    let mut continuity: BTreeMap<u32, crate::domain::continuity::ContinuityToken> = BTreeMap::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        if let StreamEvent::Continuity { index, fragment } = &event {
            let merged = model
                .continuity()
                .merge(continuity.get(index).cloned(), fragment.clone())?;
            continuity.insert(*index, merged.clone());
            accumulator.attach_continuity(*index, merged);
            continue;
        }
        accumulator.apply(event)?;
    }
    let accumulated = accumulator.finish()?;
    let finish_reason = accumulated
        .finish_reason
        .ok_or_else(|| Error::Codec("model stream ended without a Finish event".to_owned()))?;
    let status = match finish_reason {
        FinishReason::Length => MessageStatus::Truncated,
        FinishReason::Cancelled => MessageStatus::Interrupted,
        FinishReason::Stop
        | FinishReason::ToolCalls
        | FinishReason::ContentFilter
        | FinishReason::Refusal
        | FinishReason::Other(_) => MessageStatus::Complete,
    };
    Ok(
        Message::new(crate::domain::role::Role::Assistant, accumulated.parts).with_origin(
            MessageOrigin {
                model: model.reference().clone(),
                generation: GenerationId::new(),
                finish_reason,
                usage: accumulated.usage,
                status,
                request_digest: None,
            },
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::logprob::LogprobValue;
    use crate::model::StreamLimits;

    fn accumulator() -> StreamAccumulator {
        StreamAccumulator::new(StreamLimits::default())
    }

    #[test]
    fn text_deltas_assemble_into_one_part() {
        let mut accumulator = accumulator();
        accumulator
            .apply(StreamEvent::TextStart { index: 0 })
            .expect("start");
        accumulator
            .apply(StreamEvent::TextDelta {
                index: 0,
                text: "hel".to_owned(),
            })
            .expect("delta");
        accumulator
            .apply(StreamEvent::TextDelta {
                index: 0,
                text: "lo".to_owned(),
            })
            .expect("delta");
        let result = accumulator.finish().expect("finish");
        assert_eq!(result.parts.len(), 1);
        assert_eq!(result.parts[0], Part::Text(TextPart::new("hello")));
    }

    #[test]
    fn refusal_deltas_assemble_into_one_part() {
        let mut accumulator = accumulator();
        accumulator
            .apply(StreamEvent::RefusalDelta {
                index: 2,
                text: "I can".to_owned(),
            })
            .expect("delta");
        accumulator
            .apply(StreamEvent::RefusalDelta {
                index: 2,
                text: "not help".to_owned(),
            })
            .expect("delta");
        let result = accumulator.finish().expect("finish");
        assert_eq!(
            result.parts,
            vec![Part::Refusal(RefusalPart {
                text: "I cannot help".to_owned(),
            })]
        );
    }

    #[test]
    fn citations_attach_to_the_text_part() {
        let mut accumulator = accumulator();
        accumulator
            .apply(StreamEvent::TextDelta {
                index: 0,
                text: "rust".to_owned(),
            })
            .expect("delta");
        accumulator
            .apply(StreamEvent::Citation {
                index: 0,
                citation: Citation {
                    source: crate::domain::part::CitationSource::Url(
                        "https://example.test".to_owned(),
                    ),
                    title: Some("Example".to_owned()),
                },
            })
            .expect("citation");
        let result = accumulator.finish().expect("finish");
        let Part::Text(text) = &result.parts[0] else {
            panic!("expected a text part");
        };
        assert_eq!(text.text, "rust");
        assert_eq!(text.citations.len(), 1);
        assert_eq!(text.citations[0].title.as_deref(), Some("Example"));
    }

    #[test]
    fn tool_arguments_assemble_across_deltas() {
        let call_id = CallId::new();
        let mut accumulator = accumulator();
        accumulator
            .apply(StreamEvent::ToolCallStart {
                index: 0,
                call_id,
                name: "read_file".to_owned(),
            })
            .expect("start");
        for chunk in ["{\"pa", "th\":", "\"/tmp/a\"}"] {
            accumulator
                .apply(StreamEvent::ToolCallDelta {
                    index: 0,
                    call_id,
                    name: None,
                    arguments: chunk.to_owned(),
                })
                .expect("delta");
        }
        let result = accumulator.finish().expect("finish");
        let call = result.parts[0].as_tool_call().expect("tool call");
        assert_eq!(call.name, "read_file");
        assert_eq!(call.arguments, serde_json::json!({ "path": "/tmp/a" }));
    }

    #[test]
    fn empty_tool_arguments_become_an_empty_object() {
        let call_id = CallId::new();
        let mut accumulator = accumulator();
        accumulator
            .apply(StreamEvent::ToolCallStart {
                index: 0,
                call_id,
                name: "list".to_owned(),
            })
            .expect("start");
        let result = accumulator.finish().expect("finish");
        assert_eq!(
            result.parts[0].as_tool_call().expect("call").arguments,
            serde_json::json!({})
        );
    }

    #[test]
    fn parts_are_returned_in_index_order() {
        let mut accumulator = accumulator();
        accumulator
            .apply(StreamEvent::TextDelta {
                index: 2,
                text: "second".to_owned(),
            })
            .expect("delta");
        accumulator
            .apply(StreamEvent::TextDelta {
                index: 1,
                text: "first".to_owned(),
            })
            .expect("delta");
        let result = accumulator.finish().expect("finish");
        assert_eq!(
            result.parts,
            vec![Part::text("first"), Part::text("second")]
        );
    }

    #[test]
    fn media_deltas_assemble_into_one_typed_part() {
        let mut accumulator = accumulator();
        accumulator
            .apply(StreamEvent::MediaStart {
                index: 0,
                kind: MediaKind::Image,
                mime: MimeType::new("image/png").expect("mime"),
                metadata: MediaMetadata {
                    width: Some(1024),
                    height: Some(768),
                    ..MediaMetadata::default()
                },
            })
            .expect("start");
        accumulator
            .apply(StreamEvent::MediaChunk {
                index: 0,
                bytes: vec![1, 2],
                last: false,
            })
            .expect("chunk");
        let completed = accumulator
            .apply(StreamEvent::MediaChunk {
                index: 0,
                bytes: vec![3, 4],
                last: true,
            })
            .expect("chunk")
            .expect("completed part");
        let Part::Media(media) = completed else {
            panic!("expected a media part");
        };
        assert_eq!(media.kind, MediaKind::Image);
        assert_eq!(media.mime.as_str(), "image/png");
        assert_eq!(media.metadata.width, Some(1024));
        assert_eq!(media.metadata.height, Some(768));
        assert_eq!(media.source, MediaSource::Inline(vec![1, 2, 3, 4]));
        let result = accumulator.finish().expect("finish");
        assert_eq!(result.parts, vec![Part::Media(media)]);
        assert_eq!(result.finish_reason, None);
    }

    #[test]
    fn media_metadata_settles_after_the_bytes_start() {
        let mut accumulator = accumulator();
        accumulator
            .apply(StreamEvent::MediaStart {
                index: 0,
                kind: MediaKind::Audio,
                mime: MimeType::new("audio/pcm16").expect("mime"),
                metadata: MediaMetadata::default(),
            })
            .expect("start");
        accumulator
            .apply(StreamEvent::MediaChunk {
                index: 0,
                bytes: vec![1, 2],
                last: false,
            })
            .expect("chunk");
        accumulator
            .apply(StreamEvent::MediaMetadata {
                index: 0,
                metadata: MediaMetadata {
                    transcript: Some("hi".to_owned()),
                    ..MediaMetadata::default()
                },
            })
            .expect("metadata");
        let completed = accumulator
            .apply(StreamEvent::MediaChunk {
                index: 0,
                bytes: Vec::new(),
                last: true,
            })
            .expect("chunk")
            .expect("completed part");
        let Part::Media(media) = completed else {
            panic!("expected a media part");
        };
        assert_eq!(media.metadata.transcript.as_deref(), Some("hi"));
        assert_eq!(media.source, MediaSource::Inline(vec![1, 2]));
    }

    #[test]
    fn the_part_budget_is_enforced() {
        let mut accumulator = StreamAccumulator::new(StreamLimits {
            max_part_bytes: 3,
            max_buffered_events: 1,
        });
        accumulator
            .apply(StreamEvent::TextDelta {
                index: 0,
                text: "abcd".to_owned(),
            })
            .expect_err("over budget");
    }

    struct FixtureModel {
        reference: crate::id::ModelRef,
        capabilities: crate::capability::Capabilities,
        events: Vec<StreamEvent>,
    }

    #[async_trait::async_trait]
    impl crate::model::Model for FixtureModel {
        fn reference(&self) -> &crate::id::ModelRef {
            &self.reference
        }

        fn capabilities(&self) -> &crate::capability::Capabilities {
            &self.capabilities
        }

        fn continuity(&self) -> &dyn crate::model::ContinuityAdapter {
            panic!("fixture emits no continuity fragments")
        }

        async fn stream(&self, _request: ModelRequest) -> Result<crate::model::ModelStream> {
            Ok(Box::pin(futures_util::stream::iter(
                self.events.clone().into_iter().map(Ok),
            )))
        }
    }

    #[tokio::test]
    async fn collect_requires_finish_even_for_an_empty_stream() {
        let mut model = FixtureModel {
            reference: "test/model".parse().expect("model"),
            capabilities: crate::capability::Capabilities::default(),
            events: Vec::new(),
        };
        for events in [
            Vec::new(),
            vec![StreamEvent::TextDelta {
                index: 0,
                text: "partial".to_owned(),
            }],
            vec![StreamEvent::Part {
                index: 0,
                part: Part::text("complete part, incomplete stream"),
            }],
        ] {
            model.events = events;
            let error = collect(
                &model,
                ModelRequest::new(model.reference.clone(), Vec::new()),
            )
            .await
            .expect_err("EOF without Finish");
            assert!(matches!(error, Error::Codec(message) if message.contains("Finish")));
        }
        for (reason, status) in [
            (FinishReason::Stop, MessageStatus::Complete),
            (FinishReason::ToolCalls, MessageStatus::Complete),
            (FinishReason::Length, MessageStatus::Truncated),
            (FinishReason::Cancelled, MessageStatus::Interrupted),
        ] {
            model.events = vec![
                StreamEvent::Finish(reason.clone()),
                StreamEvent::Usage(Usage {
                    output_tokens: Some(3),
                    ..Usage::default()
                }),
            ];
            let message = collect(
                &model,
                ModelRequest::new(model.reference.clone(), Vec::new()),
            )
            .await
            .expect("explicit finish permits an empty response");
            let origin = message.origin.expect("origin");
            assert_eq!(origin.finish_reason, reason);
            assert_eq!(origin.status, status);
            assert_eq!(origin.usage.output_tokens, Some(3));
        }
    }

    #[test]
    fn continuity_survives_before_and_after_part_completion() {
        use crate::domain::continuity::{ContinuityScope, ContinuityToken};

        let token = ContinuityToken::new(
            "test.wire".parse().expect("wire family"),
            ContinuityScope::Any,
            1,
            vec![1, 2],
        );
        for attach_before in [true, false] {
            let mut accumulator = accumulator();
            if attach_before {
                accumulator.attach_continuity(0, token.clone());
            }
            let completed = accumulator
                .apply(StreamEvent::Part {
                    index: 0,
                    part: Part::text("complete"),
                })
                .expect("part")
                .expect("completed part");
            if attach_before {
                assert_eq!(completed.continuity(), Some(&token));
            } else {
                accumulator.attach_continuity(0, token.clone());
            }
            assert_eq!(
                accumulator.finish().expect("finish").parts[0].continuity(),
                Some(&token)
            );
        }

        let mut accumulator = accumulator();
        accumulator
            .apply(StreamEvent::ToolCallStart {
                index: 0,
                call_id: CallId::new(),
                name: "list".to_owned(),
            })
            .expect("start");
        accumulator
            .apply(StreamEvent::ToolCallDone { index: 0 })
            .expect("done");
        accumulator.attach_continuity(0, token.clone());
        assert_eq!(
            accumulator.finish().expect("finish").parts[0].continuity(),
            Some(&token)
        );
    }

    #[test]
    fn restarting_an_index_does_not_discard_partial_content() {
        let mut accumulator = accumulator();
        accumulator
            .apply(StreamEvent::TextDelta {
                index: 0,
                text: "partial".to_owned(),
            })
            .expect("delta");
        assert!(matches!(
            accumulator.apply(StreamEvent::TextStart { index: 0 }),
            Err(Error::Codec(_))
        ));
        assert_eq!(
            accumulator.finish().expect("recover partial").parts,
            vec![Part::text("partial")]
        );
    }

    #[test]
    fn closed_indices_cannot_be_reopened_by_deltas() {
        let mut accumulator = accumulator();
        accumulator
            .apply(StreamEvent::TextDelta {
                index: 0,
                text: "partial".to_owned(),
            })
            .expect("delta");
        accumulator
            .apply(StreamEvent::Part {
                index: 0,
                part: Part::text("complete"),
            })
            .expect("part");
        for event in [
            StreamEvent::TextStart { index: 0 },
            StreamEvent::TextDelta {
                index: 0,
                text: "replacement".to_owned(),
            },
        ] {
            assert!(matches!(accumulator.apply(event), Err(Error::Codec(_))));
        }
        assert_eq!(
            accumulator.finish().expect("finish").parts,
            vec![Part::text("complete")]
        );
    }

    #[test]
    fn tool_argument_parsing_agrees_at_done_and_finish() {
        for arguments in ["", " \n\t", "{\"path\":\"/tmp/a\"}", "{"] {
            let mut accumulator = accumulator();
            accumulator
                .apply(StreamEvent::ToolCallDelta {
                    index: 0,
                    call_id: CallId::new(),
                    name: Some("read_file".to_owned()),
                    arguments: arguments.to_owned(),
                })
                .expect("delta");
            let finished = accumulator.clone().finish();
            let closed = accumulator.apply(StreamEvent::ToolCallDone { index: 0 });
            match (finished, closed) {
                (Ok(finished), Ok(Some(closed))) => {
                    assert_eq!(finished.parts, vec![closed]);
                    assert_eq!(accumulator.finish().expect("finish").parts, finished.parts);
                }
                (Err(Error::Codec(finished)), Err(Error::Codec(closed))) => {
                    assert_eq!(finished, closed);
                }
                other => panic!("parsing disagreed: {other:?}"),
            }
        }
    }

    #[test]
    fn logprobs_do_not_reach_the_accumulator() {
        let mut accumulator = accumulator();
        accumulator
            .apply(StreamEvent::TextStart { index: 0 })
            .expect("start");
        accumulator
            .apply(StreamEvent::Logprobs {
                index: 0,
                entries: vec![TokenLogprob {
                    token: "rust".to_owned(),
                    logprob: LogprobValue::new(-0.25),
                    bytes: None,
                    alternatives: Vec::new(),
                    extension: BTreeMap::new(),
                }],
            })
            .expect("logprobs");
        accumulator
            .apply(StreamEvent::TextDelta {
                index: 0,
                text: "rust".to_owned(),
            })
            .expect("delta");
        let result = accumulator.finish().expect("finish");
        assert_eq!(
            result.parts,
            vec![Part::text("rust")],
            "probabilities are informational and leave no part behind"
        );
    }
}
