use std::collections::BTreeMap;

use base64::Engine as _;
use futures_util::StreamExt;
use norte_core::{
    CallId, Citation, CitationSource, Error, ExtensionPart, FinishReason, MediaKind, MediaMetadata,
    MimeType, ModelStream, Part, ProviderError, ProviderErrorKind, ProviderId, ProviderTool,
    ReasoningVisibility, RefusalPart, Result, StreamEvent, Usage, VendorUsage,
};
use norte_http::SseStream;
use serde_json::Value;

use crate::continuity::OpenAiContinuity;
use crate::json::{opt_str, str_field, u32_field};

/// The audio-output events carry no index, so the single audio stream is pinned
/// to a fixed part index well clear of the item indices.
const AUDIO_INDEX: u32 = 10_000;
/// The Responses wire streams raw 16-bit little-endian PCM at 24 kHz.
const AUDIO_MIME: &str = "audio/pcm16";

/// Maps the vendor `SSE` stream into core stream events.
pub(crate) fn map(sse: SseStream, continuity: OpenAiContinuity) -> ModelStream {
    let mut decoder = Decoder::new(continuity);
    let mut failed = false;
    let stream = sse.flat_map(move |event| {
        let mut out: Vec<Result<StreamEvent>> = Vec::new();
        if !failed {
            match event {
                Ok(event) => match decoder.decode(&event.data) {
                    Ok(events) => out.extend(events.into_iter().map(Ok)),
                    Err(error) => {
                        out.push(Err(error));
                        failed = true;
                    }
                },
                Err(error) => {
                    out.push(Err(error));
                    failed = true;
                }
            }
        }
        futures_util::stream::iter(out)
    });
    Box::pin(stream)
}

#[derive(Clone, Debug)]
struct Decoder {
    continuity: OpenAiContinuity,
    calls: BTreeMap<u32, CallId>,
    started_reasoning: BTreeMap<u32, ReasoningVisibility>,
    saw_tool_call: bool,
    audio_started: bool,
    audio_transcript: String,
}

impl Decoder {
    fn new(continuity: OpenAiContinuity) -> Self {
        Self {
            continuity,
            calls: BTreeMap::new(),
            started_reasoning: BTreeMap::new(),
            saw_tool_call: false,
            audio_started: false,
            audio_transcript: String::new(),
        }
    }

    fn decode(&mut self, data: &str) -> Result<Vec<StreamEvent>> {
        if data == "[DONE]" {
            return Ok(Vec::new());
        }
        let value: Value = serde_json::from_str(data)?;
        match str_field(&value, "type")? {
            "response.output_item.added" => self.output_item_added(&value),
            "response.output_item.done" => self.output_item_done(&value),
            "response.output_text.delta" => Self::text_delta(&value),
            "response.reasoning_summary_text.delta" => {
                self.reasoning_delta(&value, ReasoningVisibility::Summary)
            }
            "response.reasoning_text.delta" => {
                self.reasoning_delta(&value, ReasoningVisibility::Full)
            }
            "response.function_call_arguments.delta" => self.arguments_delta(&value),
            "response.output_text.annotation.added" => Ok(Self::annotation(&value)),
            "response.refusal.delta" => Self::refusal_delta(&value),
            "response.refusal.done" => Self::refusal(&value),
            "response.audio.delta" => self.audio_delta(&value),
            "response.audio.done" => Ok(self.audio_done()),
            "response.audio.transcript.delta" => self.transcript_delta(&value),
            "response.completed" | "response.incomplete" => Ok(self.finish(&value)),
            "response.failed" => Err(failed_error(&value)),
            "error" => Err(stream_error(&value)),
            _ => Ok(Vec::new()),
        }
    }

    fn output_item_added(&mut self, value: &Value) -> Result<Vec<StreamEvent>> {
        let index = u32_field(value, "output_index")?;
        let item = item_of(value)?;
        match opt_str(item, "type") {
            Some("function_call") => {
                let call_id = CallId::new();
                self.calls.insert(index, call_id);
                self.saw_tool_call = true;
                let name = str_field(item, "name")?.to_owned();
                Ok(vec![StreamEvent::ToolCallStart {
                    index,
                    call_id,
                    name,
                }])
            }
            _ => Ok(Vec::new()),
        }
    }

