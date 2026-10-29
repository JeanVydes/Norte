use std::collections::BTreeMap;

use norte_core::{
    CallId, Error, ExtensionPart, MediaKind, MediaPart, MediaSource, Message, ModelRef,
    ModelRequest, Part, ProviderToolCallPart, ProviderToolResultPart, ReasoningEffort,
    ReasoningPart, ReasoningRequest, Result, Role, SESSION_EXTENSION, ToolCallPart, ToolKind,
    ToolResultPart, ToolSpec, splice_vendor_extensions,
};
use norte_http::{Quirk, Quirks};
use serde_json::{Value, json};

use crate::WIRE_RESPONSES;
use crate::continuity::OpenAiContinuity;
use crate::json::{data_url, opt_str};

#[derive(Clone, Debug)]
struct CallMeta {
    vendor: String,
    item: Option<String>,
}

type CallIndex = BTreeMap<CallId, CallMeta>;

/// Builds the wire body for one Responses request with no quirks.
///
/// Only the tests below call this; production goes through
/// [`build_with_quirks`] so the vendor profile is applied.
///
/// # Errors
///
/// Returns a codec error when a part cannot be expressed on the wire, for
/// example media stored as a blob with no configured [`MediaAdapter`].
///
/// [`MediaAdapter`]: norte_core::MediaAdapter
#[cfg(test)]
pub(crate) fn build(
    model: &ModelRef,
    continuity: &OpenAiContinuity,
    request: &ModelRequest,
) -> Result<Value> {
    build_with_quirks(model, continuity, &Quirks::new(), request)
}

pub(crate) fn build_with_quirks(
    model: &ModelRef,
    continuity: &OpenAiContinuity,
    quirks: &Quirks,
    request: &ModelRequest,
) -> Result<Value> {
    let (instructions, conversation) = split_instructions(&request.messages);
    let calls = call_index(continuity, &request.messages);
    let input = build_input(continuity, &calls, conversation, &request.cache_boundaries)?;

    let mut body = serde_json::Map::new();
    body.insert(
        "model".to_owned(),
        Value::String(model.model.as_str().to_owned()),
    );
    body.insert("input".to_owned(), Value::Array(input));
    body.insert("stream".to_owned(), Value::Bool(true));
    body.insert("store".to_owned(), Value::Bool(false));
    if let Some(instructions) = instructions {
        body.insert("instructions".to_owned(), Value::String(instructions));
    }
    if !request.tools.is_empty() {
        body.insert(
            "tools".to_owned(),
            Value::Array(build_tools(model, &request.tools)?),
        );
    }
    let mut include: Vec<Value> = Vec::new();
    if let Some(reasoning) = build_reasoning(request.reasoning) {
        body.insert("reasoning".to_owned(), reasoning);
        include.push(Value::String("reasoning.encrypted_content".to_owned()));
    }
    if request.params.logprobs.is_some() {
        include.push(Value::String("message.output_text.logprobs".to_owned()));
    }
    if !include.is_empty() {
        body.insert("include".to_owned(), Value::Array(include));
    }
    if let Some(text) = build_text(request) {
        body.insert("text".to_owned(), text);
    }
    if let Some(max) = request.params.max_output_tokens {
        body.insert("max_output_tokens".to_owned(), json!(max));
    }
    if let Some(temperature) = request.params.temperature {
        body.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(top_p) = request.params.top_p {
        body.insert("top_p".to_owned(), json!(top_p));
    }
    if let Some(top) = request.params.logprobs {
        body.insert("top_logprobs".to_owned(), json!(top));
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
    splice_vendor_extensions(model, WIRE_RESPONSES, &request.extensions, &mut body)?;
    Ok(Value::Object(body))
}

fn split_instructions(messages: &[Message]) -> (Option<String>, &[Message]) {
    let count = messages
        .iter()
        .take_while(|message| message.role.is_instruction())
        .count();
    if count == 0 {
        return (None, messages);
    }
    let joined: Vec<String> = messages[..count]
        .iter()
        .map(Message::text)
        .filter(|text| !text.is_empty())
        .collect();
    let instructions = if joined.is_empty() {
        None
    } else {
        Some(joined.join("\n\n"))
    };
    (instructions, &messages[count..])
}

fn build_reasoning(request: ReasoningRequest) -> Option<Value> {
    let effort = match request {
        ReasoningRequest::Disabled => "none",
        ReasoningRequest::Effort(ReasoningEffort::Minimal) => "minimal",
        ReasoningRequest::Effort(ReasoningEffort::Low) => "low",
        ReasoningRequest::Effort(ReasoningEffort::Medium) => "medium",
        ReasoningRequest::Effort(ReasoningEffort::High) => "high",
        ReasoningRequest::Effort(ReasoningEffort::XHigh) => "xhigh",
        ReasoningRequest::Effort(ReasoningEffort::Max) => "max",
        _ => return None,
    };
    Some(json!({ "effort": effort, "summary": "auto" }))
}

fn build_text(request: &ModelRequest) -> Option<Value> {
    let schema = request.params.output_schema.as_ref()?;
    Some(json!({
        "format": {
            "type": "json_schema",
            "name": "output",
            "schema": schema,
            "strict": true,
        }
    }))
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
                "type": "function",
                "name": tool.name.as_str(),
                "description": tool.description,
                "parameters": tool.parameters,
                "strict": false,
            }));
        } else {
            out.push(tool.provider_declaration(model, "responses")?.clone());
        }
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
            index.insert(
                call.call_id,
                CallMeta {
                    vendor: vendor.to_owned(),
                    item: opt_str(&value, "id").map(ToOwned::to_owned),
                },
            );
        }
    }
    index
}

