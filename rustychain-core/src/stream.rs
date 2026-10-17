use std::collections::BTreeMap;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::domain::continuity::ContinuityFragment;
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
    TextStart {
        index: u32,
    },
    TextDelta {
        index: u32,
        text: String,
    },
    ReasoningStart {
        index: u32,
        visibility: ReasoningVisibility,
    },
    ReasoningDelta {
        index: u32,
        text: String,
    },
    /// A partial refusal. The vendor streams it the way it streams text, so it
    /// accumulates into one [`Part::Refusal`] at the part index.
    RefusalDelta {
        index: u32,
        text: String,
    },
    ToolCallStart {
        index: u32,
        call_id: CallId,
        name: String,
    },
    ToolCallDelta {
        index: u32,
        call_id: CallId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        arguments: String,
    },
    /// The tool call at `index` has received its complete arguments.
    ///
    /// Adapters emit it at the wire's tool-block boundary so a host that enables
    /// early tool execution can start the call before the message finishes
    /// (FR-STREAM-4).
    ToolCallDone {
        index: u32,
    },
    MediaStart {
        index: u32,
        kind: MediaKind,
        mime: MimeType,
        #[serde(default)]
        metadata: MediaMetadata,
    },
    MediaChunk {
        index: u32,
        #[serde(with = "crate::codec::bytes_base64")]
        bytes: Vec<u8>,
        last: bool,
    },
    /// Replaces the metadata of the in-flight media part at `index`.
    ///
    /// A vendor that streams media also streams the descriptive fields the
    /// container is created from (an audio transcript, a vendor media id) after
    /// the bytes have started, so the adapter settles them here before closing
    /// the part.
    MediaMetadata {
        index: u32,
        metadata: MediaMetadata,
    },
    ProviderToolCall {
        index: u32,
        call_id: CallId,
        tool: ProviderTool,
        input: serde_json::Value,
    },
    ProviderToolResult {
        index: u32,
        call_id: CallId,
        tool: ProviderTool,
        output: serde_json::Value,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<Citation>,
    },
    /// A citation the vendor attached to the text part at `index`.
    Citation {
        index: u32,
        citation: Citation,
    },
    /// A part the adapter could not express as deltas.
    Part {
        index: u32,
        part: Part,
    },
    Continuity {
        index: u32,
        fragment: ContinuityFragment,
    },
    /// The vendor's moderation labels for a generated candidate. Informational:
    /// the neutral model never gates on them (see [`SafetyRating`]).
    SafetyRatings {
        ratings: Vec<SafetyRating>,
    },
    Usage(Usage),
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

#[derive(Clone, Debug, Default)]
pub struct Accumulated {
    pub parts: Vec<Part>,
    pub finish_reason: Option<FinishReason>,
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
    /// Returns a codec error for an event that does not suit the part it targets
    /// and [`Error::LimitExceeded`] once a partial part exceeds its byte budget.
    pub fn apply(&mut self, event: StreamEvent) -> Result<Option<Part>> {
        match event {
            StreamEvent::TextStart { index } => Ok(self.begin(
                index,
                Partial::Text {
                    text: String::new(),
                    citations: Vec::new(),
                },
            )),
            StreamEvent::TextDelta { index, text } => self.grow_text(index, &text),
            StreamEvent::ReasoningStart { index, visibility } => Ok(self.begin(
                index,
                Partial::Reasoning {
                    visibility,
                    text: String::new(),
                },
            )),
            StreamEvent::ReasoningDelta { index, text } => self.grow_reasoning(index, &text),
            StreamEvent::RefusalDelta { index, text } => self.grow_refusal(index, &text),
            StreamEvent::ToolCallStart {
                index,
                call_id,
                name,
            } => Ok(self.begin(
                index,
                Partial::ToolCall {
                    call_id,
                    name,
                    arguments: String::new(),
                },
            )),
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
            } => Ok(self.begin(
                index,
                Partial::Media {
                    kind,
                    mime,
                    metadata,
                    bytes: Vec::new(),
                },
            )),
            StreamEvent::MediaChunk { index, bytes, last } => self.grow_media(index, &bytes, last),
            StreamEvent::MediaMetadata { index, metadata } => {
                self.set_media_metadata(index, metadata)
            }
            StreamEvent::ProviderToolCall {
                index,
                call_id,
                tool,
                input,
            } => Ok(Some(self.provider_call(
                index,
                ProviderToolCallPart {
                    call_id,
                    tool,
                    input,
                    continuity: None,
                },
            ))),
            StreamEvent::ProviderToolResult {
                index,
                call_id,
                tool,
                output,
                citations,
            } => Ok(Some(self.provider_result(
                index,
                ProviderToolResultPart {
                    call_id,
                    tool,
                    output,
                    citations,
                    continuity: None,
                },
            ))),
            StreamEvent::Citation { index, citation } => self.add_citation(index, citation),
            StreamEvent::Part { index, part } => {
                self.complete(index, part.clone());
                Ok(Some(part))
            }
            StreamEvent::Continuity { .. } | StreamEvent::SafetyRatings { .. } => Ok(None),
            StreamEvent::Usage(usage) => {
                self.usage = usage;
                Ok(None)
            }
            StreamEvent::Finish(reason) => {
                self.finish_reason = Some(reason);
                Ok(None)
            }
        }
    }

    fn begin(&mut self, index: u32, partial: Partial) -> Option<Part> {
        self.partials.insert(index, partial);
        None
    }

    fn provider_call(&mut self, index: u32, mut part: ProviderToolCallPart) -> Part {
        part.continuity = self.continuity.remove(&index);
        let part = Part::ProviderToolCall(part);
        self.complete(index, part.clone());
        part
    }

    fn provider_result(&mut self, index: u32, mut part: ProviderToolResultPart) -> Part {
        part.continuity = self.continuity.remove(&index);
        let part = Part::ProviderToolResult(part);
        self.complete(index, part.clone());
        part
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
        let arguments = if arguments.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&arguments)
                .map_err(|error| Error::Codec(format!("tool arguments: {error}")))?
        };
        let part = Part::ToolCall(ToolCallPart {
            call_id,
            name,
            arguments,
            continuity: self.continuity.remove(&index),
        });
        self.complete.insert(index, part.clone());
        Ok(Some(part))
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
                    self.complete(index, part.clone());
                    return Ok(Some(part));
                }
                Ok(None)
            }
            other => Err(Error::Codec(format!(
                "media chunk for a non-media part: {other:?}"
            ))),
        }
    }

    pub fn attach_continuity(
        &mut self,
        index: u32,
        token: crate::domain::continuity::ContinuityToken,
    ) {
        self.continuity.insert(index, token);
    }

    /// Closes every open part and returns the assembled content in index order.
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
                } => {
                    let arguments = if arguments.trim().is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(&arguments)
                            .map_err(|error| Error::Codec(format!("tool arguments: {error}")))?
                    };
                    Part::ToolCall(ToolCallPart {
                        call_id,
                        name,
                        arguments,
                        continuity: self.continuity.remove(&index),
                    })
                }
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

    fn complete(&mut self, index: u32, part: Part) {
        self.partials.remove(&index);
        self.complete.insert(index, part);
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
    let finish_reason = accumulated.finish_reason.unwrap_or(FinishReason::Stop);
    let status = match finish_reason {
        FinishReason::Length => MessageStatus::Truncated,
        FinishReason::Cancelled => MessageStatus::Interrupted,
        _ => MessageStatus::Complete,
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
}
