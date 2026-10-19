use norte_core::{
    CallId, Error, MediaPart, MediaSource, Message, ModelRef, ModelRequest, Part, ReasoningEffort,
    ReasoningRequest, Result, Role, ToolKind, ToolResultPart, ToolSpec, splice_vendor_extensions,
};
use serde_json::{Value, json};

use crate::WIRE_GENERATE;
use crate::continuity::GeminiContinuity;
use crate::json::{base64_encode, opt_str};

/// Builds the wire body for one `generateContent` request.
///
/// # Errors
///
/// Returns an error when a part cannot be expressed on this wire, for example a
/// blob that no [`MediaAdapter`] resolved.
///
/// [`MediaAdapter`]: norte_core::MediaAdapter
pub(crate) fn build(
    model: &ModelRef,
    continuity: &GeminiContinuity,
    request: &ModelRequest,
) -> Result<Value> {
    build_body(model, continuity, request, true)
}

/// Builds the wire body for a `countTokens` call, which accepts the prompt but
/// no generation configuration.
///
/// # Errors
///
/// Returns an error when a part cannot be expressed on this wire.
pub(crate) fn build_count(
    model: &ModelRef,
    continuity: &GeminiContinuity,
    request: &ModelRequest,
) -> Result<Value> {
    build_body(model, continuity, request, false)
}

/// Builds the wire body that creates an explicit cached content: the prompt
/// (system instruction, contents and tools) plus the resource `model` and
/// `ttl`. It deliberately carries no generation configuration.
///
/// # Errors
///
/// Returns an error when a part cannot be expressed on this wire.
pub(crate) fn build_cache(
    model: &ModelRef,
    continuity: &GeminiContinuity,
    request: &ModelRequest,
    ttl: &str,
) -> Result<Value> {
    let mut body = build_body(model, continuity, request, false)?;
    let object = body
        .as_object_mut()
        .ok_or_else(|| Error::Codec("gemini wire: cache body is not an object".to_owned()))?;
    object.insert("model".to_owned(), Value::String(qualified_model(model)));
    object.insert("ttl".to_owned(), Value::String(ttl.to_owned()));
    Ok(body)
}

/// The fully qualified `models/...` resource name a request body requires.
fn qualified_model(model: &ModelRef) -> String {
    let name = model.model.as_str();
    if name.starts_with("models/") {
        name.to_owned()
    } else {
        format!("models/{name}")
    }
}

fn build_body(
    model: &ModelRef,
    continuity: &GeminiContinuity,
    request: &ModelRequest,
    generation: bool,
) -> Result<Value> {
    let instructions = instruction_count(&request.messages);
    let names = call_names(continuity, &request.messages);

    let mut body = serde_json::Map::new();
    if instructions > 0 {
        let text: Vec<String> = request.messages[..instructions]
            .iter()
            .map(Message::text)
            .filter(|text| !text.is_empty())
            .collect();
        if !text.is_empty() {
            body.insert(
                "systemInstruction".to_owned(),
                json!({ "parts": [{ "text": text.join("\n\n") }] }),
            );
        }
    }
    body.insert(
        "contents".to_owned(),
        Value::Array(build_contents(
            model,
            continuity,
            &names,
            &request.messages[instructions..],
        )?),
    );
    if !request.tools.is_empty() {
        let tools = build_tools(model, &request.tools)?;
        if !tools.is_empty() {
            body.insert("tools".to_owned(), Value::Array(tools));
        }
    }
    if generation {
        let config = generation_config(request);
        if !config.is_empty() {
            body.insert("generationConfig".to_owned(), Value::Object(config));
        }
    }
    splice_vendor_extensions(model, WIRE_GENERATE, &request.extensions, &mut body)?;
    Ok(Value::Object(body))
}

fn instruction_count(messages: &[Message]) -> usize {
    messages
        .iter()
        .take_while(|message| message.role.is_instruction())
        .count()
}