fn vendor_call(index: &CallIndex, id: CallId) -> String {
    index
        .get(&id)
        .map_or_else(|| id.to_string(), |meta| meta.vendor.clone())
}

fn build_input(
    continuity: &OpenAiContinuity,
    calls: &CallIndex,
    messages: &[Message],
    boundaries: &[u32],
) -> Result<Vec<Value>> {
    let mut items = Vec::new();
    let mut last_content: Option<(usize, usize)> = None;
    for (position, message) in messages.iter().enumerate() {
        let start = items.len();
        push_message(continuity, calls, message, &mut items)?;
        update_last_content(&items, start, &mut last_content);
        let end = u32::try_from(position + 1).unwrap_or(u32::MAX);
        if boundaries.contains(&end) {
            if let Some((item, block)) = last_content {
                set_breakpoint(&mut items, item, block);
            }
        }
    }
    Ok(items)
}

fn push_message(
    continuity: &OpenAiContinuity,
    calls: &CallIndex,
    message: &Message,
    items: &mut Vec<Value>,
) -> Result<()> {
    match message.role {
        Role::Assistant => push_assistant(continuity, calls, message, items)?,
        other => push_content_message(message, role_name(other), items)?,
    }
    push_tool_results(calls, message, items)
}

/// The Responses role policy (FR-ROLE-2).
///
/// Leading `System`/`Developer` messages become the top-level `instructions`
/// field; a `System`/`Developer` message that appears mid-conversation keeps its
/// own `system`/`developer` role inline in `input`, which is how a mid-run
/// instruction update reaches the model.
fn role_name(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::Developer => "developer",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
        _ => "user",
    }
}

fn push_content_message(message: &Message, role: &str, items: &mut Vec<Value>) -> Result<()> {
    let content = content_blocks(&message.parts)?;
    if !content.is_empty() {
        items.push(json!({
            "type": "message",
            "role": role,
            "content": content,
        }));
    }
    for part in &message.parts {
        if let Part::Extension(ExtensionPart { namespace, data }) = part {
            if namespace.starts_with("openai") {
                items.push(data.clone());
            }
        }
    }
    Ok(())
}

fn push_assistant(
    continuity: &OpenAiContinuity,
    calls: &CallIndex,
    message: &Message,
    items: &mut Vec<Value>,
) -> Result<()> {
    let content = content_blocks(&message.parts)?;
    if !content.is_empty() {
        items.push(json!({
            "type": "message",
            "role": "assistant",
            "content": content,
        }));
    }
    for part in &message.parts {
        match part {
            Part::Reasoning(ReasoningPart {
                continuity: Some(token),
                ..
            })
            | Part::ProviderToolCall(ProviderToolCallPart {
                continuity: Some(token),
                ..
            })
            | Part::ProviderToolResult(ProviderToolResultPart {
                continuity: Some(token),
                ..
            }) => items.push(continuity.decode(token)?),
            Part::ToolCall(call) => items.push(tool_call_item(calls, call)),
            Part::Extension(ExtensionPart { namespace, data })
                if namespace.starts_with("openai") =>
            {
                items.push(data.clone());
            }
            _ => {}
        }
    }
    Ok(())
}

