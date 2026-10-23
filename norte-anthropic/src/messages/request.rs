use std::collections::BTreeMap;

use norte_core::{
    CallId, Error, MediaKind, MediaPart, MediaSource, Message, ModelRef, ModelRequest, Part,
    ReasoningEffort, ReasoningRequest, Result, Role, ToolKind, ToolResultPart, ToolSpec,
    splice_vendor_extensions,
};
use serde_json::{Value, json};

use crate::WIRE_MESSAGES;
use crate::continuity::AnthropicContinuity;
use crate::json::{base64_encode, opt_str};

/// Fallback `max_tokens` for the Messages wire, which requires the field. Hosts
/// should set `GenerationParams::max_output_tokens` on the request.
const DEFAULT_MAX_TOKENS: u64 = 4096;

type CallIndex = BTreeMap<CallId, String>;

/// Builds the wire body for one Messages request.
///
/// # Errors
///
/// Returns an error when a part cannot be expressed on this wire, for example an
/// audio part or a blob that no [`MediaAdapter`] resolved.
///
/// [`MediaAdapter`]: norte_core::MediaAdapter
pub(crate) fn build(
    model: &ModelRef,
    continuity: &AnthropicContinuity,
    request: &ModelRequest,
) -> Result<Value> {
    build_body(model, continuity, request, true)
}

/// Builds the wire body for a `count_tokens` call, which accepts the prompt but
/// no generation parameters.
///
/// # Errors
///
/// Returns an error when a part cannot be expressed on this wire.
pub(crate) fn build_count(
    model: &ModelRef,
    continuity: &AnthropicContinuity,
    request: &ModelRequest,
) -> Result<Value> {
    build_body(model, continuity, request, false)
}

fn build_body(
    model: &ModelRef,
    continuity: &AnthropicContinuity,
    request: &ModelRequest,
    stream: bool,
) -> Result<Value> {
    let instructions = instruction_count(&request.messages);
    let calls = call_index(continuity, &request.messages);

    let mut body = serde_json::Map::new();
    body.insert(
        "model".to_owned(),
        Value::String(model.model.as_str().to_owned()),
    );
    if stream {
        body.insert(
            "max_tokens".to_owned(),
            json!(
                request
                    .params
                    .max_output_tokens
                    .unwrap_or(DEFAULT_MAX_TOKENS)
            ),
        );
        body.insert("stream".to_owned(), Value::Bool(true));
    }

    let system = system_blocks(&request.messages[..instructions], &request.cache_boundaries);
    if !system.is_empty() {
        body.insert("system".to_owned(), Value::Array(system));
    }

    body.insert(
        "messages".to_owned(),
        Value::Array(build_messages(
            continuity,
            &calls,
            &request.messages[instructions..],
            instructions,
            &request.cache_boundaries,
        )?),
    );

    if !request.tools.is_empty() {
        body.insert(
            "tools".to_owned(),
            Value::Array(build_tools(model, &request.tools)?),
        );
    }
    if stream {
        if let Some(temperature) = request.params.temperature {
            body.insert("temperature".to_owned(), json!(temperature));
        }
        if let Some(top_p) = request.params.top_p {
            body.insert("top_p".to_owned(), json!(top_p));
        }
        if let Some(top_k) = request.params.top_k {
            body.insert("top_k".to_owned(), json!(top_k));
        }
        if !request.params.stop_sequences.is_empty() {
            body.insert(
                "stop_sequences".to_owned(),
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
        if let ReasoningRequest::Budget(tokens) = request.reasoning {
            body.insert(
                "thinking".to_owned(),
                json!({ "type": "enabled", "budget_tokens": tokens }),
            );
        }
        if let Some(effort) = effort_name(request.reasoning) {
            body.insert("output_config".to_owned(), json!({ "effort": effort }));
        }
    }
    splice_vendor_extensions(model, WIRE_MESSAGES, &request.extensions, &mut body)?;
    Ok(Value::Object(body))
}

fn effort_name(request: ReasoningRequest) -> Option<&'static str> {
    match request {
        // Anthropic's lowest documented level is `low`; a host that overrides
        // the capability set with `minimal` still gets the closest level.
        ReasoningRequest::Effort(ReasoningEffort::Minimal | ReasoningEffort::Low) => Some("low"),
        ReasoningRequest::Effort(ReasoningEffort::Medium) => Some("medium"),
        ReasoningRequest::Effort(ReasoningEffort::High) => Some("high"),
        ReasoningRequest::Effort(ReasoningEffort::XHigh) => Some("xhigh"),
        ReasoningRequest::Effort(ReasoningEffort::Max) => Some("max"),
        _ => None,
    }
}

fn instruction_count(messages: &[Message]) -> usize {
    messages
        .iter()
        .take_while(|message| message.role.is_instruction())
        .count()
}

fn boundary_at(boundaries: &[u32], position: usize) -> bool {
    let end = u32::try_from(position + 1).unwrap_or(u32::MAX);
    boundaries.contains(&end)
}

fn system_blocks(messages: &[Message], boundaries: &[u32]) -> Vec<Value> {
    messages
        .iter()
        .enumerate()
        .map(|(position, message)| {
            let mut block = json!({ "type": "text", "text": message.text() });
            if boundary_at(boundaries, position) {
                block["cache_control"] = json!({ "type": "ephemeral" });
            }
            block
        })
        .collect()
}

fn build_messages(
    continuity: &AnthropicContinuity,
    calls: &CallIndex,
    messages: &[Message],
    offset: usize,
    boundaries: &[u32],
) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for (position, message) in messages.iter().enumerate() {
        let mark = boundary_at(boundaries, offset + position);
        push_message(continuity, calls, message, mark, &mut out)?;
    }
    Ok(out)
}

