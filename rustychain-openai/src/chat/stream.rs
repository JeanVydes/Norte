use std::collections::BTreeMap;

use futures_util::StreamExt;
use rustychain_core::{
    CallId, Citation, CitationSource, Error, FinishReason, MediaKind, MediaMetadata, MimeType,
    ModelStream, ProviderError, ProviderErrorKind, ReasoningVisibility, Result, StreamEvent, Usage,
};
use rustychain_http::SseStream;
use serde_json::Value;

use crate::continuity::OpenAiContinuity;
use crate::json::{base64_decode, opt_str};

use super::AUDIO_ID_KEY;

const REASONING_INDEX: u32 = 0;
const TEXT_INDEX: u32 = 1;
const REFUSAL_INDEX: u32 = 2;
const AUDIO_INDEX: u32 = 3;
const TOOL_BASE: u32 = 1000;
const FUNCTION_CALL_INDEX: u32 = TOOL_BASE - 1;
/// Chat Completions streams output audio as raw 16-bit little-endian PCM.
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
struct ToolState {
    call_id: CallId,
    named: bool,
}

/// Tracks the deprecated `function_call` delta shape, which carries no id.
#[derive(Clone, Debug)]
struct LegacyCall {
    call_id: CallId,
    named: bool,
}

#[derive(Clone, Debug)]
struct Decoder {
    continuity: OpenAiContinuity,
    tools: BTreeMap<u32, ToolState>,
    legacy: Option<LegacyCall>,
    reasoning_started: bool,
    audio_started: bool,
    audio_id: Option<String>,
    audio_transcript: String,
}

impl Decoder {
    fn new(continuity: OpenAiContinuity) -> Self {
        Self {
            continuity,
            tools: BTreeMap::new(),
            legacy: None,
            reasoning_started: false,
            audio_started: false,
            audio_id: None,
            audio_transcript: String::new(),
        }
    }

