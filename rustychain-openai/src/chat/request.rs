use std::collections::BTreeMap;

use rustychain_core::{
    CallId, Error, MediaKind, MediaPart, MediaSource, Message, ModelRef, ModelRequest, Part,
    ReasoningEffort, ReasoningRequest, Result, Role, SESSION_EXTENSION, ToolKind, ToolResultPart,
    ToolSpec,
};
use rustychain_http::{Quirk, Quirks};
use serde_json::{Value, json};

use crate::continuity::OpenAiContinuity;
use crate::json::{data_url, opt_str};

type CallIndex = BTreeMap<CallId, String>;

/// Builds the wire body for one `Chat Completions` request.
///
/// `quirks` are the host-declared deviations of the target vendor (FR-OAI-2);
/// each one omits the corresponding optional field.
///
/// # Errors
///
/// Returns an error when a part cannot be expressed on this wire, for example a
/// document or a blob that no [`MediaAdapter`] resolved, or when a structured
/// output schema is requested against a vendor declared as
/// [`Quirk::NoStructuredOutput`].
///
/// [`MediaAdapter`]: rustychain_core::MediaAdapter
pub(crate) fn build(
    model: &ModelRef,
    continuity: &OpenAiContinuity,
    quirks: &Quirks,
    request: &ModelRequest,
) -> Result<Value> {
    let calls = call_index(continuity, &request.messages);
    let mut messages = Vec::new();
    for message in &request.messages {
        push_message(&calls, message, &mut messages)?;
    }

    let mut body = serde_json::Map::new();
    body.insert(
        "model".to_owned(),
        Value::String(model.model.as_str().to_owned()),
    );
    body.insert("messages".to_owned(), Value::Array(messages));
    body.insert("stream".to_owned(), Value::Bool(true));
    if !quirks.contains(Quirk::NoStreamUsage) {
        body.insert(
            "stream_options".to_owned(),
            json!({ "include_usage": true }),
        );
    }
    if !request.tools.is_empty() {
        body.insert(
            "tools".to_owned(),
            Value::Array(build_tools(model, &request.tools)?),
        );
    }
    if !quirks.contains(Quirk::NoReasoningEffort) {
        if let Some(effort) = build_effort(request.reasoning) {
            body.insert(
                "reasoning_effort".to_owned(),
                Value::String(effort.to_owned()),
            );
        }
    }
    if quirks.contains(Quirk::NoStructuredOutput) {
        if request.params.output_schema.is_some() {
            return Err(Error::Capability {
                model: model.clone(),
                feature: "structured output (the vendor declares no_structured_output)".to_owned(),
            });
        }
    } else if let Some(format) = build_response_format(request) {
        body.insert("response_format".to_owned(), format);
    }
    if let Some(max) = request.params.max_output_tokens {
        body.insert("max_tokens".to_owned(), json!(max));
    }
    if let Some(temperature) = request.params.temperature {
        body.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(top_p) = request.params.top_p {
        body.insert("top_p".to_owned(), json!(top_p));
    }
    if !quirks.contains(Quirk::NoSeed) {
        if let Some(seed) = request.params.seed {
            body.insert("seed".to_owned(), json!(seed));
        }
    }
    if !quirks.contains(Quirk::NoStopSequences) && !request.params.stop_sequences.is_empty() {
        body.insert(
            "stop".to_owned(),
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
    if !quirks.contains(Quirk::NoPromptCacheKey) {
        if let Some(key) = request
            .extensions
            .get(SESSION_EXTENSION)
            .and_then(Value::as_str)
        {
            body.insert("prompt_cache_key".to_owned(), Value::String(key.to_owned()));
        }
    }
    Ok(Value::Object(body))
}

fn build_effort(request: ReasoningRequest) -> Option<&'static str> {
    match request {
        ReasoningRequest::Effort(ReasoningEffort::Minimal) => Some("minimal"),
        ReasoningRequest::Effort(ReasoningEffort::Low) => Some("low"),
        ReasoningRequest::Effort(ReasoningEffort::Medium) => Some("medium"),
        ReasoningRequest::Effort(ReasoningEffort::High) => Some("high"),
        _ => None,
    }
}

fn build_response_format(request: &ModelRequest) -> Option<Value> {
    let schema = request.params.output_schema.as_ref()?;
    Some(json!({
        "type": "json_schema",
        "json_schema": {
            "name": "output",
            "schema": schema,
            "strict": true,
        }
    }))
}

/// Wraps the function tools for the wire.
///
/// # Errors
///
/// Returns [`Error::Capability`] for a tool kind this wire cannot express, so a
/// non-function tool is never silently dropped (Section 12 preamble).
fn build_tools(model: &ModelRef, tools: &[ToolSpec]) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        if !matches!(tool.kind, ToolKind::Function) {
            return Err(Error::Capability {
                model: model.clone(),
                feature: format!("tool kind {:?} on the chat completions wire", tool.kind),
            });
        }
        out.push(json!({
            "type": "function",
            "function": {
                "name": tool.name.as_str(),
                "description": tool.description,
                "parameters": tool.parameters,
                "strict": false,
            }
        }));
    }
    Ok(out)
}