fn push_tool_results(calls: &CallIndex, message: &Message, items: &mut Vec<Value>) -> Result<()> {
    for part in &message.parts {
        if let Part::ToolResult(result) = part {
            items.push(tool_result_item(calls, result)?);
        }
    }
    Ok(())
}

fn tool_call_item(calls: &CallIndex, call: &ToolCallPart) -> Value {
    let mut item = json!({
        "type": "function_call",
        "call_id": vendor_call(calls, call.call_id),
        "name": call.name,
        "arguments": call.arguments.to_string(),
    });
    if let Some(meta) = calls.get(&call.call_id) {
        if let Some(id) = &meta.item {
            item["id"] = Value::String(id.clone());
        }
    }
    item
}

fn tool_result_item(calls: &CallIndex, result: &ToolResultPart) -> Result<Value> {
    Ok(json!({
        "type": "function_call_output",
        "call_id": vendor_call(calls, result.call_id),
        "output": tool_output(result)?,
    }))
}

fn tool_output(result: &ToolResultPart) -> Result<Value> {
    if result.parts.iter().any(|part| part.as_media().is_some()) {
        return Ok(Value::Array(tool_output_blocks(result)?));
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
    let text: String = result
        .parts
        .iter()
        .filter_map(|part| part.as_text())
        .collect();
    Ok(Value::String(text))
}

fn content_blocks(parts: &[Part]) -> Result<Vec<Value>> {
    let mut blocks = Vec::new();
    for part in parts {
        match part {
            Part::Text(text) => blocks.push(json!({
                "type": "input_text",
                "text": text.text,
            })),
            Part::Refusal(refusal) => blocks.push(json!({
                "type": "input_text",
                "text": refusal.text,
            })),
            Part::Media(media) => blocks.push(media_block(media)?),
            _ => {}
        }
    }
    Ok(blocks)
}

/// The content blocks of a tool result, with structured data answering for a
/// result that carries no text at all (FR-TOOL-BASH-3).
fn tool_output_blocks(result: &ToolResultPart) -> Result<Vec<Value>> {
    let mut blocks = content_blocks(&result.parts)?;
    let carries_text = result.parts.iter().any(|part| part.as_text().is_some());
    if !carries_text {
        if let Some(structured) = &result.structured {
            blocks.insert(
                0,
                json!({ "type": "input_text", "text": structured.to_string() }),
            );
        }
    }
    Ok(blocks)
}

fn media_block(media: &MediaPart) -> Result<Value> {
    if matches!(media.kind, MediaKind::Image) {
        image_block(media)
    } else {
        file_block(media)
    }
}

fn image_block(media: &MediaPart) -> Result<Value> {
    let mut block = json!({ "type": "input_image" });
    match &media.source {
        MediaSource::Inline(bytes) => {
            block["image_url"] = Value::String(data_url(media.mime.as_str(), bytes));
        }
        MediaSource::Url(url) => block["image_url"] = Value::String(url.clone()),
        MediaSource::ProviderFile(reference) => {
            block["file_id"] = Value::String(reference.file_id.clone());
        }
        _ => return Err(unresolved_media()),
    }
    Ok(block)
}

fn file_block(media: &MediaPart) -> Result<Value> {
    let mut block = json!({
        "type": "input_file",
        "filename": filename(media),
    });
    match &media.source {
        MediaSource::Inline(bytes) => {
            block["file_data"] = Value::String(data_url(media.mime.as_str(), bytes));
        }
        MediaSource::Url(url) => block["file_url"] = Value::String(url.clone()),
        MediaSource::ProviderFile(reference) => {
            block["file_id"] = Value::String(reference.file_id.clone());
        }
        _ => return Err(unresolved_media()),
    }
    Ok(block)
}

fn filename(media: &MediaPart) -> String {
    let subtype = media
        .mime
        .as_str()
        .split_once('/')
        .map_or("bin", |(_, subtype)| subtype);
    let subtype = subtype.split([';', '+']).next().unwrap_or("bin");
    format!("upload.{subtype}")
}

fn unresolved_media() -> Error {
    Error::InvalidInput(
        "openai: a media part is still stored as a blob; configure a MediaAdapter".to_owned(),
    )
}

fn update_last_content(items: &[Value], start: usize, last: &mut Option<(usize, usize)>) {
    for (offset, item) in items[start..].iter().enumerate() {
        let Some(blocks) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        if !blocks.is_empty() {
            *last = Some((start + offset, blocks.len() - 1));
        }
    }
}

fn set_breakpoint(items: &mut [Value], item: usize, block: usize) {
    let Some(blocks) = items[item].get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    if let Some(target) = blocks.get_mut(block) {
        target["prompt_cache_breakpoint"] = json!({ "mode": "explicit" });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_core::{
        CallId, ContinuityAdapter, MimeType, ModelId, ProviderId, ProviderTool,
        ProviderToolCallPart, ProviderToolResultPart, ReasoningPart, ReasoningVisibility, ToolName,
        WireFamily,
    };

    use crate::WIRE_RESPONSES;

    fn model() -> ModelRef {
        ModelRef::new(
            ProviderId::new("openai").expect("provider"),
            ModelId::new("gpt-5").expect("model"),
        )
    }

    fn continuity() -> OpenAiContinuity {
        OpenAiContinuity::new(WireFamily::new(WIRE_RESPONSES).expect("family"), model())
    }

    #[test]
    fn leading_instructions_become_the_instructions_field() {
        let request = ModelRequest::new(
            model(),
            vec![Message::system("be terse"), Message::user("hi")],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["instructions"], "be terse");
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"][0]["text"], "hi");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
    }

    #[test]
    fn a_mid_run_instruction_stays_an_inline_developer_turn() {
        let request = ModelRequest::new(
            model(),
            vec![
                Message::user("hello"),
                Message::assistant("hi"),
                Message::developer("be terse"),
                Message::user("continue"),
            ],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert!(body.get("instructions").is_none());
        let input = body["input"].as_array().expect("input");
        assert_eq!(input[2]["role"], "developer");
        assert_eq!(input[2]["content"][0]["text"], "be terse");
        assert_eq!(input[3]["role"], "user");
    }

    #[test]
    fn cache_boundaries_place_prompt_cache_breakpoints() {
        let mut request =
            ModelRequest::new(model(), vec![Message::user("one"), Message::user("two")]);
        request.cache_boundaries = vec![1];
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(
            body["input"][0]["content"][0]["prompt_cache_breakpoint"]["mode"],
            "explicit"
        );
        assert!(
            body["input"][1]["content"][0]
                .get("prompt_cache_breakpoint")
                .is_none()
        );
    }

    #[test]
    fn tool_call_and_result_reuse_the_vendor_call_id() {
        let call_id = CallId::new();
        let token = continuity()
            .build(&serde_json::json!({ "call_id": "call_1", "id": "fc_1" }))
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
        assert_eq!(body["input"][0]["type"], "function_call");
        assert_eq!(body["input"][0]["call_id"], "call_1");
        assert_eq!(body["input"][0]["id"], "fc_1");
        assert_eq!(body["input"][0]["arguments"], "{\"q\":1}");
        assert_eq!(body["input"][1]["type"], "function_call_output");
        assert_eq!(body["input"][1]["call_id"], "call_1");
        assert_eq!(body["input"][1]["output"], "42");
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
        assert_eq!(body["input"][0]["output"], "rendered text");

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
        assert_eq!(body["input"][0]["output"], "{\"answer\":42}");
    }

    #[test]
    fn a_tool_result_carries_media_in_its_output_blocks() {
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
        let output = &body["input"][0]["output"];
        assert_eq!(output[0]["type"], "input_text");
        assert_eq!(output[0]["text"], "the chart");
        assert_eq!(output[1]["type"], "input_image");
        assert!(
            output[1]["image_url"]
                .as_str()
                .expect("url")
                .starts_with("data:image/png;base64,")
        );
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
        let output = &body["input"][0]["output"];
        assert_eq!(output[0]["type"], "input_text");
        assert_eq!(output[0]["text"], "{\"answer\":42}");
        assert_eq!(output[1]["type"], "input_image");
    }

    #[test]
    fn inline_image_becomes_a_data_url() {
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
        let url = body["input"][0]["content"][0]["image_url"]
            .as_str()
            .expect("url");
        assert!(url.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn audio_and_video_travel_as_input_files() {
        let audio = MediaPart::new(
            MediaKind::Audio,
            MediaSource::Inline(vec![1, 2, 3]),
            MimeType::new("audio/wav").expect("mime"),
        );
        let video = MediaPart::new(
            MediaKind::Video,
            MediaSource::Url("https://example.test/a.mp4".to_owned()),
            MimeType::new("video/mp4").expect("mime"),
        );
        let request = ModelRequest::new(
            model(),
            vec![Message::new(
                Role::User,
                vec![Part::Media(audio), Part::Media(video)],
            )],
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_file");
        assert_eq!(body["input"][0]["content"][0]["filename"], "upload.wav");
        assert_eq!(body["input"][0]["content"][1]["type"], "input_file");
        assert_eq!(
            body["input"][0]["content"][1]["file_url"],
            "https://example.test/a.mp4"
        );
    }

    #[test]
    fn function_tools_are_flattened() {
        let request = ModelRequest::new(model(), vec![Message::user("hi")]);
        let mut request = request;
        request.tools = vec![ToolSpec {
            name: ToolName::new("lookup").expect("tool"),
            description: "Look up a value".to_owned(),
            kind: ToolKind::Function,
            parameters: serde_json::json!({ "type": "object" }),
            declaration: None,
        }];
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "lookup");
        assert_eq!(body["tools"][0]["strict"], false);
    }

    #[test]
    fn reasoning_item_is_echoed_verbatim() {
        let item = serde_json::json!({
            "type": "reasoning",
            "id": "rs_1",
            "encrypted_content": "abc",
        });
        let token = continuity().build(&item).expect("token");
        let assistant = Message::new(
            Role::Assistant,
            vec![Part::Reasoning(ReasoningPart {
                visibility: ReasoningVisibility::Redacted,
                text: String::new(),
                continuity: Some(token),
            })],
        );
        let request = ModelRequest::new(model(), vec![assistant, Message::user("again")]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["input"][0], item);
        assert_eq!(body["input"][1]["content"][0]["text"], "again");
    }

    #[test]
    fn every_reasoning_effort_maps_to_its_wire_name() {
        for (effort, name) in [
            (ReasoningEffort::Minimal, "minimal"),
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::XHigh, "xhigh"),
            (ReasoningEffort::Max, "max"),
        ] {
            let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
            request.reasoning = ReasoningRequest::Effort(effort);
            let body = build(&model(), &continuity(), &request).expect("build");
            assert_eq!(body["reasoning"]["effort"], name);
        }
    }

    #[test]
    fn provider_tool_items_are_replayed_verbatim() {
        let call = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "completed",
            "action": { "type": "search", "query": "rust" },
        });
        let result = serde_json::json!({
            "type": "file_search_call",
            "id": "fs_1",
            "status": "completed",
            "results": [{ "file_id": "f_1", "text": "fast" }],
        });
        let adapter = continuity();
        let call_id = CallId::new();
        let tool = ProviderTool {
            namespace: "openai".to_owned(),
            name: "web_search".to_owned(),
        };
        let assistant = Message::new(
            Role::Assistant,
            vec![
                Part::ProviderToolCall(ProviderToolCallPart {
                    call_id,
                    tool: tool.clone(),
                    input: call.clone(),
                    continuity: Some(adapter.build(&call).expect("call token")),
                }),
                Part::ProviderToolResult(ProviderToolResultPart {
                    call_id,
                    tool,
                    output: result.clone(),
                    citations: Vec::new(),
                    continuity: Some(adapter.build(&result).expect("result token")),
                }),
            ],
        );
        let request = ModelRequest::new(model(), vec![assistant, Message::user("again")]);
        let body = build(&model(), &adapter, &request).expect("build");
        assert_eq!(body["input"][0], call);
        assert_eq!(body["input"][1], result);
    }

    #[test]
    fn session_becomes_the_prompt_cache_key() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request
            .extensions
            .insert(SESSION_EXTENSION.to_owned(), serde_json::json!("ses_1"));
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["prompt_cache_key"], "ses_1");
    }

    #[test]
    fn no_prompt_cache_key_quirk_omits_the_key() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request
            .extensions
            .insert(SESSION_EXTENSION.to_owned(), serde_json::json!("ses_1"));
        let quirks: Quirks = [Quirk::NoPromptCacheKey].into_iter().collect();
        let body = build_with_quirks(&model(), &continuity(), &quirks, &request).expect("build");
        assert!(body.get("prompt_cache_key").is_none());
    }

    #[test]
    fn openai_extension_items_are_replayed_verbatim() {
        let item = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "completed",
        });
        let assistant = Message::new(
            Role::Assistant,
            vec![Part::Extension(ExtensionPart {
                namespace: "openai".to_owned(),
                data: item.clone(),
            })],
        );
        let request = ModelRequest::new(model(), vec![Message::user("q"), assistant]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["input"][1], item);
    }

    #[test]
    fn user_message_extensions_are_replayed_verbatim() {
        let item = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_2",
            "status": "completed",
        });
        let user = Message::new(
            Role::User,
            vec![Part::Extension(ExtensionPart {
                namespace: "openai".to_owned(),
                data: item.clone(),
            })],
        );
        let request = ModelRequest::new(model(), vec![user]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["input"][0], item);
    }

    #[test]
    fn a_provider_tool_is_emitted_verbatim() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.tools = vec![ToolSpec {
            name: ToolName::new("web_search").expect("tool"),
            description: "provider tool".to_owned(),
            kind: ToolKind::ProviderExecuted,
            parameters: serde_json::json!({ "type": "object" }),
            declaration: Some(serde_json::json!({ "type": "web_search" })),
        }];
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(
            body["tools"][0],
            serde_json::json!({ "type": "web_search" })
        );
    }

    #[test]
    fn a_provider_tool_without_a_declaration_is_a_typed_error() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.tools = vec![ToolSpec {
            name: ToolName::new("web_search").expect("tool"),
            description: "provider tool".to_owned(),
            kind: ToolKind::ProviderExecuted,
            parameters: serde_json::json!({ "type": "object" }),
            declaration: None,
        }];
        let error = build(&model(), &continuity(), &request).expect_err("error");
        assert!(matches!(error, Error::Capability { .. }));
    }

    #[test]
    fn vendor_extras_are_spliced_into_the_body() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.extensions.insert(
            "openai.responses:metadata".to_owned(),
            serde_json::json!({ "trace": "x" }),
        );
        request.extensions.insert(
            "google.gemini:safetySettings".to_owned(),
            serde_json::json!([]),
        );
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["metadata"]["trace"], "x");
        assert!(body.get("safetySettings").is_none());
    }

    #[test]
    fn a_vendor_extra_may_not_override_adapter_state() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.extensions.insert(
            "openai.responses:model".to_owned(),
            serde_json::json!("other"),
        );
        let error = build(&model(), &continuity(), &request).expect_err("collision");
        assert!(matches!(error, Error::Capability { .. }));
    }

    #[test]
    fn logprobs_add_the_include_marker_and_the_count() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.params.logprobs = Some(3);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["top_logprobs"], 3);
        assert_eq!(body["include"], json!(["message.output_text.logprobs"]));
    }

    #[test]
    fn logprobs_join_the_reasoning_include_list() {
        let mut request = ModelRequest::new(model(), vec![Message::user("hi")]);
        request.params.logprobs = Some(0);
        request.reasoning = ReasoningRequest::Effort(ReasoningEffort::Low);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert_eq!(body["top_logprobs"], 0);
        assert_eq!(
            body["include"],
            json!([
                "reasoning.encrypted_content",
                "message.output_text.logprobs"
            ])
        );
    }

    #[test]
    fn logprobs_leave_the_request_untouched_when_unset() {
        let request = ModelRequest::new(model(), vec![Message::user("hi")]);
        let body = build(&model(), &continuity(), &request).expect("build");
        assert!(body.get("top_logprobs").is_none());
        assert!(body.get("include").is_none());
    }
}