/// The Anthropic role policy (FR-ROLE-2).
///
/// Leading `System`/`Developer` messages become the top-level `system` field.
/// A `System`/`Developer` message that appears mid-conversation is rendered as a
/// `user` turn carrying the instruction text verbatim; consecutive same-role
/// turns stay distinct so cache boundaries remain addressable. This is how a
/// mid-run instruction update reaches the model.
fn push_message(
    continuity: &AnthropicContinuity,
    calls: &CallIndex,
    message: &Message,
    mark: bool,
    out: &mut Vec<Value>,
) -> Result<()> {
    match message.role {
        Role::Assistant => push_assistant(continuity, calls, message, mark, out),
        _ => push_user(continuity, calls, message, mark, out),
    }
}

fn push_user(
    continuity: &AnthropicContinuity,
    calls: &CallIndex,
    message: &Message,
    mark: bool,
    out: &mut Vec<Value>,
) -> Result<()> {
    let mut blocks = Vec::new();
    for part in &message.parts {
        match part {
            Part::Text(text) => blocks.push(json!({ "type": "text", "text": text.text })),
            Part::Refusal(refusal) => blocks.push(json!({ "type": "text", "text": refusal.text })),
            Part::Media(media) => blocks.push(media_block(media)?),
            Part::ToolResult(result) => blocks.push(tool_result_block(calls, result)?),
            Part::ProviderToolResult(result) => {
                if let Some(token) = &result.continuity {
                    blocks.push(continuity.decode(token)?);
                }
            }
            Part::Extension(extension) if extension.namespace.starts_with("anthropic") => {
                blocks.push(extension.data.clone());
            }
            _ => {}
        }
    }
    push_blocks("user", blocks, mark, out);
    Ok(())
}

fn push_assistant(
    continuity: &AnthropicContinuity,
    calls: &CallIndex,
    message: &Message,
    mark: bool,
    out: &mut Vec<Value>,
) -> Result<()> {
    let mut blocks = Vec::new();
    for part in &message.parts {
        match part {
            Part::Reasoning(reasoning) => {
                if let Some(token) = &reasoning.continuity {
                    blocks.push(continuity.decode(token)?);
                }
            }
            Part::Text(text) => blocks.push(json!({ "type": "text", "text": text.text })),
            Part::Refusal(refusal) => blocks.push(json!({ "type": "text", "text": refusal.text })),
            Part::Media(media) => blocks.push(media_block(media)?),
            Part::ToolCall(call) => blocks.push(json!({
                "type": "tool_use",
                "id": vendor_call(calls, call.call_id),
                "name": call.name,
                "input": call.arguments,
            })),
            Part::ProviderToolCall(call) => {
                if let Some(token) = &call.continuity {
                    blocks.push(continuity.decode(token)?);
                }
            }
            Part::ProviderToolResult(result) => {
                if let Some(token) = &result.continuity {
                    blocks.push(continuity.decode(token)?);
                }
            }
            Part::Extension(extension) if extension.namespace.starts_with("anthropic") => {
                blocks.push(extension.data.clone());
            }
            _ => {}
        }
    }
    push_blocks("assistant", blocks, mark, out);
    Ok(())
}

