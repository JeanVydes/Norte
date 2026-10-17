use std::collections::BTreeMap;

use futures_util::StreamExt;
use rustychain_core::{
    CallId, Citation, CitationSource, Error, ExtensionPart, FinishReason, ModelStream, Part,
    ProviderError, ProviderErrorKind, ReasoningVisibility, Result, StreamEvent, Usage,
};
use rustychain_http::SseStream;
use serde_json::Value;

use crate::continuity::AnthropicContinuity;
use crate::json::{opt_str, str_field, u32_field};

/// Maps the vendor `SSE` stream into core stream events.
pub(crate) fn map(sse: SseStream, continuity: AnthropicContinuity) -> ModelStream {
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
struct ReasoningState {
    text: String,
    signature: String,
    redacted: bool,
}

#[derive(Clone, Debug)]
struct Decoder {
    continuity: AnthropicContinuity,
    calls: BTreeMap<u32, CallId>,
    reasoning: BTreeMap<u32, ReasoningState>,
    opaque: BTreeMap<u32, (Value, String)>,
    usage: Usage,
}

impl Decoder {
    fn new(continuity: AnthropicContinuity) -> Self {
        Self {
            continuity,
            calls: BTreeMap::new(),
            reasoning: BTreeMap::new(),
            opaque: BTreeMap::new(),
            usage: Usage::default(),
        }
    }

    fn decode(&mut self, data: &str) -> Result<Vec<StreamEvent>> {
        let value: Value = serde_json::from_str(data)?;
        match str_field(&value, "type")? {
            "message_start" => Ok(self.message_start(&value)),
            "content_block_start" => self.block_start(&value),
            "content_block_delta" => self.block_delta(&value),
            "content_block_stop" => self.block_stop(&value),
            "message_delta" => Ok(self.message_delta(&value)),
            "error" => Err(stream_error(&value)),
            _ => Ok(Vec::new()),
        }
    }

    fn message_start(&mut self, value: &Value) -> Vec<StreamEvent> {
        let message = value.get("message").unwrap_or(value);
        if let Some(usage) = message.get("usage") {
            self.merge_usage(usage);
        }
        vec![StreamEvent::Usage(self.usage.clone())]
    }

    fn block_start(&mut self, value: &Value) -> Result<Vec<StreamEvent>> {
        let index = u32_field(value, "index")?;
        let block = value
            .get("content_block")
            .ok_or_else(|| Error::Codec("anthropic wire: missing content_block".to_owned()))?;
        match opt_str(block, "type") {
            Some("text") => {
                let mut events = vec![StreamEvent::TextStart { index }];
                events.extend(block_citations(index, block));
                Ok(events)
            }
            Some("thinking") => {
                let state = ReasoningState {
                    text: opt_str(block, "thinking").unwrap_or_default().to_owned(),
                    signature: opt_str(block, "signature").unwrap_or_default().to_owned(),
                    redacted: false,
                };
                self.reasoning.insert(index, state);
                Ok(vec![StreamEvent::ReasoningStart {
                    index,
                    visibility: ReasoningVisibility::Full,
                }])
            }
            Some("redacted_thinking") => {
                let state = ReasoningState {
                    text: String::new(),
                    signature: opt_str(block, "data").unwrap_or_default().to_owned(),
                    redacted: true,
                };
                self.reasoning.insert(index, state);
                Ok(vec![StreamEvent::ReasoningStart {
                    index,
                    visibility: ReasoningVisibility::Redacted,
                }])
            }
            Some("tool_use") => self.tool_use(index, block),
            _ => {
                self.opaque.insert(index, (block.clone(), String::new()));
                Ok(Vec::new())
            }
        }
    }

    fn tool_use(&mut self, index: u32, block: &Value) -> Result<Vec<StreamEvent>> {
        let call_id = CallId::new();
        self.calls.insert(index, call_id);
        let name = opt_str(block, "name").unwrap_or_default().to_owned();
        let mut events = vec![StreamEvent::ToolCallStart {
            index,
            call_id,
            name,
        }];
        if let Some(vendor) = opt_str(block, "id") {
            events.push(StreamEvent::Continuity {
                index,
                fragment: self
                    .continuity
                    .fragment(&serde_json::json!({ "tool_use_id": vendor }))?,
            });
        }
        Ok(events)
    }

    fn block_delta(&mut self, value: &Value) -> Result<Vec<StreamEvent>> {
        let index = u32_field(value, "index")?;
        let delta = value
            .get("delta")
            .ok_or_else(|| Error::Codec("anthropic wire: missing delta".to_owned()))?;
        match opt_str(delta, "type") {
            Some("text_delta") => Ok(vec![StreamEvent::TextDelta {
                index,
                text: str_field(delta, "text")?.to_owned(),
            }]),
            Some("input_json_delta") => {
                let partial = str_field(delta, "partial_json")?.to_owned();
                if let Some(call_id) = self.calls.get(&index).copied() {
                    Ok(vec![StreamEvent::ToolCallDelta {
                        index,
                        call_id,
                        name: None,
                        arguments: partial,
                    }])
                } else if let Some((_, buffer)) = self.opaque.get_mut(&index) {
                    buffer.push_str(&partial);
                    Ok(Vec::new())
                } else {
                    Ok(Vec::new())
                }
            }
            Some("citations_delta") => {
                let citation = delta.get("citation").ok_or_else(|| {
                    Error::Codec("anthropic wire: citations_delta has no citation".to_owned())
                })?;
                Ok(vec![StreamEvent::Citation {
                    index,
                    citation: citation_of(citation),
                }])
            }
            Some("thinking_delta") => {
                let text = str_field(delta, "thinking")?.to_owned();
                if let Some(state) = self.reasoning.get_mut(&index) {
                    state.text.push_str(&text);
                }
                Ok(vec![StreamEvent::ReasoningDelta { index, text }])
            }
            Some("signature_delta") => {
                let signature = str_field(delta, "signature")?.to_owned();
                if let Some(state) = self.reasoning.get_mut(&index) {
                    if !state.redacted {
                        state.signature.push_str(&signature);
                    }
                }
                Ok(Vec::new())
            }
            _ => Ok(Vec::new()),
        }
    }

    fn block_stop(&mut self, value: &Value) -> Result<Vec<StreamEvent>> {
        let index = u32_field(value, "index")?;
        if let Some(state) = self.reasoning.remove(&index) {
            let block = if state.redacted {
                serde_json::json!({ "type": "redacted_thinking", "data": state.signature })
            } else {
                serde_json::json!({
                    "type": "thinking",
                    "thinking": state.text,
                    "signature": state.signature,
                })
            };
            return Ok(vec![StreamEvent::Continuity {
                index,
                fragment: self.continuity.fragment(&block)?,
            }]);
        }
        if let Some((mut block, buffer)) = self.opaque.remove(&index) {
            if !buffer.is_empty() {
                if let Ok(input) = serde_json::from_str::<Value>(&buffer) {
                    block["input"] = input;
                }
            }
            return Ok(vec![StreamEvent::Part {
                index,
                part: Part::Extension(ExtensionPart {
                    namespace: "anthropic".to_owned(),
                    data: block,
                }),
            }]);
        }
        if self.calls.remove(&index).is_some() {
            return Ok(vec![StreamEvent::ToolCallDone { index }]);
        }
        Ok(Vec::new())
    }

    fn message_delta(&mut self, value: &Value) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if let Some(usage) = value.get("usage") {
            self.merge_usage(usage);
            events.push(StreamEvent::Usage(self.usage.clone()));
        }
        if let Some(reason) = value
            .get("delta")
            .and_then(|delta| opt_str(delta, "stop_reason"))
        {
            events.push(StreamEvent::Finish(finish_reason(reason)));
        }
        events
    }

    fn merge_usage(&mut self, usage: &Value) {
        set(&mut self.usage.input_tokens, usage, "input_tokens");
        set(&mut self.usage.output_tokens, usage, "output_tokens");
        set(
            &mut self.usage.cache_read_tokens,
            usage,
            "cache_read_input_tokens",
        );
        set(
            &mut self.usage.cache_write_tokens,
            usage,
            "cache_creation_input_tokens",
        );
    }
}