    fn output_item_done(&mut self, value: &Value) -> Result<Vec<StreamEvent>> {
        let index = u32_field(value, "output_index")?;
        let item = item_of(value)?;
        let Some(kind) = opt_str(item, "type") else {
            return Ok(Vec::new());
        };
        match kind {
            "reasoning" => self.reasoning_done(index, item),
            "function_call" => {
                let meta = serde_json::json!({
                    "call_id": opt_str(item, "call_id"),
                    "id": opt_str(item, "id"),
                });
                Ok(vec![
                    StreamEvent::Continuity {
                        index,
                        fragment: self.continuity.fragment(&meta)?,
                    },
                    StreamEvent::ToolCallDone { index },
                ])
            }
            "image_generation_call" => image_output(index, item),
            "message" => Ok(Vec::new()),
            // Provider-executed tool items are typed; anything else is replayed
            // to the same vendor verbatim.
            other => self.provider_item(index, other, item),
        }
    }

    /// Types a provider-executed output item (web search, file search, code
    /// interpreter, MCP, …) while keeping the verbatim item in continuity, so
    /// replay stays byte-exact (FR-ADP-3). Items with no typed mapping stay
    /// opaque `Extension` parts.
    fn provider_item(&self, index: u32, kind: &str, item: &Value) -> Result<Vec<StreamEvent>> {
        let Some((name, is_result)) = provider_kind(kind) else {
            return Ok(vec![StreamEvent::Part {
                index,
                part: Part::Extension(ExtensionPart {
                    namespace: "openai".to_owned(),
                    data: item.clone(),
                }),
            }]);
        };
        let tool = ProviderTool {
            namespace: "openai".to_owned(),
            name: name.to_owned(),
        };
        let call_id = CallId::new();
        let event = if is_result {
            StreamEvent::ProviderToolResult {
                index,
                call_id,
                tool,
                output: item.clone(),
                citations: Vec::new(),
            }
        } else {
            StreamEvent::ProviderToolCall {
                index,
                call_id,
                tool,
                input: item.clone(),
            }
        };
        Ok(vec![
            StreamEvent::Continuity {
                index,
                fragment: self.continuity.fragment(item)?,
            },
            event,
        ])
    }

    fn reasoning_done(&mut self, index: u32, item: &Value) -> Result<Vec<StreamEvent>> {
        let mut events = Vec::new();
        if let std::collections::btree_map::Entry::Vacant(slot) =
            self.started_reasoning.entry(index)
        {
            let visibility = reasoning_visibility(item);
            slot.insert(visibility);
            events.push(StreamEvent::ReasoningStart { index, visibility });
            let summary = summary_text(item);
            if !summary.is_empty() {
                events.push(StreamEvent::ReasoningDelta {
                    index,
                    text: summary,
                });
            }
        }
        events.push(StreamEvent::Continuity {
            index,
            fragment: self.continuity.fragment(item)?,
        });
        Ok(events)
    }

    fn text_delta(value: &Value) -> Result<Vec<StreamEvent>> {
        Ok(vec![StreamEvent::TextDelta {
            index: u32_field(value, "output_index")?,
            text: str_field(value, "delta")?.to_owned(),
        }])
    }

    fn reasoning_delta(
        &mut self,
        value: &Value,
        visibility: ReasoningVisibility,
    ) -> Result<Vec<StreamEvent>> {
        let index = u32_field(value, "output_index")?;
        let text = str_field(value, "delta")?.to_owned();
        let mut events = Vec::new();
        if self.started_reasoning.insert(index, visibility).is_none() {
            events.push(StreamEvent::ReasoningStart { index, visibility });
        }
        events.push(StreamEvent::ReasoningDelta { index, text });
        Ok(events)
    }

    fn arguments_delta(&mut self, value: &Value) -> Result<Vec<StreamEvent>> {
        let index = u32_field(value, "output_index")?;
        let arguments = str_field(value, "delta")?.to_owned();
        let call_id = *self.calls.entry(index).or_default();
        self.saw_tool_call = true;
        Ok(vec![StreamEvent::ToolCallDelta {
            index,
            call_id,
            name: None,
            arguments,
        }])
    }

    /// Maps a URL/file citation annotation onto the neutral [`Citation`].
    fn annotation(value: &Value) -> Vec<StreamEvent> {
        let Ok(index) = u32_field(value, "output_index") else {
            return Vec::new();
        };
        let Some(annotation) = value.get("annotation").filter(|item| !item.is_null()) else {
            return Vec::new();
        };
        vec![StreamEvent::Citation {
            index,
            citation: citation_of(annotation),
        }]
    }