fn push_blocks(role: &str, mut blocks: Vec<Value>, mark: bool, out: &mut Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if mark {
        if let Some(last) = blocks.last_mut() {
            last["cache_control"] = json!({ "type": "ephemeral" });
        }
    }
    out.push(json!({ "role": role, "content": blocks }));
}

fn call_index(continuity: &AnthropicContinuity, messages: &[Message]) -> CallIndex {
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
            let Some(vendor) = opt_str(&value, "tool_use_id") else {
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

/// # Errors
///
/// Returns [`Error::Capability`] for a provider tool whose declaration the host
/// omitted, so a non-function tool is never silently dropped (Section 12
/// preamble).
fn build_tools(model: &ModelRef, tools: &[ToolSpec]) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        if matches!(tool.kind, ToolKind::Function) {
            out.push(json!({
                "name": tool.name.as_str(),
                "description": tool.description,
                "input_schema": tool.parameters,
            }));
        } else {
            out.push(tool.provider_declaration(model, "messages")?.clone());
        }
    }
    Ok(out)
}

fn tool_result_block(calls: &CallIndex, result: &ToolResultPart) -> Result<Value> {
    Ok(json!({
        "type": "tool_result",
        "tool_use_id": vendor_call(calls, result.call_id),
        "content": tool_result_content(result)?,
        "is_error": result.is_error,
    }))
}

fn tool_result_content(result: &ToolResultPart) -> Result<Value> {
    if result.parts.iter().any(|part| part.as_media().is_some()) {
        let mut blocks = Vec::new();
        let mut carries_text = false;
        for part in &result.parts {
            match part {
                Part::Text(text) => {
                    blocks.push(json!({ "type": "text", "text": text.text }));
                    carries_text = true;
                }
                Part::Refusal(refusal) => {
                    blocks.push(json!({ "type": "text", "text": refusal.text }));
                    carries_text = true;
                }
                Part::Media(media) => blocks.push(media_block(media)?),
                _ => {}
            }
        }
        if !carries_text {
            if let Some(structured) = &result.structured {
                blocks.insert(0, json!({ "type": "text", "text": structured.to_string() }));
            }
        }
        return Ok(Value::Array(blocks));
    }
    let carries_text = result.parts.iter().any(|part| part.as_text().is_some());
    // Text is authoritative (FR-TOOL-BASH-3): the host renders the text the
    // model reads, so structured data only answers for a result that carries no
    // text at all.
    if !carries_text {
        if let Some(structured) = &result.structured {
            return Ok(Value::String(structured.to_string()));
        }
    }
    let text: String = result.parts.iter().filter_map(Part::as_text).collect();
    Ok(Value::String(text))
}

fn media_block(media: &MediaPart) -> Result<Value> {
    let kind = match media.kind {
        MediaKind::Image => "image",
        MediaKind::Document => "document",
        _ => {
            return Err(Error::InvalidInput(
                "anthropic: only image and document media are supported on this wire".to_owned(),
            ));
        }
    };
    let source = media_source(media)?;
    Ok(json!({ "type": kind, "source": source }))
}