/// The Gemini role policy (FR-ROLE-2).
///
/// Leading `System`/`Developer` messages become `systemInstruction`. A
/// `System`/`Developer` message that appears mid-conversation is rendered as a
/// `user` content and merged with an adjacent `user` content, since Gemini's
/// `contents` only knows the `user` and `model` roles. This is how a mid-run
/// instruction update reaches the model.
fn build_contents(
    model: &ModelRef,
    continuity: &GeminiContinuity,
    names: &CallNames,
    messages: &[Message],
) -> Result<Vec<Value>> {
    let mut out: Vec<Value> = Vec::new();
    for message in messages {
        let role = if matches!(message.role, Role::Assistant) {
            "model"
        } else {
            "user"
        };
        let mut parts = Vec::new();
        for part in &message.parts {
            push_part(model, continuity, names, part, &mut parts)?;
        }
        push_content(role, parts, &mut out);
    }
    Ok(out)
}

fn push_content(role: &str, parts: Vec<Value>, out: &mut Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.get("role").and_then(Value::as_str) == Some(role) {
            if let Some(array) = last.get_mut("parts").and_then(Value::as_array_mut) {
                array.extend(parts);
                return;
            }
        }
    }
    out.push(json!({ "role": role, "parts": parts }));
}

fn push_part(
    model: &ModelRef,
    continuity: &GeminiContinuity,
    names: &CallNames,
    part: &Part,
    out: &mut Vec<Value>,
) -> Result<()> {
    match part {
        Part::Reasoning(reasoning) => {
            replay(continuity, reasoning.continuity.as_ref(), out)?;
        }
        Part::Text(text) => {
            if !replay(continuity, text.continuity.as_ref(), out)? {
                out.push(json!({ "text": text.text }));
            }
        }
        Part::Refusal(refusal) => out.push(json!({ "text": refusal.text })),
        Part::Media(media) => out.push(media_part(media)?),
        Part::ToolCall(call) => {
            if !replay(continuity, call.continuity.as_ref(), out)? {
                out.push(json!({
                    "functionCall": { "name": call.name, "args": call.arguments },
                }));
            }
        }
        Part::ToolResult(result) => out.push(tool_response(model, names, result)?),
        Part::ProviderToolCall(call) => {
            replay(continuity, call.continuity.as_ref(), out)?;
        }
        Part::ProviderToolResult(result) => {
            replay(continuity, result.continuity.as_ref(), out)?;
        }
        Part::Extension(extension) if extension.namespace.starts_with("google") => {
            out.push(extension.data.clone());
        }
        _ => {}
    }
    Ok(())
}

/// Appends the vendor part a continuity token carries, telling the caller
/// whether it did.
///
/// # Errors
///
/// Returns a codec error when the payload is not valid `JSON`.
fn replay(
    continuity: &GeminiContinuity,
    token: Option<&norte_core::ContinuityToken>,
    out: &mut Vec<Value>,
) -> Result<bool> {
    let Some(token) = token else {
        return Ok(false);
    };
    out.push(continuity.decode(token)?);
    Ok(true)
}

type CallNames = std::collections::BTreeMap<CallId, String>;