    fn refusal(value: &Value) -> Result<Vec<StreamEvent>> {
        Ok(vec![StreamEvent::Part {
            index: u32_field(value, "output_index")?,
            part: Part::Refusal(RefusalPart {
                text: str_field(value, "refusal")?.to_owned(),
            }),
        }])
    }

    fn refusal_delta(value: &Value) -> Result<Vec<StreamEvent>> {
        Ok(vec![StreamEvent::RefusalDelta {
            index: u32_field(value, "output_index")?,
            text: str_field(value, "delta")?.to_owned(),
        }])
    }

    fn audio_delta(&mut self, value: &Value) -> Result<Vec<StreamEvent>> {
        let bytes = crate::json::base64_decode(str_field(value, "delta")?)?;
        let mut events = Vec::new();
        if !self.audio_started {
            self.audio_started = true;
            events.push(StreamEvent::MediaStart {
                index: AUDIO_INDEX,
                kind: MediaKind::Audio,
                mime: MimeType::new(AUDIO_MIME)?,
                metadata: MediaMetadata::default(),
            });
        }
        events.push(StreamEvent::MediaChunk {
            index: AUDIO_INDEX,
            bytes,
            last: false,
        });
        Ok(events)
    }

    fn audio_done(&mut self) -> Vec<StreamEvent> {
        if !self.audio_started {
            return Vec::new();
        }
        self.audio_started = false;
        let mut events = Vec::new();
        if !self.audio_transcript.is_empty() {
            let mut metadata = MediaMetadata::default();
            metadata.transcript = Some(std::mem::take(&mut self.audio_transcript));
            events.push(StreamEvent::MediaMetadata {
                index: AUDIO_INDEX,
                metadata,
            });
        }
        events.push(StreamEvent::MediaChunk {
            index: AUDIO_INDEX,
            bytes: Vec::new(),
            last: true,
        });
        events
    }

    fn transcript_delta(&mut self, value: &Value) -> Result<Vec<StreamEvent>> {
        self.audio_transcript.push_str(str_field(value, "delta")?);
        Ok(Vec::new())
    }

    fn finish(&self, value: &Value) -> Vec<StreamEvent> {
        let response = value.get("response").unwrap_or(value);
        vec![
            StreamEvent::Usage(usage_of(response)),
            StreamEvent::Finish(self.finish_reason(response)),
        ]
    }

    fn finish_reason(&self, response: &Value) -> FinishReason {
        if self.saw_tool_call {
            return FinishReason::ToolCalls;
        }
        let status = opt_str(response, "status");
        let detail = response
            .get("incomplete_details")
            .and_then(|details| opt_str(details, "reason"));
        match (status, detail) {
            (Some("incomplete"), Some("content_filter")) => FinishReason::ContentFilter,
            (Some("incomplete"), _) => FinishReason::Length,
            _ => FinishReason::Stop,
        }
    }
}

/// Maps one output-text annotation onto the neutral [`Citation`].
fn citation_of(annotation: &Value) -> Citation {
    match opt_str(annotation, "type").unwrap_or_default() {
        "url_citation" => Citation {
            source: opt_str(annotation, "url").map_or_else(
                || extension_citation(annotation),
                |url| CitationSource::Url(url.to_owned()),
            ),
            title: opt_str(annotation, "title").map(ToOwned::to_owned),
        },
        "file_citation" | "container_file_citation" | "file_path" => file_citation(annotation),
        _ => Citation {
            source: extension_citation(annotation),
            title: None,
        },
    }
}

fn file_citation(annotation: &Value) -> Citation {
    let title = opt_str(annotation, "filename").map(ToOwned::to_owned);
    if let (Some(file_id), Ok(provider)) =
        (opt_str(annotation, "file_id"), ProviderId::new("openai"))
    {
        Citation {
            source: CitationSource::ProviderFile {
                provider,
                file_id: file_id.to_owned(),
            },
            title,
        }
    } else {
        Citation {
            source: extension_citation(annotation),
            title,
        }
    }
}

fn extension_citation(annotation: &Value) -> CitationSource {
    CitationSource::Extension {
        namespace: "openai".to_owned(),
        data: annotation.clone(),
    }
}

