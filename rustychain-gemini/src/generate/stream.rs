use futures_util::StreamExt;
use rustychain_core::{
    CallId, Citation, CitationSource, Error, ExtensionPart, FinishReason, MediaKind, MediaMetadata,
    MediaPart, MediaSource, MimeType, ModelStream, Part, ProviderError, ProviderErrorKind,
    ReasoningVisibility, Result, StreamEvent, Usage,
};
use rustychain_http::SseStream;
use serde_json::{Value, json};

use crate::continuity::GeminiContinuity;
use crate::json::{base64_decode, opt_str};

/// Maps the vendor `SSE` stream into core stream events.
pub(crate) fn map(sse: SseStream, continuity: GeminiContinuity) -> ModelStream {
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
    continuity: GeminiContinuity,
    next_index: u32,
    text_index: Option<u32>,
    reasoning_index: Option<u32>,
    text: String,
    reasoning: String,
    saw_tool_call: bool,
    finished: bool,
    pending_finish: Option<FinishReason>,
}

impl Decoder {
    fn new(continuity: GeminiContinuity) -> Self {
        Self {
            continuity,
            next_index: 0,
            text_index: None,
            reasoning_index: None,
            text: String::new(),
            reasoning: String::new(),
            saw_tool_call: false,
            finished: false,
            pending_finish: None,
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
        self.prompt_feedback(&value);
        self.candidates(&value, &mut events)?;
        if let Some(usage) = value.get("usageMetadata").filter(|usage| !usage.is_null()) {
            events.push(StreamEvent::Usage(usage_of(usage)));
        }
        if let Some(reason) = self.pending_finish.take() {
            events.push(StreamEvent::Finish(reason));
        }
        Ok(events)
    }

    fn prompt_feedback(&mut self, value: &Value) {
        let blocked = value
            .get("promptFeedback")
            .and_then(|feedback| opt_str(feedback, "blockReason"))
            .is_some_and(|reason| !reason.is_empty() && reason != "BLOCK_REASON_UNSPECIFIED");
        if blocked && !self.finished {
            self.finished = true;
            self.pending_finish = Some(FinishReason::ContentFilter);
        }
    }

    fn candidates(&mut self, value: &Value, events: &mut Vec<StreamEvent>) -> Result<()> {
        let candidates = value
            .get("candidates")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for candidate in candidates {
            if let Some(parts) = candidate
                .get("content")
                .and_then(|content| content.get("parts"))
                .and_then(Value::as_array)
            {
                for part in parts {
                    self.part(part, events)?;
                }
            }
            if let Some(reason) = opt_str(&candidate, "finishReason") {
                if !matches!(reason, "" | "FINISH_REASON_UNSPECIFIED") && !self.finished {
                    self.finished = true;
                    self.pending_finish = Some(self.finish_reason(reason));
                }
            }
            self.grounding(&candidate, events);
        }
        Ok(())
    }

    /// Surfaces search / maps grounding as neutral `URL` citations on the text
    /// part being streamed.
    fn grounding(&self, candidate: &Value, events: &mut Vec<StreamEvent>) {
        let Some(index) = self.text_index else {
            return;
        };
        let Some(chunks) = candidate
            .get("groundingMetadata")
            .and_then(|metadata| metadata.get("groundingChunks"))
            .and_then(Value::as_array)
        else {
            return;
        };
        let mut seen = std::collections::BTreeSet::new();
        for chunk in chunks {
            let context = ["web", "retrievedContext", "maps"]
                .iter()
                .find_map(|key| chunk.get(*key));
            let Some(context) = context else {
                continue;
            };
            let Some(uri) = opt_str(context, "uri") else {
                continue;
            };
            if !seen.insert(uri.to_owned()) {
                continue;
            }
            events.push(StreamEvent::Citation {
                index,
                citation: Citation {
                    source: CitationSource::Url(uri.to_owned()),
                    title: opt_str(context, "title").map(ToOwned::to_owned),
                },
            });
        }
    }

    fn part(&mut self, part: &Value, events: &mut Vec<StreamEvent>) -> Result<()> {
        if let Some(call) = part.get("functionCall").filter(|call| !call.is_null()) {
            return self.function_call(part, call, events);
        }
        if let Some(data) = part.get("inlineData").filter(|data| !data.is_null()) {
            return self.inline_data(data, events);
        }
        if let Some(data) = part.get("fileData").filter(|data| !data.is_null()) {
            return self.file_data(data, events);
        }
        if let Some(text) = opt_str(part, "text") {
            let thought = part
                .get("thought")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            return self.text(part, text, thought, events);
        }
        self.extension(part, events);
        Ok(())
    }

    /// Captures any part this decoder does not model — including server-side tool
    /// parts — as an opaque `Extension`, so unknown wire blocks round-trip
    /// unchanged (FR-ADP-3).
    fn extension(&mut self, part: &Value, events: &mut Vec<StreamEvent>) {
        if part.as_object().is_none_or(serde_json::Map::is_empty) {
            return;
        }
        let index = self.alloc();
        events.push(StreamEvent::Part {
            index,
            part: Part::Extension(ExtensionPart {
                namespace: "google".to_owned(),
                data: part.clone(),
            }),
        });
        self.break_text();
    }

    fn text(
        &mut self,
        part: &Value,
        text: &str,
        thought: bool,
        events: &mut Vec<StreamEvent>,
    ) -> Result<()> {
        let signature = opt_str(part, "thoughtSignature");
        if thought {
            let index = self.reasoning_index.unwrap_or_else(|| {
                let index = self.alloc();
                self.reasoning_index = Some(index);
                self.reasoning.clear();
                events.push(StreamEvent::ReasoningStart {
                    index,
                    visibility: ReasoningVisibility::Summary,
                });
                index
            });
            self.reasoning.push_str(text);
            events.push(StreamEvent::ReasoningDelta {
                index,
                text: text.to_owned(),
            });
            if let Some(signature) = signature {
                let wire = json!({
                    "text": self.reasoning,
                    "thought": true,
                    "thoughtSignature": signature,
                });
                events.push(StreamEvent::Continuity {
                    index,
                    fragment: self.continuity.fragment(&wire)?,
                });
            }
        } else {
            let index = self.text_index.unwrap_or_else(|| {
                let index = self.alloc();
                self.text_index = Some(index);
                self.text.clear();
                index
            });
            self.text.push_str(text);
            events.push(StreamEvent::TextDelta {
                index,
                text: text.to_owned(),
            });
            if let Some(signature) = signature {
                let wire = json!({ "text": self.text, "thoughtSignature": signature });
                events.push(StreamEvent::Continuity {
                    index,
                    fragment: self.continuity.fragment(&wire)?,
                });
            }
        }
        Ok(())
    }

    fn function_call(
        &mut self,
        part: &Value,
        call: &Value,
        events: &mut Vec<StreamEvent>,
    ) -> Result<()> {
        let index = self.alloc();
        let call_id = CallId::new();
        let name = opt_str(call, "name").unwrap_or_default().to_owned();
        let arguments = call.get("args").cloned().unwrap_or_else(|| json!({}));
        self.saw_tool_call = true;
        events.push(StreamEvent::ToolCallStart {
            index,
            call_id,
            name,
        });
        events.push(StreamEvent::ToolCallDelta {
            index,
            call_id,
            name: None,
            arguments: arguments.to_string(),
        });
        events.push(StreamEvent::Continuity {
            index,
            fragment: self.continuity.fragment(part)?,
        });
        events.push(StreamEvent::ToolCallDone { index });
        self.break_text();
        Ok(())
    }

    fn inline_data(&mut self, data: &Value, events: &mut Vec<StreamEvent>) -> Result<()> {
        let bytes = base64_decode(opt_str(data, "data").unwrap_or_default())?;
        let mime = mime_of(data)?;
        let index = self.alloc();
        events.push(StreamEvent::MediaStart {
            index,
            kind: media_kind(mime.as_str()),
            mime,
            metadata: MediaMetadata::default(),
        });
        events.push(StreamEvent::MediaChunk {
            index,
            bytes,
            last: true,
        });
        self.break_text();
        Ok(())
    }

    fn file_data(&mut self, data: &Value, events: &mut Vec<StreamEvent>) -> Result<()> {
        let uri = opt_str(data, "fileUri").unwrap_or_default().to_owned();
        let mime = mime_of(data)?;
        let index = self.alloc();
        let part = MediaPart::new(media_kind(mime.as_str()), MediaSource::Url(uri), mime);
        events.push(StreamEvent::Part {
            index,
            part: Part::Media(part),
        });
        self.break_text();
        Ok(())
    }

    fn alloc(&mut self) -> u32 {
        let index = self.next_index;
        self.next_index += 1;
        index
    }

    /// Closes any in-flight text or reasoning part so the next content starts a
    /// distinct part, matching the vendor's part ordering.
    fn break_text(&mut self) {
        self.text_index = None;
        self.text.clear();
        self.reasoning_index = None;
        self.reasoning.clear();
    }

    fn finish_reason(&self, reason: &str) -> FinishReason {
        if self.saw_tool_call {
            return FinishReason::ToolCalls;
        }
        match reason {
            "STOP" => FinishReason::Stop,
            "MAX_TOKENS" => FinishReason::Length,
            "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "LANGUAGE"
            | "IMAGE_SAFETY" => FinishReason::ContentFilter,
            other => FinishReason::Other(other.to_owned()),
        }
    }
}

fn mime_of(data: &Value) -> Result<MimeType> {
    MimeType::new(opt_str(data, "mimeType").unwrap_or("application/octet-stream"))
}

fn media_kind(mime: &str) -> MediaKind {
    if mime.starts_with("image/") {
        MediaKind::Image
    } else if mime.starts_with("audio/") {
        MediaKind::Audio
    } else if mime.starts_with("video/") {
        MediaKind::Video
    } else {
        MediaKind::Document
    }
}

fn usage_of(usage: &Value) -> Usage {
    let mut mapped = Usage::default();
    mapped.input_tokens = usage.get("promptTokenCount").and_then(Value::as_u64);
    mapped.output_tokens = usage.get("candidatesTokenCount").and_then(Value::as_u64);
    mapped.reasoning_tokens = usage.get("thoughtsTokenCount").and_then(Value::as_u64);
    mapped.cache_read_tokens = usage.get("cachedContentTokenCount").and_then(Value::as_u64);
    mapped
}

fn stream_error(error: &Value) -> Error {
    let message = opt_str(error, "message").unwrap_or("gemini stream error");
    let status = opt_str(error, "status");
    let code = error
        .get("code")
        .and_then(Value::as_u64)
        .and_then(|code| u16::try_from(code).ok());
    let kind = match (status, code) {
        (Some("RESOURCE_EXHAUSTED"), _) | (_, Some(429)) => ProviderErrorKind::RateLimited,
        (Some("UNAVAILABLE"), _) => ProviderErrorKind::ModelOverloaded,
        (Some("INTERNAL"), _) | (_, Some(500..=599)) => ProviderErrorKind::ServerError,
        (Some("INVALID_ARGUMENT" | "FAILED_PRECONDITION"), _) => ProviderErrorKind::InvalidRequest,
        (Some("UNAUTHENTICATED"), _) => ProviderErrorKind::Authentication,
        (Some("PERMISSION_DENIED"), _) => ProviderErrorKind::Permission,
        (Some("NOT_FOUND"), _) => ProviderErrorKind::NotFound,
        _ => ProviderErrorKind::Other,
    };
    let mut provider = ProviderError::new(kind, message.to_owned());
    if let Some(status) = status {
        provider = provider.with_code(status.to_owned());
    }
    if let Some(code) = code {
        provider = provider.with_status(code);
    }
    Error::provider(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use rustychain_core::{ContinuityAdapter, ModelId, ModelRef, ProviderId, WireFamily};
    use rustychain_http::SseEvent;

    use crate::WIRE_GENERATE;

    fn continuity() -> GeminiContinuity {
        let model = ModelRef::new(
            ProviderId::new("google").expect("provider"),
            ModelId::new("gemini-2.5-pro").expect("model"),
        );
        GeminiContinuity::new(WireFamily::new(WIRE_GENERATE).expect("family"), model)
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

    fn decode_continuity(events: &[StreamEvent]) -> Value {
        let fragment = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::Continuity { fragment, .. } => Some(fragment.clone()),
                _ => None,
            })
            .expect("continuity");
        let token = continuity().merge(None, fragment).expect("merge");
        continuity().decode(&token).expect("decode")
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
            "../../../rustychain-testing/fixtures/gemini.generate.json"
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
        let usage = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::Usage(usage) => Some(usage),
                _ => None,
            })
            .expect("usage");
        assert_eq!(usage.input_tokens, Some(7));
        assert_eq!(usage.output_tokens, Some(2));
        assert_eq!(usage.cache_read_tokens, Some(3));
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finish(FinishReason::Stop))
        );
    }

    #[tokio::test]
    async fn text_deltas_and_usage_survive_the_decode() {
        let events = drain(&[
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hel"}]}}]}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"lo"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":2,"cachedContentTokenCount":3}}"#,
        ])
        .await;
        assert_eq!(
            events[0],
            StreamEvent::TextDelta {
                index: 0,
                text: "Hel".to_owned(),
            }
        );
        assert_eq!(
            events[1],
            StreamEvent::TextDelta {
                index: 0,
                text: "lo".to_owned(),
            }
        );
        let StreamEvent::Usage(usage) = &events[2] else {
            panic!("expected usage");
        };
        assert_eq!(usage.input_tokens, Some(7));
        assert_eq!(usage.output_tokens, Some(2));
        assert_eq!(usage.cache_read_tokens, Some(3));
        assert_eq!(events[3], StreamEvent::Finish(FinishReason::Stop));
    }

    #[tokio::test]
    async fn function_calls_keep_their_signature() {
        let events = drain(&[
            r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"lookup","args":{"q":1}},"thoughtSignature":"sig"}]},"finishReason":"STOP"}]}"#,
        ])
        .await;
        let StreamEvent::ToolCallStart { index, name, .. } = &events[0] else {
            panic!("expected tool call start");
        };
        assert_eq!(*index, 0);
        assert_eq!(name, "lookup");
        let args = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::ToolCallDelta { arguments, .. } => Some(arguments.clone()),
                _ => None,
            })
            .expect("arguments");
        assert_eq!(args, r#"{"q":1}"#);
        let decoded = decode_continuity(&events);
        assert_eq!(decoded["functionCall"]["name"], "lookup");
        assert_eq!(decoded["thoughtSignature"], "sig");
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finish(FinishReason::ToolCalls))
        );
    }

    #[tokio::test]
    async fn thought_summaries_stream_then_settle_with_continuity() {
        let events = drain(&[
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"plan","thought":true}]}}]}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"the answer","thoughtSignature":"sig"}]},"finishReason":"STOP"}]}"#,
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
                text: "plan".to_owned(),
            }
        );
        assert_eq!(
            events[2],
            StreamEvent::TextDelta {
                index: 1,
                text: "the answer".to_owned(),
            }
        );
        assert!(matches!(
            &events[3],
            StreamEvent::Continuity { index: 1, .. }
        ));
    }

    #[tokio::test]
    async fn text_signature_is_stored_with_the_whole_text() {
        let events = drain(&[
            r#"{"candidates":[{"content":{"parts":[{"text":"an "}]}}]}"#,
            r#"{"candidates":[{"content":{"parts":[{"text":"answer","thoughtSignature":"sig"}]}}]}"#,
        ])
        .await;
        let decoded = decode_continuity(&events);
        assert_eq!(decoded["text"], "an answer");
        assert_eq!(decoded["thoughtSignature"], "sig");
    }

    #[tokio::test]
    async fn inline_media_is_materialized() {
        let events = drain(&[
            r#"{"candidates":[{"content":{"parts":[{"inlineData":{"mimeType":"image/png","data":"AQID"}}]}}]}"#,
        ])
        .await;
        let StreamEvent::MediaStart {
            index, kind, mime, ..
        } = &events[0]
        else {
            panic!("expected media start");
        };
        assert_eq!(*index, 0);
        assert_eq!(*kind, MediaKind::Image);
        assert_eq!(mime.as_str(), "image/png");
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
    async fn vendor_file_output_is_a_url_for_the_engine_to_ingest() {
        let events = drain(&[
            r#"{"candidates":[{"content":{"parts":[{"fileData":{"mimeType":"image/jpeg","fileUri":"https://example.test/a.jpg"}}]}}]}"#,
        ])
        .await;
        let StreamEvent::Part {
            index,
            part: Part::Media(media),
        } = &events[0]
        else {
            panic!("expected media part");
        };
        assert_eq!(*index, 0);
        assert_eq!(media.kind, MediaKind::Image);
        assert_eq!(
            media.source,
            MediaSource::Url("https://example.test/a.jpg".to_owned())
        );
    }

    #[tokio::test]
    async fn server_side_tool_parts_round_trip_as_extensions() {
        let events = drain(&[
            r#"{"candidates":[{"content":{"parts":[{"executableCode":{"language":"PYTHON","code":"1+1"}}]}}]}"#,
        ])
        .await;
        let StreamEvent::Part {
            part: Part::Extension(extension),
            ..
        } = &events[0]
        else {
            panic!("expected extension part");
        };
        assert_eq!(extension.namespace, "google");
        assert_eq!(extension.data["executableCode"]["code"], "1+1");
    }

    #[tokio::test]
    async fn unknown_parts_round_trip_as_extensions() {
        let events = drain(&[
            r#"{"candidates":[{"content":{"parts":[{"vendorExtension":{"payload":[1,2,3]}}]}}]}"#,
        ])
        .await;
        let StreamEvent::Part {
            part: Part::Extension(extension),
            ..
        } = &events[0]
        else {
            panic!("expected extension part");
        };
        assert_eq!(extension.namespace, "google");
        assert_eq!(
            extension.data["vendorExtension"]["payload"],
            json!([1, 2, 3])
        );
    }

    #[tokio::test]
    async fn empty_parts_are_ignored() {
        let events = drain(&[r#"{"candidates":[{"content":{"parts":[{}]}}]}"#]).await;
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn blocked_prompts_finish_as_content_filter() {
        let events = drain(&[r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#]).await;
        assert_eq!(
            events,
            vec![StreamEvent::Finish(FinishReason::ContentFilter)]
        );
    }

    #[tokio::test]
    async fn stream_errors_are_classified() {
        let mut stream = map(
            stream_of(&[
                r#"{"error":{"code":429,"message":"quota","status":"RESOURCE_EXHAUSTED"}}"#,
            ]),
            continuity(),
        );
        let error = stream.next().await.expect("some").expect_err("error");
        assert_eq!(error.provider_kind(), Some(ProviderErrorKind::RateLimited));
    }

    #[tokio::test]
    async fn grounding_metadata_becomes_url_citations() {
        let events = drain(&[
            r#"{"candidates":[{"content":{"parts":[{"text":"Rust is fast"}]},"groundingMetadata":{"groundingChunks":[{"web":{"uri":"https://example.test/rust","title":"Rust"}}],"groundingSupports":[{"segment":{"startIndex":0,"endIndex":4},"groundingChunkIndices":[0]}]},"finishReason":"STOP"}]}"#,
        ])
        .await;
        let StreamEvent::Citation { index, citation } = &events[1] else {
            panic!("expected a citation");
        };
        assert_eq!(*index, 0);
        assert_eq!(
            citation.source,
            CitationSource::Url("https://example.test/rust".to_owned())
        );
        assert_eq!(citation.title.as_deref(), Some("Rust"));
    }

    #[tokio::test]
    async fn done_and_empty_chunks_are_ignored() {
        let events = drain(&["[DONE]", r#"{"candidates":[]}"#]).await;
        assert!(events.is_empty());
    }
}