    fn decode(&mut self, data: &str) -> Result<Vec<StreamEvent>> {
        if data == "[DONE]" {
            return Ok(self.flush_audio());
        }
        let value: Value = serde_json::from_str(data)?;
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            return Err(stream_error(error));
        }
        let mut events = Vec::new();
        if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
            events.push(StreamEvent::Usage(usage_of(usage)));
        }
        for choice in value
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(delta) = choice.get("delta") {
                self.delta(delta, &mut events)?;
            }
            if let Some(finish) = choice.get("finish_reason").and_then(Value::as_str) {
                events.extend(self.flush_audio());
                events.push(StreamEvent::Finish(finish_reason(finish)));
            }
        }
        Ok(events)
    }

    fn delta(&mut self, delta: &Value, events: &mut Vec<StreamEvent>) -> Result<()> {
        if let Some(text) = opt_str(delta, "content").filter(|text| !text.is_empty()) {
            events.push(StreamEvent::TextDelta {
                index: TEXT_INDEX,
                text: text.to_owned(),
            });
        }
        self.reasoning(delta, events);
        if let Some(text) = opt_str(delta, "refusal").filter(|text| !text.is_empty()) {
            events.push(StreamEvent::RefusalDelta {
                index: REFUSAL_INDEX,
                text: text.to_owned(),
            });
        }
        if let Some(annotations) = delta.get("annotations").and_then(Value::as_array) {
            for annotation in annotations {
                if let Some(citation) = chat_citation(annotation) {
                    events.push(StreamEvent::Citation {
                        index: TEXT_INDEX,
                        citation,
                    });
                }
            }
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in tool_calls {
                self.tool_call(call, events)?;
            }
        }
        if let Some(call) = delta.get("function_call").filter(|call| !call.is_null()) {
            self.function_call(call, events);
        }
        if let Some(audio) = delta.get("audio").filter(|audio| !audio.is_null()) {
            self.audio(audio, events)?;
        }
        Ok(())
    }

    /// Streams audio output: the bytes become media chunks, while the vendor id
    /// and the transcript are settled into the part metadata when the block
    /// closes so an assistant turn can be replayed by reference.
    fn audio(&mut self, audio: &Value, events: &mut Vec<StreamEvent>) -> Result<()> {
        if self.audio_id.is_none() {
            self.audio_id = opt_str(audio, "id").map(ToOwned::to_owned);
        }
        if let Some(text) = opt_str(audio, "transcript").filter(|text| !text.is_empty()) {
            self.audio_transcript.push_str(text);
        }
        if let Some(data) = opt_str(audio, "data").filter(|data| !data.is_empty()) {
            let bytes = base64_decode(data)?;
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
        }
        Ok(())
    }

    fn flush_audio(&mut self) -> Vec<StreamEvent> {
        if !self.audio_started {
            return Vec::new();
        }
        self.audio_started = false;
        let mut metadata = MediaMetadata::default();
        if let Some(id) = self.audio_id.take() {
            metadata
                .extension
                .insert(AUDIO_ID_KEY.to_owned(), Value::String(id));
        }
        let transcript = std::mem::take(&mut self.audio_transcript);
        if !transcript.is_empty() {
            metadata.transcript = Some(transcript);
        }
        vec![
            StreamEvent::MediaMetadata {
                index: AUDIO_INDEX,
                metadata,
            },
            StreamEvent::MediaChunk {
                index: AUDIO_INDEX,
                bytes: Vec::new(),
                last: true,
            },
        ]
    }

    /// Maps the deprecated `function_call` delta shape, which carries the name
    /// and arguments without an id or index, onto a tool call so it is never
    /// dropped.
    fn function_call(&mut self, call: &Value, events: &mut Vec<StreamEvent>) {
        let name = opt_str(call, "name");
        let arguments = opt_str(call, "arguments").unwrap_or("");
        let call_id = if let Some(state) = &mut self.legacy {
            let call_id = state.call_id;
            if let Some(name) = name.filter(|_| !state.named) {
                state.named = true;
                events.push(StreamEvent::ToolCallDelta {
                    index: FUNCTION_CALL_INDEX,
                    call_id,
                    name: Some(name.to_owned()),
                    arguments: String::new(),
                });
            }
            call_id
        } else {
            let call_id = CallId::new();
            self.legacy = Some(LegacyCall {
                call_id,
                named: name.is_some(),
            });
            events.push(StreamEvent::ToolCallStart {
                index: FUNCTION_CALL_INDEX,
                call_id,
                name: name.unwrap_or_default().to_owned(),
            });
            call_id
        };
        if !arguments.is_empty() {
            events.push(StreamEvent::ToolCallDelta {
                index: FUNCTION_CALL_INDEX,
                call_id,
                name: None,
                arguments: arguments.to_owned(),
            });
        }
    }

    fn reasoning(&mut self, delta: &Value, events: &mut Vec<StreamEvent>) {
        let text = opt_str(delta, "reasoning_content")
            .or_else(|| opt_str(delta, "reasoning"))
            .filter(|text| !text.is_empty());
        let Some(text) = text else {
            return;
        };
        if !self.reasoning_started {
            self.reasoning_started = true;
            events.push(StreamEvent::ReasoningStart {
                index: REASONING_INDEX,
                visibility: ReasoningVisibility::Full,
            });
        }
        events.push(StreamEvent::ReasoningDelta {
            index: REASONING_INDEX,
            text: text.to_owned(),
        });
    }

    fn tool_call(&mut self, call: &Value, events: &mut Vec<StreamEvent>) -> Result<()> {
        let tool_index = call
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| u32::try_from(index).ok())
            .unwrap_or(0);
        let index = TOOL_BASE.saturating_add(tool_index);
        let function = call.get("function");
        let name = function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str);
        let arguments = function
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            .unwrap_or("");

        let call_id = if let Some(state) = self.tools.get(&tool_index) {
            state.call_id
        } else {
            let id = CallId::new();
            self.tools.insert(
                tool_index,
                ToolState {
                    call_id: id,
                    named: name.is_some(),
                },
            );
            events.push(StreamEvent::ToolCallStart {
                index,
                call_id: id,
                name: name.unwrap_or_default().to_owned(),
            });
            if let Some(vendor) = opt_str(call, "id") {
                events.push(StreamEvent::Continuity {
                    index,
                    fragment: self
                        .continuity
                        .fragment(&serde_json::json!({ "call_id": vendor }))?,
                });
            }
            id
        };

        if let Some(name) = name {
            if self
                .tools
                .get(&tool_index)
                .is_some_and(|state| !state.named)
            {
                if let Some(state) = self.tools.get_mut(&tool_index) {
                    state.named = true;
                }
                events.push(StreamEvent::ToolCallDelta {
                    index,
                    call_id,
                    name: Some(name.to_owned()),
                    arguments: String::new(),
                });
            }
        }
        if !arguments.is_empty() {
            events.push(StreamEvent::ToolCallDelta {
                index,
                call_id,
                name: None,
                arguments: arguments.to_owned(),
            });
        }
        Ok(())
    }
}