fn media_source(media: &MediaPart) -> Result<Value> {
    match &media.source {
        MediaSource::Inline(bytes) => Ok(json!({
            "type": "base64",
            "media_type": media.mime.as_str(),
            "data": base64_encode(bytes),
        })),
        MediaSource::Url(url) => Ok(json!({ "type": "url", "url": url })),
        MediaSource::ProviderFile(reference) => {
            Ok(json!({ "type": "file", "file_id": reference.file_id }))
        }
        _ => Err(Error::InvalidInput(
            "anthropic: a media part is still stored as a blob; configure a MediaAdapter"
                .to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_core::{
        CallId, ContinuityAdapter, ExtensionPart, MimeType, ModelId, ProviderId, ProviderTool,
        ProviderToolCallPart, ProviderToolResultPart, ReasoningPart, ReasoningVisibility,
        ToolCallPart, ToolName, WireFamily,
    };

    use crate::WIRE_MESSAGES;

    fn model() -> ModelRef {
        ModelRef::new(
            ProviderId::new("anthropic").expect("provider"),
            ModelId::new("claude-sonnet-4").expect("model"),
        )
    }

    fn continuity() -> AnthropicContinuity {
        AnthropicContinuity::new(WireFamily::new(WIRE_MESSAGES).expect("family"), model())
    }

    #[test]
    fn leading_instructions_become_the_system_field() {
        let request = ModelRequest::new(
            model(),
            vec![Message::system("be terse"), Message::user("hi")],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["system"][0]["text"], "be terse");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn a_mid_run_instruction_becomes_a_user_turn() {
        let request = ModelRequest::new(
            model(),
            vec![
                Message::user("hello"),
                Message::assistant("hi"),
                Message::developer("switch to pirate mode"),
                Message::user("continue"),
            ],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert!(body.get("system").is_none());
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["text"], "switch to pirate mode");
        assert_eq!(messages[3]["role"], "user");
    }

    #[test]
    fn default_max_tokens_is_always_present() {
        let request = ModelRequest::new(model(), vec![Message::user("hi")]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn every_reasoning_effort_maps_to_output_config() {
        for (effort, name) in [
            (ReasoningEffort::Minimal, "low"),
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::XHigh, "xhigh"),
            (ReasoningEffort::Max, "max"),
        ] {
            let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
            request.reasoning = ReasoningRequest::Effort(effort);
            let body = build(&model(), &continuity(), &request).expect("build");
            assert_eq!(body["output_config"]["effort"], name);
        }
    }

    #[test]
    fn a_reasoning_budget_enables_thinking() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.reasoning = ReasoningRequest::Budget(4096);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 4096);
        assert!(
            body.get("output_config").is_none(),
            "the legacy budget and the current effort are different knobs"
        );
    }

    #[test]
    fn a_count_request_omits_the_reasoning_controls() {
        for reasoning in [
            ReasoningRequest::Effort(ReasoningEffort::Max),
            ReasoningRequest::Budget(4096),
        ] {
            let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
            request.reasoning = reasoning;
            let body = build_count(&model(), &continuity(), &request).expect("build");
            assert!(body.get("output_config").is_none());
            assert!(body.get("thinking").is_none());
        }
    }

    #[test]
    fn cache_boundaries_mark_content_blocks() {
        let mut request =
            ModelRequest::new(model(), vec![Message::user("one"), Message::user("two")]);
        request.cache_boundaries = vec![2];
        let body = build(&model(), &continuity(), &request).expect("build");
        assert!(
            body["messages"][0]["content"][0]
                .get("cache_control")
                .is_none()
        );
        assert_eq!(
            body["messages"][1]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn a_boundary_on_the_system_prefix_marks_the_system_block() {
        let mut request =
            ModelRequest::new(model(), vec![Message::system("rules"), Message::user("hi")]);
        request.cache_boundaries = vec![1];
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn tool_use_and_tool_result_reuse_the_vendor_id() {
        let call_id = CallId::new();
        let token = continuity()
            .build(&serde_json::json!({ "tool_use_id": "toolu_1" }))
            .expect("token");
        let assistant = Message::new(
            Role::Assistant,
            vec![Part::ToolCall(ToolCallPart {
                call_id,
                name: "lookup".to_owned(),
                arguments: serde_json::json!({ "q": 1 }),
                continuity: Some(token),
            })],
        );
        let tool = Message::new(
            Role::Tool,
            vec![Part::ToolResult(ToolResultPart::text(call_id, "42"))],
        );
        let request = ModelRequest::new(model(), vec![assistant, tool]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][0]["content"][0]["id"], "toolu_1");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_result");
        assert_eq!(body["messages"][1]["content"][0]["tool_use_id"], "toolu_1");
        assert_eq!(body["messages"][1]["content"][0]["content"], "42");
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
        let request = ModelRequest::new(
            model(),
            vec![Message::new(Role::Tool, vec![Part::ToolResult(rendered)])],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(
            body["messages"][0]["content"][0]["content"],
            "rendered text"
        );

        let structured_only = ToolResultPart {
            call_id,
            parts: Vec::new(),
            structured: Some(serde_json::json!({ "answer": 42 })),
            is_error: false,
        };
        let request = ModelRequest::new(
            model(),
            vec![Message::new(
                Role::Tool,
                vec![Part::ToolResult(structured_only)],
            )],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(
            body["messages"][0]["content"][0]["content"],
            "{\"answer\":42}"
        );
    }

    #[test]
    fn a_tool_result_carries_media_in_its_content_blocks() {
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
            structured: None,
            is_error: false,
        };
        let request = ModelRequest::new(
            model(),
            vec![Message::new(Role::Tool, vec![Part::ToolResult(result)])],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        let content = &body["messages"][0]["content"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "the chart");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], "AQID");
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
            structured: Some(serde_json::json!({ "answer": 42 })),
            is_error: false,
        };
        let request = ModelRequest::new(
            model(),
            vec![Message::new(Role::Tool, vec![Part::ToolResult(result)])],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        let content = &body["messages"][0]["content"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "{\"answer\":42}");
        assert_eq!(content[1]["type"], "image");
    }

    #[test]
    fn thinking_blocks_are_replayed_verbatim() {
        let block = serde_json::json!({
            "type": "thinking",
            "thinking": "why",
            "signature": "sig",
        });
        let token = continuity().build(&block).expect("token");
        let assistant = Message::new(
            Role::Assistant,
            vec![Part::Reasoning(ReasoningPart {
                visibility: ReasoningVisibility::Full,
                text: "why".to_owned(),
                continuity: Some(token),
            })],
        );
        let request = ModelRequest::new(model(), vec![assistant, Message::user("again")]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["messages"][0]["content"][0], block);
    }

    #[test]
    fn provider_tool_blocks_are_replayed_verbatim() {
        let call_block = serde_json::json!({
            "type": "server_tool_use",
            "id": "srvtoolu_1",
            "name": "web_search",
            "input": {"query": "rust"},
        });
        let result_block = serde_json::json!({
            "type": "web_search_tool_result",
            "tool_use_id": "srvtoolu_1",
            "content": [{"type": "web_search_result", "url": "https://rust-lang.org"}],
        });
        let adapter = continuity();
        let call_id = CallId::new();
        let tool = ProviderTool {
            namespace: "anthropic".to_owned(),
            name: "web_search".to_owned(),
        };
        let assistant = Message::new(
            Role::Assistant,
            vec![
                Part::ProviderToolCall(ProviderToolCallPart {
                    call_id,
                    tool: tool.clone(),
                    input: call_block["input"].clone(),
                    continuity: Some(adapter.build(&call_block).expect("call token")),
                }),
                Part::ProviderToolResult(ProviderToolResultPart {
                    call_id,
                    tool,
                    output: result_block["content"].clone(),
                    citations: Vec::new(),
                    continuity: Some(adapter.build(&result_block).expect("result token")),
                }),
            ],
        );
        let request = ModelRequest::new(model(), vec![assistant]);
        let body = build(&model(), &adapter, &request).expect("build");
        assert_eq!(body["messages"][0]["content"][0], call_block);
        assert_eq!(body["messages"][0]["content"][1], result_block);
    }

    #[test]
    fn inline_image_becomes_a_base64_source() {
        let media = MediaPart::new(
            MediaKind::Image,
            MediaSource::Inline(vec![1, 2, 3]),
            MimeType::new("image/png").expect("mime"),
        );
        let request = ModelRequest::new(
            model(),
            vec![Message::new(Role::User, vec![Part::Media(media)])],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["messages"][0]["content"][0]["type"], "image");
        assert_eq!(
            body["messages"][0]["content"][0]["source"]["type"],
            "base64"
        );
        assert_eq!(
            body["messages"][0]["content"][0]["source"]["media_type"],
            "image/png"
        );
    }

    #[test]
    fn function_tools_use_input_schema() {
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
        assert_eq!(body["tools"][0]["name"], "lookup");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    }

    #[test]
    fn a_provider_tool_is_emitted_verbatim() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.tools = vec![ToolSpec {
            name: ToolName::new("web_search").expect("tool"),
            description: "provider tool".to_owned(),
            kind: ToolKind::ProviderSchema,
            parameters: serde_json::json!({ "type": "object" }),
            declaration: Some(serde_json::json!({
                "type": "web_search_20250305",
                "name": "web_search",
            })),
            approval: None,
        }];
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["tools"][0]["type"], "web_search_20250305");
    }

    #[test]
    fn a_provider_tool_without_a_declaration_is_a_typed_error() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.tools = vec![ToolSpec {
            name: ToolName::new("web_search").expect("tool"),
            description: "provider tool".to_owned(),
            kind: ToolKind::ProviderSchema,
            parameters: serde_json::json!({ "type": "object" }),
            declaration: None,
            approval: None,
        }];
        let error = build(&model(), &continuity(), &request).expect_err("error");
        assert!(matches!(error, Error::Capability { .. }));
    }

    #[test]
    fn user_message_extensions_round_trip_verbatim() {
        let block = serde_json::json!({
            "type": "server_tool_use",
            "id": "srvtoolu_1",
            "name": "web_search",
            "input": { "query": "rust" },
        });
        let request = ModelRequest::new(
            model(),
            vec![Message::new(
                Role::User,
                vec![Part::Extension(ExtensionPart {
                    namespace: "anthropic".to_owned(),
                    data: block.clone(),
                })],
            )],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["messages"][0]["content"][0], block);
    }

    #[test]
    fn count_tokens_body_omits_generation_parameters() {
        let mut request =
            ModelRequest::new(model(), vec![Message::system("rules"), Message::user("hi")]);
        request.params.temperature = Some(0.5);
        request.params.max_output_tokens = Some(64);
        let body = build_count(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["model"], "claude-sonnet-4");
        assert_eq!(body["system"][0]["text"], "rules");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("stream").is_none());
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn audio_media_is_rejected() {
        let media = MediaPart::new(
            MediaKind::Audio,
            MediaSource::Inline(vec![1]),
            MimeType::new("audio/wav").expect("mime"),
        );
        let request = ModelRequest::new(
            model(),
            vec![Message::new(Role::User, vec![Part::Media(media)])],
        );
        assert!(build(&model(), &continuity(), &request).is_err());
    }

    #[test]
    fn video_media_is_rejected() {
        let media = MediaPart::new(
            MediaKind::Video,
            MediaSource::Inline(vec![1]),
            MimeType::new("video/mp4").expect("mime"),
        );
        let request = ModelRequest::new(
            model(),
            vec![Message::new(Role::User, vec![Part::Media(media)])],
        );
        assert!(build(&model(), &continuity(), &request).is_err());
    }

    #[test]
    fn continuity_adapter_is_used_for_the_call_index() {
        let token = continuity()
            .build(&serde_json::json!({ "tool_use_id": "toolu_9" }))
            .expect("token");
        let call_id = CallId::new();
        let assistant = Message::new(
            Role::Assistant,
            vec![Part::ToolCall(ToolCallPart {
                call_id,
                name: "x".to_owned(),
                arguments: serde_json::json!({}),
                continuity: Some(token),
            })],
        );
        let request = ModelRequest::new(model(), vec![assistant]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["messages"][0]["content"][0]["id"], "toolu_9");
    }

    #[test]
    fn vendor_extras_are_spliced_into_the_body() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.extensions.insert(
            "anthropic.messages:metadata".to_owned(),
            serde_json::json!({ "trace": "x" }),
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["metadata"]["trace"], "x");
    }

    #[test]
    fn a_vendor_extra_may_not_override_adapter_state() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.extensions.insert(
            "anthropic.messages:model".to_owned(),
            serde_json::json!("other"),
        );
        let error = build(&model(), &continuity(), &request).expect_err("collision");
        assert!(matches!(error, Error::Capability { .. }));
    }
}