/// Extracts the citations a text block already carries at its start.
fn block_citations(index: u32, block: &Value) -> Vec<StreamEvent> {
    block
        .get("citations")
        .and_then(Value::as_array)
        .map(|citations| {
            citations
                .iter()
                .map(|citation| StreamEvent::Citation {
                    index,
                    citation: citation_of(citation),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Maps one vendor citation onto the neutral [`Citation`].
fn citation_of(citation: &Value) -> Citation {
    Citation {
        source: citation_source(citation),
        title: opt_str(citation, "document_title")
            .or_else(|| opt_str(citation, "title"))
            .map(ToOwned::to_owned),
    }
}

/// Maps a vendor citation location onto the closest neutral source, falling back
/// to the verbatim payload for a location the neutral model does not structure.
fn citation_source(citation: &Value) -> CitationSource {
    match opt_str(citation, "type").unwrap_or_default() {
        "char_location" => range_source(citation, "start_char_index", "end_char_index"),
        "page_location" => range_source(citation, "start_page_number", "end_page_number"),
        "content_block_location" => range_source(citation, "start_block_index", "end_block_index"),
        "search_result_location" => opt_str(citation, "source").map_or_else(
            || extension_source(citation),
            |url| CitationSource::Url(url.to_owned()),
        ),
        _ => extension_source(citation),
    }
}

fn range_source(citation: &Value, start: &str, end: &str) -> CitationSource {
    let start = citation
        .get(start)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let end = citation
        .get(end)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    match (start, end) {
        (Some(start), Some(end)) => CitationSource::Range { start, end },
        _ => extension_source(citation),
    }
}

fn extension_source(citation: &Value) -> CitationSource {
    CitationSource::Extension {
        namespace: "anthropic".to_owned(),
        data: citation.clone(),
    }
}

fn set(target: &mut Option<u64>, usage: &Value, key: &str) {
    if let Some(value) = usage.get(key).and_then(Value::as_u64) {
        *target = Some(value);
    }
}

fn finish_reason(reason: &str) -> FinishReason {
    match reason {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "max_tokens" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        "refusal" => FinishReason::Refusal,
        other => FinishReason::Other(other.to_owned()),
    }
}

fn stream_error(value: &Value) -> Error {
    let error = value.get("error").unwrap_or(value);
    let message = opt_str(error, "message").unwrap_or("anthropic stream error");
    let mut provider = ProviderError::new(ProviderErrorKind::Other, message.to_owned());
    if let Some(code) = opt_str(error, "type") {
        provider = provider.with_code(code.to_owned());
    }
    Error::provider(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use rustychain_core::{ContinuityAdapter, ModelId, ModelRef, ProviderId, WireFamily};
    use rustychain_http::SseEvent;

    use crate::WIRE_MESSAGES;

    fn continuity() -> AnthropicContinuity {
        let model = ModelRef::new(
            ProviderId::new("anthropic").expect("provider"),
            ModelId::new("claude-sonnet-4").expect("model"),
        );
        AnthropicContinuity::new(WireFamily::new(WIRE_MESSAGES).expect("family"), model)
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
            "../../../rustychain-testing/fixtures/anthropic.messages.json"
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
        // Anthropic streams usage cumulatively: input/cache arrive on
        // `message_start`, output on `message_delta`. Fold the last Some per field.
        let mut input = None;
        let mut cache_read = None;
        let mut output = None;
        for event in &events {
            if let StreamEvent::Usage(usage) = event {
                input = usage.input_tokens.or(input);
                cache_read = usage.cache_read_tokens.or(cache_read);
                output = usage.output_tokens.or(output);
            }
        }
        assert_eq!(input, Some(10));
        assert_eq!(cache_read, Some(4));
        assert_eq!(output, Some(2));
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finish(FinishReason::Stop))
        );
    }

    #[tokio::test]
    async fn text_and_usage_decode_with_cumulative_accounting() {
        let events = drain(&[
            r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":4}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
            r#"{"type":"message_stop"}"#,
        ])
        .await;
        assert_eq!(events[1], StreamEvent::TextStart { index: 0 });
        assert_eq!(
            events[2],
            StreamEvent::TextDelta {
                index: 0,
                text: "Hel".to_owned()
            }
        );
        assert_eq!(
            events[3],
            StreamEvent::TextDelta {
                index: 0,
                text: "lo".to_owned()
            }
        );
        let usage = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::Usage(usage) => Some(usage),
                _ => None,
            })
            .expect("usage");
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.cache_read_tokens, Some(4));
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finish(FinishReason::Stop))
        );
    }

    #[tokio::test]
    async fn tool_use_streams_arguments_and_keeps_the_vendor_id() {
        let events = drain(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"lookup"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"1}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}"#,
        ])
        .await;
        let StreamEvent::ToolCallStart { name, .. } = &events[0] else {
            panic!("expected tool call start");
        };
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
        assert_eq!(decoded["tool_use_id"], "toolu_1");
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finish(FinishReason::ToolCalls))
        );
    }

    #[tokio::test]
    async fn thinking_settles_into_a_replayable_block() {
        let events = drain(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"why"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
        ])
        .await;
        assert_eq!(
            events[0],
            StreamEvent::ReasoningStart {
                index: 0,
                visibility: ReasoningVisibility::Full,
            }
        );
        assert_eq!(
            events[1],
            StreamEvent::ReasoningDelta {
                index: 0,
                text: "why".to_owned()
            }
        );
        let StreamEvent::Continuity { fragment, .. } = &events[2] else {
            panic!("expected continuity");
        };
        let decoded = continuity()
            .decode(&continuity().merge(None, fragment.clone()).expect("merge"))
            .expect("decode");
        assert_eq!(decoded["thinking"], "why");
        assert_eq!(decoded["signature"], "sig");
    }

    #[tokio::test]
    async fn unknown_blocks_round_trip_as_extensions() {
        let events = drain(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_1","name":"web_search","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"rust\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
        ])
        .await;
        let StreamEvent::Part { part, .. } = &events[0] else {
            panic!("expected an extension part");
        };
        let Part::Extension(extension) = part else {
            panic!("expected an extension");
        };
        assert_eq!(extension.namespace, "anthropic");
        assert_eq!(extension.data["type"], "server_tool_use");
        assert_eq!(extension.data["input"]["query"], "rust");
    }

    #[tokio::test]
    async fn char_citations_become_neutral_ranges() {
        let events = drain(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Rust is fast"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"citations_delta","citation":{"type":"char_location","cited_text":"Rust","document_index":0,"document_title":"intro.txt","start_char_index":0,"end_char_index":4}}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
        ])
        .await;
        let StreamEvent::Citation { index, citation } = &events[2] else {
            panic!("expected a citation");
        };
        assert_eq!(*index, 0);
        assert_eq!(citation.source, CitationSource::Range { start: 0, end: 4 });
        assert_eq!(citation.title.as_deref(), Some("intro.txt"));
    }

    #[tokio::test]
    async fn search_result_citations_become_urls() {
        let events = drain(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"citations_delta","citation":{"type":"search_result_location","cited_text":"x","source":"https://example.test","title":"Example"}}}"#,
        ])
        .await;
        let StreamEvent::Citation { citation, .. } = &events[1] else {
            panic!("expected a citation");
        };
        assert_eq!(
            citation.source,
            CitationSource::Url("https://example.test".to_owned())
        );
        assert_eq!(citation.title.as_deref(), Some("Example"));
    }

    #[tokio::test]
    async fn unknown_citations_keep_the_vendor_payload() {
        let events = drain(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"citations_delta","citation":{"type":"future_location","token":"abc"}}}"#,
        ])
        .await;
        let StreamEvent::Citation { citation, .. } = &events[1] else {
            panic!("expected a citation");
        };
        let CitationSource::Extension { namespace, data } = &citation.source else {
            panic!("expected an extension source");
        };
        assert_eq!(namespace, "anthropic");
        assert_eq!(data["token"], "abc");
    }

    #[tokio::test]
    async fn failures_surface_as_provider_errors() {
        let mut stream = map(
            stream_of(&[
                r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#,
            ]),
            continuity(),
        );
        let error = stream.next().await.expect("some").expect_err("error");
        assert_eq!(error.provider_kind(), Some(ProviderErrorKind::Other));
    }
}