/// Maps one chat-completions annotation onto the neutral [`Citation`].
fn chat_citation(annotation: &Value) -> Option<Citation> {
    let source = match opt_str(annotation, "type")? {
        "url_citation" => {
            let body = annotation.get("url_citation")?;
            let url = opt_str(body, "url")?;
            CitationSource::Url(url.to_owned())
        }
        _ => CitationSource::Extension {
            namespace: "openai".to_owned(),
            data: annotation.clone(),
        },
    };
    let body = annotation.get("url_citation").unwrap_or(annotation);
    Some(Citation {
        source,
        title: opt_str(body, "title").map(ToOwned::to_owned),
    })
}

fn finish_reason(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        other => FinishReason::Other(other.to_owned()),
    }
}

fn usage_of(usage: &Value) -> Usage {
    let prompt = usage.get("prompt_tokens_details");
    let completion = usage.get("completion_tokens_details");
    let mut mapped = Usage::default();
    mapped.input_tokens = usage.get("prompt_tokens").and_then(Value::as_u64);
    mapped.output_tokens = usage.get("completion_tokens").and_then(Value::as_u64);
    mapped.reasoning_tokens = counter(completion, "reasoning_tokens");
    mapped.cache_read_tokens = counter(prompt, "cached_tokens");
    mapped.cache_write_tokens = counter(prompt, "cache_write_tokens");
    mapped.audio_input_tokens = counter(prompt, "audio_tokens");
    mapped.audio_output_tokens = counter(completion, "audio_tokens");
    collect_counters(
        &mut mapped,
        prompt,
        &[
            ("prompt_text_tokens", "text_tokens"),
            ("prompt_image_tokens", "image_tokens"),
        ],
    );
    collect_counters(
        &mut mapped,
        completion,
        &[
            ("completion_text_tokens", "text_tokens"),
            ("accepted_prediction_tokens", "accepted_prediction_tokens"),
            ("rejected_prediction_tokens", "rejected_prediction_tokens"),
        ],
    );
    mapped
}

/// Reads one vendor counter out of a details object, if present.
fn counter(details: Option<&Value>, key: &str) -> Option<u64> {
    details?.get(key).and_then(Value::as_u64)
}

/// Preserves vendor counters that have no neutral field, so none is lost.
fn collect_counters(mapped: &mut Usage, details: Option<&Value>, keys: &[(&str, &str)]) {
    for (neutral, vendor) in keys {
        if let Some(value) = counter(details, vendor) {
            mapped.extension.insert((*neutral).to_owned(), value);
        }
    }
}

fn stream_error(error: &Value) -> Error {
    let message = opt_str(error, "message").unwrap_or("openai stream error");
    let mut provider = ProviderError::new(ProviderErrorKind::Other, message.to_owned());
    if let Some(code) = opt_str(error, "code") {
        provider = provider.with_code(code.to_owned());
    }
    Error::provider(provider)
}

/// A stateful entry point the `fuzz` crate drives with arbitrary `SSE` payloads.
///
/// Enabled by the `fuzz` feature. It is not part of the supported API.
#[cfg(feature = "fuzz")]
#[doc(hidden)]
pub struct FuzzChat {
    decoder: Decoder,
}

#[cfg(feature = "fuzz")]
impl FuzzChat {
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
    use rustychain_core::{ContinuityAdapter, ModelId, ModelRef, ProviderId, WireFamily};
    use rustychain_http::SseEvent;

