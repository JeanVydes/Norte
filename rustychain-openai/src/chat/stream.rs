use std::collections::BTreeMap;

use futures_util::StreamExt;
use rustychain_core::{
    CallId, Citation, CitationSource, Error, FinishReason, ModelStream, ProviderError,
    ProviderErrorKind, ReasoningVisibility, Result, StreamEvent, Usage,
};
use rustychain_http::SseStream;
use serde_json::Value;

use crate::continuity::OpenAiContinuity;
use crate::json::opt_str;

const REASONING_INDEX: u32 = 0;
const TEXT_INDEX: u32 = 1;
const TOOL_BASE: u32 = 1000;

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

#[derive(Clone, Debug)]
struct Decoder {
    continuity: OpenAiContinuity,
    tools: BTreeMap<u32, ToolState>,
    reasoning_started: bool,
}

impl Decoder {
    fn new(continuity: OpenAiContinuity) -> Self {
        Self {
            continuity,
            tools: BTreeMap::new(),
            reasoning_started: false,
        }
    }

    fn decode(&mut self, data: &str) -> Result<Vec<StreamEvent>> {
        if data == "[DONE]" {
            return Ok(Vec::new());
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
        Ok(())
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
            if self.tools.get(&tool_index).is_some_and(|state| !state.named) {
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
    let mut mapped = Usage::default();
    mapped.input_tokens = usage.get("prompt_tokens").and_then(Value::as_u64);
    mapped.output_tokens = usage.get("completion_tokens").and_then(Value::as_u64);
    mapped.reasoning_tokens = usage
        .get("completion_tokens_details")
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64);
    mapped.cache_read_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64);
    mapped
}

fn stream_error(error: &Value) -> Error {
    let message = opt_str(error, "message").unwrap_or("openai stream error");
    let mut provider = ProviderError::new(ProviderErrorKind::Other, message.to_owned());
    if let Some(code) = opt_str(error, "code") {
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
        assert_eq!(events[0], StreamEvent::TextDelta { index: TEXT_INDEX, text: "hi".to_owned() });
        let StreamEvent::ToolCallStart { index, name, .. } = &events[1] else {
            panic!("expected tool call start");
        };
        assert_eq!(*index, TOOL_BASE);
        assert_eq!(name, "lookup");
        let mut arguments = String::new();
        let mut fragment = None;
        for event in &events {
            match event {
                StreamEvent::ToolCallDelta { arguments: delta, .. } => arguments.push_str(delta),
                StreamEvent::Continuity { fragment: value, .. } => fragment = Some(value.clone()),
                _ => {}
            }
        }
        assert_eq!(arguments, r#"{"q":1}"#);
        let decoded = continuity()
            .decode(&continuity().merge(None, fragment.expect("fragment")).expect("merge"))
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
        assert_eq!(events[2], StreamEvent::TextDelta { index: TEXT_INDEX, text: "because".to_owned() });
        assert_eq!(events[3], StreamEvent::Finish(FinishReason::Stop));
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