fn call_names(continuity: &GeminiContinuity, messages: &[Message]) -> CallNames {
    let mut names = CallNames::new();
    for message in messages {
        for part in &message.parts {
            let Part::ToolCall(call) = part else {
                continue;
            };
            let Some(token) = &call.continuity else {
                continue;
            };
            let Ok(value) = continuity.decode(token) else {
                continue;
            };
            let name = opt_str(&value, "name")
                .map(ToOwned::to_owned)
                .or_else(|| {
                    value
                        .get("functionCall")
                        .and_then(|call| opt_str(call, "name"))
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| call.name.clone());
            names.insert(call.call_id, name);
        }
    }
    names
}

fn tool_response(model: &ModelRef, names: &CallNames, result: &ToolResultPart) -> Result<Value> {
    let name = names
        .get(&result.call_id)
        .cloned()
        .unwrap_or_else(|| result.call_id.to_string());
    let text: String = result.parts.iter().filter_map(Part::as_text).collect();
    let carries_text = result.parts.iter().any(|part| part.as_text().is_some());
    // Text is authoritative (FR-TOOL-BASH-3): the host renders the text the
    // model reads, so structured data only answers for a result that carries no
    // text at all.
    let response = match (carries_text, &result.structured) {
        (false, Some(structured)) => json!({ "result": structured }),
        _ => json!({ "result": text }),
    };
    let mut media = Vec::new();
    for part in &result.parts {
        let Part::Media(media_part) = part else {
            continue;
        };
        match &media_part.source {
            MediaSource::Inline(bytes) => media.push(json!({
                "inlineData": {
                    "mimeType": media_part.mime.as_str(),
                    "data": base64_encode(bytes),
                }
            })),
            MediaSource::Blob(_) => {
                return Err(Error::InvalidInput(
                    "gemini: a media part is still stored as a blob; configure a MediaAdapter"
                        .to_owned(),
                ));
            }
            _ => {
                // A function response part only carries inline bytes on this
                // wire; a URL or a vendor file is a typed capability error
                // rather than a silent omission.
                return Err(Error::Capability {
                    model: model.clone(),
                    feature: "media from a URL or provider file in a tool result on the \
                              generateContent wire"
                        .to_owned(),
                });
            }
        }
    }
    let mut function_response = json!({ "name": name, "response": response });
    if !media.is_empty() {
        function_response["parts"] = Value::Array(media);
    }
    Ok(json!({ "functionResponse": function_response }))
}

fn media_part(media: &MediaPart) -> Result<Value> {
    match &media.source {
        MediaSource::Inline(bytes) => Ok(json!({
            "inlineData": {
                "mimeType": media.mime.as_str(),
                "data": base64_encode(bytes),
            }
        })),
        MediaSource::Url(url) => Ok(json!({
            "fileData": { "mimeType": media.mime.as_str(), "fileUri": url }
        })),
        MediaSource::ProviderFile(reference) => Ok(json!({
            "fileData": { "mimeType": media.mime.as_str(), "fileUri": reference.file_id }
        })),
        _ => Err(Error::InvalidInput(
            "gemini: a media part is still stored as a blob; configure a MediaAdapter".to_owned(),
        )),
    }
}

/// Builds the `tools` array entries: one `functionDeclarations` entry for the
/// host-executed functions plus one entry per provider tool.
///
/// # Errors
///
/// Returns [`Error::Capability`] for a provider tool whose declaration the host
/// omitted, so a non-function tool is never silently dropped (Section 12
/// preamble).
fn build_tools(model: &ModelRef, tools: &[ToolSpec]) -> Result<Vec<Value>> {
    let mut declarations = Vec::new();
    let mut provider = Vec::new();
    for tool in tools {
        if matches!(tool.kind, ToolKind::Function) {
            declarations.push(json!({
                "name": tool.name.as_str(),
                "description": tool.description,
                "parameters": tool.parameters,
            }));
        } else {
            provider.push(tool.provider_declaration(model, "generateContent")?.clone());
        }
    }
    let mut out = Vec::new();
    if !declarations.is_empty() {
        out.push(json!({ "functionDeclarations": declarations }));
    }
    out.extend(provider);
    Ok(out)
}

fn generation_config(request: &ModelRequest) -> serde_json::Map<String, Value> {
    let mut config = serde_json::Map::new();
    if let Some(temperature) = request.params.temperature {
        config.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(top_p) = request.params.top_p {
        config.insert("topP".to_owned(), json!(top_p));
    }
    if let Some(top_k) = request.params.top_k {
        config.insert("topK".to_owned(), json!(top_k));
    }
    if let Some(seed) = request.params.seed {
        config.insert("seed".to_owned(), json!(seed));
    }
    if let Some(max) = request.params.max_output_tokens {
        config.insert("maxOutputTokens".to_owned(), json!(max));
    }
    if !request.params.stop_sequences.is_empty() {
        config.insert(
            "stopSequences".to_owned(),
            Value::Array(
                request
                    .params
                    .stop_sequences
                    .iter()
                    .map(|stop| Value::String(stop.clone()))
                    .collect(),
            ),
        );
    }
    if let Some(schema) = &request.params.output_schema {
        config.insert(
            "responseMimeType".to_owned(),
            Value::String("application/json".to_owned()),
        );
        config.insert("responseSchema".to_owned(), schema.clone());
    }
    if let Some(thinking) = thinking_config(request.reasoning) {
        config.insert("thinkingConfig".to_owned(), thinking);
    }
    if let Some(top) = request.params.logprobs {
        config.insert("responseLogprobs".to_owned(), Value::Bool(true));
        if top > 0 {
            config.insert("logprobs".to_owned(), json!(top));
        }
    }
    config
}

fn thinking_config(request: ReasoningRequest) -> Option<Value> {
    match request {
        ReasoningRequest::Disabled => Some(json!({ "thinkingBudget": 0 })),
        ReasoningRequest::Budget(budget) => {
            Some(json!({ "thinkingBudget": i64::from(budget), "includeThoughts": true }))
        }
        ReasoningRequest::Effort(effort) => {
            Some(json!({ "thinkingLevel": effort_name(effort), "includeThoughts": true }))
        }
        // `Automatic` and any future mode leave the depth to the model.
        _ => None,
    }
}

fn effort_name(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Minimal => "MINIMAL",
        ReasoningEffort::Low => "LOW",
        ReasoningEffort::High => "HIGH",
        // `Medium` and any future level default to the balanced depth.
        _ => "MEDIUM",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_core::{
        ContinuityAdapter, MediaKind, MimeType, ModelId, ProviderId, ProviderTool,
        ProviderToolCallPart, ProviderToolResultPart, ReasoningPart, ReasoningVisibility,
        ToolCallPart, ToolName, WireFamily,
    };

    use crate::WIRE_GENERATE;

    fn model() -> ModelRef {
        ModelRef::new(
            ProviderId::new("google").expect("provider"),
            ModelId::new("gemini-2.5-pro").expect("model"),
        )
    }

    fn continuity() -> GeminiContinuity {
        GeminiContinuity::new(WireFamily::new(WIRE_GENERATE).expect("family"), model())
    }

    #[test]
    fn leading_instructions_become_the_system_instruction() {
        let request = ModelRequest::new(
            model(),
            vec![Message::system("be terse"), Message::user("hi")],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be terse");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "hi");
    }

    #[test]
    fn a_mid_run_instruction_merges_into_the_user_turn() {
        let request = ModelRequest::new(
            model(),
            vec![
                Message::user("hello"),
                Message::assistant("hi"),
                Message::system("be terse"),
                Message::user("continue"),
            ],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert!(body.get("systemInstruction").is_none());
        let contents = body["contents"].as_array().expect("contents");
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(contents[2]["parts"].as_array().expect("parts").len(), 2);
    }

    #[test]
    fn a_model_turn_uses_the_model_role() {
        let request = ModelRequest::new(
            model(),
            vec![Message::user("hi"), Message::assistant("hello")],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["contents"][1]["role"], "model");
    }

    #[test]
    fn tool_call_replays_verbatim_with_its_signature() {
        let part = serde_json::json!({
            "functionCall": { "name": "lookup", "args": { "q": 1 } },
            "thoughtSignature": "sig",
        });
        let token = continuity().build(&part).expect("token");
        let assistant = Message::new(
            Role::Assistant,
            vec![Part::ToolCall(ToolCallPart {
                call_id: CallId::new(),
                name: "lookup".to_owned(),
                arguments: serde_json::json!({ "q": 1 }),
                continuity: Some(token),
            })],
        );
        let request = ModelRequest::new(model(), vec![Message::user("go"), assistant]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["contents"][1]["parts"][0], part);
    }

    #[test]
    fn provider_code_execution_replays_verbatim() {
        let call = serde_json::json!({
            "executableCode": { "language": "PYTHON", "code": "1+1" },
        });
        let result = serde_json::json!({
            "codeExecutionResult": { "outcome": "OUTCOME_OK", "output": "2" },
        });
        let call_id = CallId::new();
        let tool = ProviderTool {
            namespace: "google".to_owned(),
            name: "code_execution".to_owned(),
        };
        let assistant = Message::new(
            Role::Assistant,
            vec![
                Part::ProviderToolCall(ProviderToolCallPart {
                    call_id,
                    tool: tool.clone(),
                    input: call["executableCode"].clone(),
                    continuity: Some(continuity().build(&call).expect("call token")),
                }),
                Part::ProviderToolResult(ProviderToolResultPart {
                    call_id,
                    tool,
                    output: result["codeExecutionResult"].clone(),
                    citations: Vec::new(),
                    continuity: Some(continuity().build(&result).expect("result token")),
                }),
            ],
        );
        let request = ModelRequest::new(model(), vec![Message::user("go"), assistant]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["contents"][1]["parts"][0], call);
        assert_eq!(body["contents"][1]["parts"][1], result);
    }

    #[test]
    fn tool_result_becomes_a_function_response() {
        let call_id = CallId::new();
        let token = continuity()
            .build(&serde_json::json!({
                "functionCall": { "name": "lookup", "args": {} },
            }))
            .expect("token");
        let assistant = Message::new(
            Role::Assistant,
            vec![Part::ToolCall(ToolCallPart {
                call_id,
                name: "lookup".to_owned(),
                arguments: serde_json::json!({}),
                continuity: Some(token),
            })],
        );
        let tool = Message::new(
            Role::Tool,
            vec![Part::ToolResult(ToolResultPart::text(call_id, "42"))],
        );
        let request = ModelRequest::new(model(), vec![assistant, tool]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["contents"][1]["role"], "user");
        assert_eq!(
            body["contents"][1]["parts"][0]["functionResponse"]["name"],
            "lookup"
        );
        assert_eq!(
            body["contents"][1]["parts"][0]["functionResponse"]["response"]["result"],
            "42"
        );
    }

    #[test]
    fn a_tool_result_sends_text_and_falls_back_to_structured() {
        let call_id = CallId::new();
        let rendered = ToolResultPart {
            call_id,
            parts: vec![Part::text("rendered text")],
            structured: Some(serde_json::json!({ "answer": 42 })),
            is_error: false,
        };
        let tool = Message::new(Role::Tool, vec![Part::ToolResult(rendered)]);
        let request = ModelRequest::new(model(), vec![tool]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(
            body["contents"][0]["parts"][0]["functionResponse"]["response"]["result"],
            "rendered text"
        );

        let structured_only = ToolResultPart {
            call_id,
            parts: Vec::new(),
            structured: Some(serde_json::json!({ "answer": 42 })),
            is_error: false,
        };
        let tool = Message::new(Role::Tool, vec![Part::ToolResult(structured_only)]);
        let request = ModelRequest::new(model(), vec![tool]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(
            body["contents"][0]["parts"][0]["functionResponse"]["response"]["result"],
            serde_json::json!({ "answer": 42 })
        );
    }

    #[test]
    fn a_tool_result_carries_inline_media_in_its_parts() {
        let call_id = CallId::new();
        let result = ToolResultPart {
            call_id,
            parts: vec![
                Part::text("the chart"),
                Part::Media(MediaPart::new(
                    MediaKind::Image,
                    MediaSource::Inline(vec![1, 2, 3]),
                    MimeType::new("image/png").expect("mime"),
                )),
            ],
            structured: Some(serde_json::json!({ "rows": 12 })),
            is_error: false,
        };
        let tool = Message::new(Role::Tool, vec![Part::ToolResult(result)]);
        let request = ModelRequest::new(model(), vec![tool]);
        let body = build(&model(), &continuity(), &request).expect("build");
        let response = &body["contents"][0]["parts"][0]["functionResponse"];
        assert_eq!(response["response"]["result"], "the chart");
        assert_eq!(response["parts"][0]["inlineData"]["mimeType"], "image/png");
        assert_eq!(response["parts"][0]["inlineData"]["data"], "AQID");
    }

    #[test]
    fn a_structured_only_tool_result_with_media_keeps_the_json() {
        let call_id = CallId::new();
        let result = ToolResultPart {
            call_id,
            parts: vec![Part::Media(MediaPart::new(
                MediaKind::Image,
                MediaSource::Inline(vec![1, 2, 3]),
                MimeType::new("image/png").expect("mime"),
            ))],
            structured: Some(serde_json::json!({ "rows": 12 })),
            is_error: false,
        };
        let tool = Message::new(Role::Tool, vec![Part::ToolResult(result)]);
        let request = ModelRequest::new(model(), vec![tool]);
        let body = build(&model(), &continuity(), &request).expect("build");
        let response = &body["contents"][0]["parts"][0]["functionResponse"];
        assert_eq!(
            response["response"]["result"],
            serde_json::json!({ "rows": 12 })
        );
        assert_eq!(response["parts"][0]["inlineData"]["mimeType"], "image/png");
    }

    #[test]
    fn media_from_a_url_in_a_tool_result_is_a_typed_error() {
        let call_id = CallId::new();
        let result = ToolResultPart {
            call_id,
            parts: vec![Part::Media(MediaPart::new(
                MediaKind::Image,
                MediaSource::Url("https://example.test/a.png".to_owned()),
                MimeType::new("image/png").expect("mime"),
            ))],
            structured: None,
            is_error: false,
        };
        let tool = Message::new(Role::Tool, vec![Part::ToolResult(result)]);
        let request = ModelRequest::new(model(), vec![tool]);
        let error = build(&model(), &continuity(), &request).expect_err("refused");
        assert!(
            matches!(error, Error::Capability { ref feature, .. } if feature.contains("tool result")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn consecutive_same_role_turns_merge() {
        let request = ModelRequest::new(model(), vec![Message::user("a"), Message::user("b")]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["contents"].as_array().expect("contents").len(), 1);
        assert_eq!(body["contents"][0]["parts"][1]["text"], "b");
    }

    #[test]
    fn inline_image_uses_inline_data() {
        let media = MediaPart::new(
            MediaKind::Image,
            MediaSource::Inline(vec![1, 2, 3]),
            norte_core::MimeType::new("image/png").expect("mime"),
        );
        let request = ModelRequest::new(
            model(),
            vec![Message::new(Role::User, vec![Part::Media(media)])],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(
            body["contents"][0]["parts"][0]["inlineData"]["mimeType"],
            "image/png"
        );
        assert_eq!(
            body["contents"][0]["parts"][0]["inlineData"]["data"],
            "AQID"
        );
    }

    #[test]
    fn audio_and_video_use_inline_data() {
        let audio = MediaPart::new(
            MediaKind::Audio,
            MediaSource::Inline(vec![1, 2, 3]),
            norte_core::MimeType::new("audio/wav").expect("mime"),
        );
        let video = MediaPart::new(
            MediaKind::Video,
            MediaSource::Url("https://example.test/a.mp4".to_owned()),
            norte_core::MimeType::new("video/mp4").expect("mime"),
        );
        let request = ModelRequest::new(
            model(),
            vec![Message::new(
                Role::User,
                vec![Part::Media(audio), Part::Media(video)],
            )],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(
            body["contents"][0]["parts"][0]["inlineData"]["mimeType"],
            "audio/wav"
        );
        assert_eq!(
            body["contents"][0]["parts"][1]["fileData"]["mimeType"],
            "video/mp4"
        );
    }

    #[test]
    fn tools_use_function_declarations() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.tools = vec![ToolSpec {
            name: ToolName::new("lookup").expect("tool"),
            description: "Look up a value".to_owned(),
            kind: ToolKind::Function,
            parameters: serde_json::json!({ "type": "object" }),
            declaration: None,
            approval: None,
        }];
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "lookup"
        );
    }

    #[test]
    fn provider_tools_are_separate_entries() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.tools = vec![
            ToolSpec {
                name: ToolName::new("lookup").expect("tool"),
                description: "Look up a value".to_owned(),
                kind: ToolKind::Function,
                parameters: serde_json::json!({ "type": "object" }),
                declaration: None,
                approval: None,
            },
            ToolSpec {
                name: ToolName::new("google_search").expect("tool"),
                description: "provider tool".to_owned(),
                kind: ToolKind::ProviderExecuted,
                parameters: serde_json::json!({ "type": "object" }),
                declaration: Some(serde_json::json!({ "googleSearch": {} })),
                approval: None,
            },
        ];
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "lookup"
        );
        assert_eq!(body["tools"][1], serde_json::json!({ "googleSearch": {} }));
    }

    #[test]
    fn a_provider_tool_without_a_declaration_is_a_typed_error() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.tools = vec![ToolSpec {
            name: ToolName::new("google_search").expect("tool"),
            description: "provider tool".to_owned(),
            kind: ToolKind::ProviderExecuted,
            parameters: serde_json::json!({ "type": "object" }),
            declaration: None,
            approval: None,
        }];
        let error = build(&model(), &continuity(), &request).expect_err("error");
        assert!(matches!(error, Error::Capability { .. }));
    }

    #[test]
    fn structured_output_sets_response_schema() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.params.output_schema = Some(serde_json::json!({ "type": "object" }));
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(
            body["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert_eq!(body["generationConfig"]["responseSchema"]["type"], "object");
    }

    #[test]
    fn count_tokens_body_omits_generation_config() {
        let mut request =
            ModelRequest::new(model(), vec![Message::system("rules"), Message::user("hi")]);
        request.params.temperature = Some(0.5);
        request.reasoning = ReasoningRequest::Budget(256);
        let body = build_count(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "rules");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "hi");
        assert!(body.get("generationConfig").is_none());
    }

    #[test]
    fn cache_body_qualifies_the_model_and_carries_the_prompt() {
        let request =
            ModelRequest::new(model(), vec![Message::system("rules"), Message::user("hi")]);
        let body = build_cache(&model(), &continuity(), &request, "3600s").expect("build");
        assert_eq!(body["model"], "models/gemini-2.5-pro");
        assert_eq!(body["ttl"], "3600s");
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "rules");
        assert_eq!(body["contents"][0]["role"], "user");
        assert!(body.get("generationConfig").is_none());
    }

    #[test]
    fn thinking_config_follows_the_request() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.reasoning = ReasoningRequest::Budget(1024);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            1024
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
    }

    #[test]
    fn thinking_text_is_replayed_verbatim() {
        let part = serde_json::json!({ "text": "why", "thought": true, "thoughtSignature": "sig" });
        let token = continuity().build(&part).expect("token");
        let assistant = Message::new(
            Role::Assistant,
            vec![Part::Reasoning(ReasoningPart {
                visibility: ReasoningVisibility::Full,
                text: "why".to_owned(),
                continuity: Some(token),
            })],
        );
        let request = ModelRequest::new(model(), vec![Message::user("go"), assistant]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["contents"][1]["parts"][0], part);
    }

    #[test]
    fn vendor_extras_are_spliced_into_the_body() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.extensions.insert(
            "google.gemini:safetySettings".to_owned(),
            serde_json::json!([
                { "category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_NONE" }
            ]),
        );
        request.extensions.insert(
            "google.gemini:generationConfig.responseModalities".to_owned(),
            serde_json::json!(["TEXT", "IMAGE"]),
        );
        request.extensions.insert(
            "openai.responses:metadata".to_owned(),
            serde_json::json!({ "trace": "x" }),
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["safetySettings"][0]["threshold"], "BLOCK_NONE");
        assert_eq!(body["generationConfig"]["responseModalities"][1], "IMAGE");
        assert!(body.get("metadata").is_none());
    }

    #[test]
    fn a_vendor_extra_may_not_override_adapter_state() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request
            .extensions
            .insert("google.gemini:contents".to_owned(), serde_json::json!([]));
        let error = build(&model(), &continuity(), &request).expect_err("collision");
        assert!(matches!(error, Error::Capability { .. }));
    }

    #[test]
    fn logprobs_turn_on_the_result_and_the_count() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.params.logprobs = Some(4);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["generationConfig"]["responseLogprobs"], true);
        assert_eq!(body["generationConfig"]["logprobs"], 4);
    }

    #[test]
    fn a_zero_count_asks_for_the_chosen_token_only() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.params.logprobs = Some(0);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["generationConfig"]["responseLogprobs"], true);
        assert!(
            body["generationConfig"].get("logprobs").is_none(),
            "a zero count needs no top list"
        );
    }

    #[test]
    fn logprobs_stay_out_of_the_counting_bodies() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.params.logprobs = Some(4);
        let body = build_count(&model(), &continuity(), &request).expect("build");
        assert!(
            body.get("generationConfig").is_none(),
            "a token-counting call sends no sampling parameters"
        );
    }
}