    use crate::WIRE_CHAT;

    fn continuity() -> OpenAiContinuity {
        let model = ModelRef::new(
            ProviderId::new("openai").expect("provider"),
            ModelId::new("gpt-4o").expect("model"),
        );
        OpenAiContinuity::new(WireFamily::new(WIRE_CHAT).expect("family"), model)
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
        let events = drain_fixture(include_str!(
            "../../../rustychain-testing/fixtures/openai.chat.json"
        ))
        .await;
        let text: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::TextDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello, world");
        let reasoning: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ReasoningDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(reasoning.contains("Hello, world"));
        let usage = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::Usage(usage) => Some(usage.clone()),
                _ => None,
            })
            .expect("usage");
        assert_eq!(usage.input_tokens, Some(23));
        assert_eq!(usage.output_tokens, Some(91));
        assert_eq!(usage.reasoning_tokens, Some(86));
        assert_eq!(usage.cache_read_tokens, Some(0));
        let finish = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::Finish(reason) => Some(reason.clone()),
                _ => None,
            })
            .expect("finish");
        assert_eq!(finish, FinishReason::Stop);
    }

    #[tokio::test]
    async fn content_tool_calls_usage_and_finish_decode() {
        let events = drain(&[
            r#"{"choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"lookup","arguments":"{\"q\":"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"completion_tokens_details":{"reasoning_tokens":1}}}"#,
            "[DONE]",
        ])
        .await;
        assert_eq!(
            events[0],
            StreamEvent::TextDelta {
                index: TEXT_INDEX,
                text: "hi".to_owned()
            }
        );
        let StreamEvent::ToolCallStart { index, name, .. } = &events[1] else {
            panic!("expected tool call start");
        };
        assert_eq!(*index, TOOL_BASE);
        assert_eq!(name, "lookup");
        let mut arguments = String::new();
        let mut fragment = None;
        for event in &events {
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
        assert!(events.contains(&StreamEvent::Finish(FinishReason::ToolCalls)));
        let usage = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::Usage(usage) => Some(usage),
                _ => None,
            })
            .expect("usage");
        assert_eq!(usage.input_tokens, Some(3));
        assert_eq!(usage.output_tokens, Some(2));
        assert_eq!(usage.reasoning_tokens, Some(1));
    }

    #[tokio::test]
    async fn reasoning_content_streams_as_full_reasoning() {
        let events = drain(&[
            r#"{"choices":[{"index":0,"delta":{"reasoning_content":"why"},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"because"},"finish_reason":"stop"}]}"#,
        ])
        .await;
        assert_eq!(
            events[0],
            StreamEvent::ReasoningStart {
                index: REASONING_INDEX,
                visibility: ReasoningVisibility::Full,
            }
        );
        assert_eq!(
            events[1],
            StreamEvent::ReasoningDelta {
                index: REASONING_INDEX,
                text: "why".to_owned(),
            }
        );
        assert_eq!(
            events[2],
            StreamEvent::TextDelta {
                index: TEXT_INDEX,
                text: "because".to_owned()
            }
        );
        assert_eq!(events[3], StreamEvent::Finish(FinishReason::Stop));
    }

    #[tokio::test]
    async fn audio_and_prediction_token_counters_are_preserved() {
        let events = drain(&[
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":2,"cache_write_tokens":1,"audio_tokens":3,"text_tokens":5,"image_tokens":2},"completion_tokens_details":{"reasoning_tokens":1,"audio_tokens":2,"text_tokens":1,"accepted_prediction_tokens":1,"rejected_prediction_tokens":0}}}"#,
        ])
        .await;
        let usage = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::Usage(usage) => Some(usage),
                _ => None,
            })
            .expect("usage");
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.audio_input_tokens, Some(3));
        assert_eq!(usage.audio_output_tokens, Some(2));
        assert_eq!(usage.cache_read_tokens, Some(2));
        assert_eq!(usage.cache_write_tokens, Some(1));
        assert_eq!(usage.extension.get("prompt_text_tokens"), Some(&5));
        assert_eq!(usage.extension.get("prompt_image_tokens"), Some(&2));
        assert_eq!(usage.extension.get("completion_text_tokens"), Some(&1));
        assert_eq!(usage.extension.get("accepted_prediction_tokens"), Some(&1));
        assert_eq!(usage.extension.get("rejected_prediction_tokens"), Some(&0));
    }

    #[tokio::test]
    async fn refusal_content_streams_as_a_refusal_part() {
        let events = drain(&[
            r#"{"choices":[{"index":0,"delta":{"refusal":"I can"},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"refusal":"not help"},"finish_reason":"stop"}]}"#,
        ])
        .await;
        assert_eq!(
            events[0],
            StreamEvent::RefusalDelta {
                index: REFUSAL_INDEX,
                text: "I can".to_owned(),
            }
        );
        assert_eq!(
            events[1],
            StreamEvent::RefusalDelta {
                index: REFUSAL_INDEX,
                text: "not help".to_owned(),
            }
        );
        assert_eq!(events[2], StreamEvent::Finish(FinishReason::Stop));
    }

    #[tokio::test]
    async fn the_legacy_function_call_shape_becomes_a_tool_call() {
        let events = drain(&[
            r#"{"choices":[{"index":0,"delta":{"function_call":{"name":"lookup","arguments":"{\"q\":"}},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"function_call":{"arguments":"1}"}},"finish_reason":"function_call"}]}"#,
        ])
        .await;
        let StreamEvent::ToolCallStart { index, name, .. } = &events[0] else {
            panic!("expected tool call start");
        };
        assert_eq!(*index, FUNCTION_CALL_INDEX);
        assert_eq!(name, "lookup");
        let arguments: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolCallDelta { arguments, .. } => Some(arguments.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(arguments, r#"{"q":1}"#);
        assert!(events.contains(&StreamEvent::Finish(FinishReason::ToolCalls)));
    }

    #[tokio::test]
    async fn streamed_audio_output_is_materialized_with_its_id_and_transcript() {
        let events = drain(&[
            r#"{"choices":[{"index":0,"delta":{"audio":{"id":"audio_1","transcript":"hi","data":"AQID"}},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"audio":{"transcript":" there","data":"BAU="}},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
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
        assert_eq!(metadata.transcript.as_deref(), Some("hi there"));
        assert_eq!(
            metadata.extension.get(AUDIO_ID_KEY).and_then(Value::as_str),
            Some("audio_1")
        );
        assert_eq!(
            events[4],
            StreamEvent::MediaChunk {
                index: AUDIO_INDEX,
                bytes: Vec::new(),
                last: true,
            }
        );
        assert_eq!(events[5], StreamEvent::Finish(FinishReason::Stop));
    }

    #[tokio::test]
    async fn inline_errors_surface_as_provider_errors() {
        let mut stream = map(
            stream_of(&[r#"{"error":{"code":"invalid_request_error","message":"bad"}}"#]),
            continuity(),
        );
        let error = stream.next().await.expect("some").expect_err("error");
        assert_eq!(error.provider_kind(), Some(ProviderErrorKind::Other));
    }

    #[tokio::test]
    async fn url_citation_annotations_become_citations() {
        let events = drain(&[
            r#"{"choices":[{"delta":{"content":"see ","annotations":[{"type":"url_citation","url_citation":{"url":"https://example.test","title":"Example","start_index":0,"end_index":3}}]}}]}"#,
        ])
        .await;
        let StreamEvent::TextDelta { index, text } = &events[0] else {
            panic!("expected a text delta");
        };
        assert_eq!(*index, TEXT_INDEX);
        assert_eq!(text, "see ");
        let StreamEvent::Citation { index, citation } = &events[1] else {
            panic!("expected a citation");
        };
        assert_eq!(*index, TEXT_INDEX);
        assert_eq!(
            citation.source,
            CitationSource::Url("https://example.test".to_owned())
        );
        assert_eq!(citation.title.as_deref(), Some("Example"));
    }

    #[tokio::test]
    async fn done_sentinel_is_ignored() {
        assert!(drain(&["[DONE]"]).await.is_empty());
    }
}