fn call_index(continuity: &OpenAiContinuity, messages: &[Message]) -> CallIndex {
    let mut index = CallIndex::new();
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
            let Some(vendor) = opt_str(&value, "call_id") else {
                continue;
            };
            index.insert(call.call_id, vendor.to_owned());
        }
    }
    index
}

fn vendor_call(calls: &CallIndex, id: CallId) -> String {
    calls.get(&id).map_or_else(|| id.to_string(), Clone::clone)
}

fn push_message(calls: &CallIndex, message: &Message, out: &mut Vec<Value>) -> Result<()> {
    match message.role {
        Role::Assistant => push_assistant(calls, message, out),
        Role::Tool => push_tool_results(calls, message, out),
        role => push_user(calls, role, message, out)?,
    }
    Ok(())
}

fn push_user(calls: &CallIndex, role: Role, message: &Message, out: &mut Vec<Value>) -> Result<()> {
    let content = user_content(message)?;
    out.push(json!({
        "role": chat_role(role),
        "content": content,
    }));
    push_tool_results(calls, message, out);
    Ok(())
}

/// The Chat Completions role policy (FR-ROLE-2).
///
/// `System` and `Developer` both map to the wire `system` role and may appear
/// anywhere in the conversation, so a mid-run instruction update is just another
/// `system` message in sequence.
fn chat_role(role: Role) -> &'static str {
    match role {
        Role::System | Role::Developer => "system",
        _ => "user",
    }
}

fn push_assistant(calls: &CallIndex, message: &Message, out: &mut Vec<Value>) {
    let text = message.text();
    let tool_calls = assistant_tool_calls(calls, message);
    let mut object = serde_json::Map::new();
    object.insert("role".to_owned(), Value::String("assistant".to_owned()));
    if text.is_empty() && !tool_calls.is_empty() {
        object.insert("content".to_owned(), Value::Null);
    } else {
        object.insert("content".to_owned(), Value::String(text));
    }
    if !tool_calls.is_empty() {
        object.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }
    out.push(Value::Object(object));
}

fn assistant_tool_calls(calls: &CallIndex, message: &Message) -> Vec<Value> {
    message
        .parts
        .iter()
        .filter_map(Part::as_tool_call)
        .map(|call| {
            json!({
                "id": vendor_call(calls, call.call_id),
                "type": "function",
                "function": {
                    "name": call.name,
                    "arguments": call.arguments.to_string(),
                }
            })
        })
        .collect()
}

fn push_tool_results(calls: &CallIndex, message: &Message, out: &mut Vec<Value>) {
    for result in message.tool_results() {
        out.push(json!({
            "role": "tool",
            "tool_call_id": vendor_call(calls, result.call_id),
            "content": tool_content(result),
        }));
    }
}

fn tool_content(result: &ToolResultPart) -> Value {
    if let Some(structured) = &result.structured {
        return Value::String(structured.to_string());
    }
    let text: String = result.parts.iter().filter_map(Part::as_text).collect();
    Value::String(text)
}

fn user_content(message: &Message) -> Result<Value> {
    let has_media = message.parts.iter().any(|part| part.as_media().is_some());
    if !has_media {
        let text: String = message.parts.iter().filter_map(Part::as_text).collect();
        return Ok(Value::String(text));
    }
    let mut blocks = Vec::new();
    for part in &message.parts {
        match part {
            Part::Text(text) => blocks.push(json!({
                "type": "text",
                "text": text.text,
            })),
            Part::Refusal(refusal) => blocks.push(json!({
                "type": "text",
                "text": refusal.text,
            })),
            Part::Media(media) => blocks.push(media_block(media)?),
            _ => {}
        }
    }
    Ok(Value::Array(blocks))
}