fn item_of(value: &Value) -> Result<&Value> {
    value
        .get("item")
        .ok_or_else(|| Error::Codec("openai wire: output item event has no item".to_owned()))
}

/// The neutral tool name and role of an `OpenAI` provider-executed output item.
/// `true` means the item is a completed execution (a result); `false` means it
/// is the invocation only (a call). Items the host executes, or that carry no
/// provider-tool meaning, have no mapping.
fn provider_kind(kind: &str) -> Option<(&'static str, bool)> {
    Some(match kind {
        "web_search_call" => ("web_search", false),
        "mcp_approval_request" => ("mcp_approval_request", false),
        "file_search_call" => ("file_search", true),
        "code_interpreter_call" => ("code_interpreter", true),
        "mcp_call" => ("mcp", true),
        "mcp_list_tools" => ("mcp_list_tools", true),
        _ => return None,
    })
}

fn reasoning_visibility(item: &Value) -> ReasoningVisibility {
    let has_summary = item
        .get("summary")
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            parts
                .iter()
                .any(|part| opt_str(part, "text").is_some_and(|text| !text.is_empty()))
        });
    if has_summary {
        ReasoningVisibility::Summary
    } else {
        ReasoningVisibility::Redacted
    }
}

fn summary_text(item: &Value) -> String {
    item.get("summary")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| opt_str(part, "text"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn image_output(index: u32, item: &Value) -> Result<Vec<StreamEvent>> {
    let Some(encoded) = opt_str(item, "result") else {
        return Ok(Vec::new());
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| Error::Codec(format!("openai image output: {error}")))?;
    let mut metadata = MediaMetadata::default();
    if let Some(size) = opt_str(item, "size") {
        let (width, height) = parse_size(size);
        metadata.width = width;
        metadata.height = height;
    }
    Ok(vec![
        StreamEvent::MediaStart {
            index,
            kind: MediaKind::Image,
            mime: MimeType::new("image/png")?,
            metadata: metadata.clone(),
        },
        StreamEvent::MediaChunk {
            index,
            bytes,
            last: true,
        },
    ])
}

fn parse_size(size: &str) -> (Option<u32>, Option<u32>) {
    let Some((width, height)) = size.split_once('x') else {
        return (None, None);
    };
    (width.trim().parse().ok(), height.trim().parse().ok())
}

fn usage_of(response: &Value) -> Usage {
    let Some(usage) = response.get("usage") else {
        return Usage::default();
    };
    let mut vendor = VendorUsage::new(usage);
    let mut mapped = Usage::default();
    mapped.input_tokens = vendor.take(&["input_tokens"]);
    mapped.output_tokens = vendor.take(&["output_tokens"]);
    mapped.reasoning_tokens = vendor.take(&["output_tokens_details", "reasoning_tokens"]);
    mapped.cache_read_tokens = vendor.take(&["input_tokens_details", "cached_tokens"]);
    mapped.cache_write_tokens = vendor.take(&["input_tokens_details", "cache_write_tokens"]);
    mapped.vendor = vendor.remainder();
    mapped
}

fn failed_error(value: &Value) -> Error {
    let response = value.get("response").unwrap_or(value);
    let error = response.get("error").unwrap_or(response);
    let message = opt_str(error, "message").unwrap_or("openai response failed");
    provider(message, opt_str(error, "code"))
}

fn stream_error(value: &Value) -> Error {
    let message = opt_str(value, "message").unwrap_or("openai stream error");
    provider(message, opt_str(value, "code"))
}

fn provider(message: &str, code: Option<&str>) -> Error {
    let mut provider = ProviderError::new(ProviderErrorKind::Other, message.to_owned());
    if let Some(code) = code {
        provider = provider.with_code(code.to_owned());
    }
    Error::provider(provider)
}

/// A stateful entry point the `fuzz` crate drives with arbitrary `SSE` payloads.
///
/// Enabled by the `fuzz` feature. It is not part of the supported API.
#[cfg(feature = "fuzz")]
#[doc(hidden)]
pub struct FuzzResponses {
    decoder: Decoder,
}

#[cfg(feature = "fuzz")]
impl FuzzResponses {
    /// Creates a decoder bound to `continuity`.
    #[must_use]
    pub fn new(continuity: OpenAiContinuity) -> Self {
        Self {
            decoder: Decoder::new(continuity),
        }
    }

    /// Decodes one raw `SSE` `data` payload.
    ///
    /// # Errors
    ///
    /// Returns a codec error for malformed input.
    pub fn decode(&mut self, data: &str) -> Result<Vec<StreamEvent>> {
        self.decoder.decode(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use norte_core::{ContinuityAdapter, ModelId, ModelRef, ProviderId, WireFamily};
    use norte_http::SseEvent;

    use crate::WIRE_RESPONSES;

    fn continuity() -> OpenAiContinuity {
        let model = ModelRef::new(
            ProviderId::new("openai").expect("provider"),
            ModelId::new("gpt-5").expect("model"),
        );
        OpenAiContinuity::new(WireFamily::new(WIRE_RESPONSES).expect("family"), model)
    }

    fn stream_of(data: &[&str]) -> SseStream {
        let events: Vec<Result<SseEvent>> = data
            .iter()
            .map(|data| {
                Ok(SseEvent {
                    data: (*data).to_owned(),
                    ..SseEvent::default()
                })
            })
            .collect();
        Box::pin(futures_util::stream::iter(events))
    }

    async fn drain(data: &[&str]) -> Vec<StreamEvent> {
        let mut stream = map(stream_of(data), continuity());
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.expect("event"));
        }
        events
    }

    /// Loads a recorded transcript — a JSON array of SSE `data` payloads — and
    /// maps it through the wire, so live-provider behaviour is pinned hermetically
    /// (FR-TEST-2).
    async fn drain_fixture(fixture: &str) -> Vec<StreamEvent> {
        let data: Vec<String> = serde_json::from_str(fixture).expect("fixture json");
        let refs: Vec<&str> = data.iter().map(String::as_str).collect();
        drain(&refs).await
    }

    #[tokio::test]
    async fn recorded_transcript_pins_the_critical_path() {
        let events = drain_fixture(include_str!("../../fixtures/openai.responses.json")).await;
        let text: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::TextDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello, world");
        let usage = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::Usage(usage) => Some(usage),
                _ => None,
            })
            .expect("usage");
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(3));
        assert_eq!(usage.cache_read_tokens, Some(8));
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finish(FinishReason::Stop))
        );
    }

    #[tokio::test]
    async fn text_deltas_and_usage_survive_the_decode() {
        let events = drain(&[
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"Hel"}"#,
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"lo"}"#,
            r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":7,"output_tokens":2,"total_tokens":9,"input_tokens_details":{"cached_tokens":3,"cache_write_tokens":1,"text_tokens":5,"image_tokens":2},"output_tokens_details":{"reasoning_tokens":1}}}}"#,
        ])
        .await;
        assert_eq!(
            events[0],
            StreamEvent::TextDelta {
                index: 0,
                text: "Hel".to_owned()
            }
        );
        assert_eq!(
            events[1],
            StreamEvent::TextDelta {
                index: 0,
                text: "lo".to_owned()
            }
        );
        let StreamEvent::Usage(usage) = &events[2] else {
            panic!("expected usage");
        };
        assert_eq!(usage.input_tokens, Some(7));
        assert_eq!(usage.output_tokens, Some(2));
        assert_eq!(usage.cache_read_tokens, Some(3));
        assert_eq!(usage.cache_write_tokens, Some(1));
        assert_eq!(usage.reasoning_tokens, Some(1));
        // No field of this family is named by the neutral vocabulary, so they
        // survive in `vendor` rather than dying at the parser.
        assert_eq!(
            usage.vendor.get("total_tokens").and_then(Value::as_u64),
            Some(9)
        );
        let details = usage
            .vendor
            .get("input_tokens_details")
            .expect("leftover details");
        assert_eq!(details.get("text_tokens").and_then(Value::as_u64), Some(5));
        assert_eq!(details.get("image_tokens").and_then(Value::as_u64), Some(2));
        assert_eq!(events[3], StreamEvent::Finish(FinishReason::Stop));
    }

    #[tokio::test]
    async fn function_calls_carry_their_vendor_identity() {
        let events = drain(&[
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","name":"lookup"}}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"q\":"}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"1}"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"lookup","arguments":"{\"q\":1}"}}"#,
            r#"{"type":"response.completed","response":{"status":"completed","usage":{}}}"#,
        ])
        .await;
        let StreamEvent::ToolCallStart { index, name, .. } = &events[0] else {
            panic!("expected tool call start");
        };
        assert_eq!(*index, 0);
        assert_eq!(name, "lookup");
        let mut arguments = String::new();
        let mut fragment = None;
        for event in &events[1..] {
            match event {
                StreamEvent::ToolCallDelta {
                    arguments: delta, ..
                } => arguments.push_str(delta),
                StreamEvent::Continuity {
                    fragment: value, ..
                } => fragment = Some(value.clone()),
                _ => {}
            }
        }
        assert_eq!(arguments, r#"{"q":1}"#);
        let decoded = continuity()
            .decode(
                &continuity()
                    .merge(None, fragment.expect("fragment"))
                    .expect("merge"),
            )
            .expect("decode");
        assert_eq!(decoded["call_id"], "call_1");
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finish(FinishReason::ToolCalls))
        );
    }

    #[tokio::test]
    async fn reasoning_summary_streams_then_settles_with_continuity() {
        let events = drain(&[
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[]}}"#,
            r#"{"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"think"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"think"}],"encrypted_content":"enc"}}"#,
            r#"{"type":"response.completed","response":{"status":"completed","usage":{}}}"#,
        ])
        .await;
        assert_eq!(
            events[0],
            StreamEvent::ReasoningStart {
                index: 0,
                visibility: ReasoningVisibility::Summary,
            }
        );
        assert_eq!(
            events[1],
            StreamEvent::ReasoningDelta {
                index: 0,
                text: "think".to_owned()
            }
        );
        let StreamEvent::Continuity { fragment, .. } = &events[2] else {
            panic!("expected continuity");
        };
        let decoded = continuity()
            .decode(&continuity().merge(None, fragment.clone()).expect("merge"))
            .expect("decode");
        assert_eq!(decoded["encrypted_content"], "enc");
    }

    #[tokio::test]
    async fn incomplete_responses_finish_as_length() {
        let events = drain(&[
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"partial"}"#,
            r#"{"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{}}}"#,
        ])
        .await;
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finish(FinishReason::Length))
        );
    }

    #[tokio::test]
    async fn image_generation_output_is_materialized() {
        let events = drain(&[
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"image_generation_call","result":"AQID","status":"completed","size":"16x16"}}"#,
        ])
        .await;
        let StreamEvent::MediaStart {
            kind,
            mime,
            metadata,
            ..
        } = &events[0]
        else {
            panic!("expected media start");
        };
        assert_eq!(*kind, MediaKind::Image);
        assert_eq!(mime.as_str(), "image/png");
        assert_eq!(metadata.width, Some(16));
        assert_eq!(metadata.height, Some(16));
        assert_eq!(
            events[1],
            StreamEvent::MediaChunk {
                index: 0,
                bytes: vec![1, 2, 3],
                last: true,
            }
        );
    }

    #[tokio::test]
    async fn streamed_refusal_becomes_a_refusal_part() {
        let events = drain(&[
            r#"{"type":"response.refusal.delta","output_index":0,"content_index":0,"delta":"I can"}"#,
            r#"{"type":"response.refusal.done","output_index":0,"content_index":0,"refusal":"I cannot help"}"#,
        ])
        .await;
        assert_eq!(
            events[0],
            StreamEvent::RefusalDelta {
                index: 0,
                text: "I can".to_owned(),
            }
        );
        assert_eq!(
            events[1],
            StreamEvent::Part {
                index: 0,
                part: Part::Refusal(RefusalPart {
                    text: "I cannot help".to_owned(),
                }),
            }
        );
    }

    #[tokio::test]
    async fn streamed_audio_output_is_materialized() {
        let events = drain(&[
            r#"{"type":"response.audio.transcript.delta","response_id":"r","delta":"hello"}"#,
            r#"{"type":"response.audio.delta","response_id":"r","delta":"AQID"}"#,
            r#"{"type":"response.audio.delta","response_id":"r","delta":"BAU="}"#,
            r#"{"type":"response.audio.done","response_id":"r"}"#,
        ])
        .await;
        let StreamEvent::MediaStart {
            index, kind, mime, ..
        } = &events[0]
        else {
            panic!("expected media start");
        };
        assert_eq!(*index, AUDIO_INDEX);
        assert_eq!(*kind, MediaKind::Audio);
        assert_eq!(mime.as_str(), "audio/pcm16");
        assert_eq!(
            events[1],
            StreamEvent::MediaChunk {
                index: AUDIO_INDEX,
                bytes: vec![1, 2, 3],
                last: false,
            }
        );
        assert_eq!(
            events[2],
            StreamEvent::MediaChunk {
                index: AUDIO_INDEX,
                bytes: vec![4, 5],
                last: false,
            }
        );
        let StreamEvent::MediaMetadata { index, metadata } = &events[3] else {
            panic!("expected media metadata");
        };
        assert_eq!(*index, AUDIO_INDEX);
        assert_eq!(metadata.transcript.as_deref(), Some("hello"));
        assert_eq!(
            events[4],
            StreamEvent::MediaChunk {
                index: AUDIO_INDEX,
                bytes: Vec::new(),
                last: true,
            }
        );
    }

    #[tokio::test]
    async fn url_citation_annotations_become_citations() {
        let events = drain(&[
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"see "}"#,
            r#"{"type":"response.output_text.annotation.added","output_index":0,"content_index":0,"annotation_index":0,"annotation":{"type":"url_citation","url":"https://example.test","title":"Example","start_index":0,"end_index":3}}"#,
        ])
        .await;
        let StreamEvent::Citation { index, citation } = &events[1] else {
            panic!("expected a citation");
        };
        assert_eq!(*index, 0);
        assert_eq!(
            citation.source,
            CitationSource::Url("https://example.test".to_owned())
        );
        assert_eq!(citation.title.as_deref(), Some("Example"));
    }

    #[tokio::test]
    async fn file_citations_name_the_provider_file() {
        let events = drain(&[
            r#"{"type":"response.output_text.annotation.added","output_index":0,"annotation":{"type":"file_citation","file_id":"file_1","filename":"notes.txt","index":0}}"#,
        ])
        .await;
        let StreamEvent::Citation { citation, .. } = &events[0] else {
            panic!("expected a citation");
        };
        let CitationSource::ProviderFile { provider, file_id } = &citation.source else {
            panic!("expected a provider file source");
        };
        assert_eq!(provider.as_str(), "openai");
        assert_eq!(file_id, "file_1");
        assert_eq!(citation.title.as_deref(), Some("notes.txt"));
    }

    #[tokio::test]
    async fn provider_tool_items_are_typed() {
        let events = drain(&[
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","query":"rust"}}}"#,
            r#"{"type":"response.output_item.done","output_index":1,"item":{"type":"file_search_call","id":"fs_1","status":"completed","queries":["rust"],"results":[{"file_id":"f_1","text":"fast"}]}}"#,
        ])
        .await;
        let StreamEvent::ProviderToolCall { tool, input, .. } = &events[1] else {
            panic!("expected a provider tool call, got {events:?}");
        };
        assert_eq!(tool.namespace, "openai");
        assert_eq!(tool.name, "web_search");
        assert_eq!(input["action"]["query"], "rust");
        let StreamEvent::ProviderToolResult { tool, output, .. } = &events[3] else {
            panic!("expected a provider tool result, got {events:?}");
        };
        assert_eq!(tool.name, "file_search");
        assert_eq!(output["results"][0]["text"], "fast");
    }

    #[tokio::test]
    async fn host_executed_items_round_trip_as_extensions() {
        let events = drain(&[
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"custom_tool_call","call_id":"c_1","name":"run","input":"x"}}"#,
        ])
        .await;
        let StreamEvent::Part {
            part: Part::Extension(extension),
            ..
        } = &events[0]
        else {
            panic!("expected an extension part");
        };
        assert_eq!(extension.namespace, "openai");
        assert_eq!(extension.data["type"], "custom_tool_call");
        assert_eq!(extension.data["input"], "x");
    }

    #[tokio::test]
    async fn failures_and_unknown_events_are_handled() {
        let mut stream = map(
            stream_of(&[
                r#"{"type":"keepalive"}"#,
                r#"{"type":"response.failed","response":{"error":{"code":"server_error","message":"boom"}}}"#,
            ]),
            continuity(),
        );
        let error = stream.next().await.expect("some").expect_err("error");
        assert_eq!(error.provider_kind(), Some(ProviderErrorKind::Other));

        let done = drain(&["[DONE]", r#"{"type":"response.created"}"#]).await;
        assert!(done.is_empty());
    }
}