fn media_block(media: &MediaPart) -> Result<Value> {
    if !matches!(media.kind, MediaKind::Image) {
        return Err(Error::InvalidInput(
            "openai chat: only image media is supported on this wire".to_owned(),
        ));
    }
    let url = match &media.source {
        MediaSource::Inline(bytes) => data_url(media.mime.as_str(), bytes),
        MediaSource::Url(url) => url.clone(),
        _ => {
            return Err(Error::InvalidInput(
                "openai chat: image must be inline bytes or a URL".to_owned(),
            ));
        }
    };
    Ok(json!({
        "type": "image_url",
        "image_url": { "url": url },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustychain_core::{
        ContinuityAdapter, MimeType, ModelId, ProviderId, ToolCallPart, ToolName, ToolResultPart,
        WireFamily,
    };

    use crate::WIRE_CHAT;

    fn model() -> ModelRef {
        ModelRef::new(
            ProviderId::new("openai").expect("provider"),
            ModelId::new("gpt-4o").expect("model"),
        )
    }

    fn continuity() -> OpenAiContinuity {
        OpenAiContinuity::new(WireFamily::new(WIRE_CHAT).expect("family"), model())
    }

    #[test]
    fn instructions_map_to_the_system_role() {
        let request = ModelRequest::new(
            model(),
            vec![Message::developer("be terse"), Message::user("hi")],
        );
        let body = build(&model(), &continuity(), &Quirks::new(), &request).expect("build");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be terse");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn a_mid_run_instruction_keeps_the_system_role_inline() {
        let request = ModelRequest::new(
            model(),
            vec![
                Message::user("hello"),
                Message::assistant("hi"),
                Message::developer("be terse"),
                Message::user("continue"),
            ],
        );
        let body = build(&model(), &continuity(), &Quirks::new(), &request).expect("build");
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2]["role"], "system");
        assert_eq!(messages[2]["content"], "be terse");
        assert_eq!(messages[3]["role"], "user");
    }

    #[test]
    fn assistant_tool_calls_and_tool_results_reuse_vendor_ids() {
        let call_id = CallId::new();
        let token = continuity()
            .build(&serde_json::json!({ "call_id": "call_1" }))
            .expect("token");
        let assistant = Message::new(
            Role::Assistant,
            vec![
                Part::text("using tool"),
                Part::ToolCall(ToolCallPart {
                    call_id,
                    name: "lookup".to_owned(),
                    arguments: serde_json::json!({ "q": 1 }),
                    continuity: Some(token),
                }),
            ],
        );
        let tool = Message::tool(vec![ToolResultPart::text(call_id, "42")]);
        let request = ModelRequest::new(model(), vec![assistant, tool]);
        let body = build(&model(), &continuity(), &Quirks::new(), &request).expect("build");
        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["content"], "using tool");
        assert_eq!(body["messages"][0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function"]["name"],
            "lookup"
        );
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "call_1");
        assert_eq!(body["messages"][1]["content"], "42");
    }

    #[test]
    fn inline_image_becomes_an_image_url_block() {
        let media = MediaPart::new(
            MediaKind::Image,
            MediaSource::Inline(vec![1, 2, 3]),
            MimeType::new("image/png").expect("mime"),
        );
        let message = Message::new(Role::User, vec![Part::text("look"), Part::Media(media)]);
        let request = ModelRequest::new(model(), vec![message]);
        let body = build(&model(), &continuity(), &Quirks::new(), &request).expect("build");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        let url = body["messages"][0]["content"][1]["image_url"]["url"]
            .as_str()
            .expect("url");
        assert!(url.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn non_image_media_is_rejected() {
        let media = MediaPart::new(
            MediaKind::Document,
            MediaSource::Inline(vec![1]),
            MimeType::new("application/pdf").expect("mime"),
        );
        let message = Message::new(Role::User, vec![Part::Media(media)]);
        let request = ModelRequest::new(model(), vec![message]);
        assert!(build(&model(), &continuity(), &Quirks::new(), &request).is_err());
    }

    #[test]
    fn output_schema_becomes_a_response_format() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.params.output_schema = Some(serde_json::json!({ "type": "object" }));
        let body = build(&model(), &continuity(), &Quirks::new(), &request).expect("build");
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    }

    #[test]
    fn function_tools_are_wrapped() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.tools = vec![ToolSpec {
            name: ToolName::new("lookup").expect("tool"),
            description: "Look up a value".to_owned(),
            kind: ToolKind::Function,
            parameters: serde_json::json!({ "type": "object" }),
            approval: None,
        }];
        let body = build(&model(), &continuity(), &Quirks::new(), &request).expect("build");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "lookup");
        assert_eq!(body["tools"][0]["function"]["strict"], false);
    }

    #[test]
    fn quirks_omit_the_deviating_fields() {
        let quirks: Quirks = [
            Quirk::NoStreamUsage,
            Quirk::NoSeed,
            Quirk::NoStopSequences,
            Quirk::NoReasoningEffort,
            Quirk::NoPromptCacheKey,
        ]
        .into_iter()
        .collect();
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.params.seed = Some(7);
        request.params.stop_sequences = vec!["stop".to_owned()];
        request.reasoning = ReasoningRequest::Effort(ReasoningEffort::High);
        request
            .extensions
            .insert(SESSION_EXTENSION.to_owned(), json!("sess"));
        let body = build(&model(), &continuity(), &quirks, &request).expect("build");
        assert!(body.get("stream_options").is_none());
        assert!(body.get("seed").is_none());
        assert!(body.get("stop").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("prompt_cache_key").is_none());
    }

    #[test]
    fn structured_output_against_a_deviating_vendor_is_a_typed_error() {
        let quirks: Quirks = [Quirk::NoStructuredOutput].into_iter().collect();
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.params.output_schema = Some(json!({ "type": "object" }));
        let error = build(&model(), &continuity(), &quirks, &request).expect_err("error");
        assert!(matches!(error, Error::Capability { .. }));
    }

    #[test]
    fn a_non_function_tool_is_a_typed_error() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.tools = vec![ToolSpec {
            name: ToolName::new("search").expect("tool"),
            description: "provider tool".to_owned(),
            kind: ToolKind::ProviderExecuted,
            parameters: json!({ "type": "object" }),
            approval: None,
        }];
        let error = build(&model(), &continuity(), &Quirks::new(), &request).expect_err("error");
        assert!(matches!(error, Error::Capability { .. }));
    }
}
