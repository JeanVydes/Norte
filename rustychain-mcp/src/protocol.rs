//! The MCP wire types, modelled faithfully but not exhaustively.
//!
//! The structs here are the exact shapes that cross the transport. Optional
//! fields are skipped when absent, and unknown fields are preserved in an
//! `extra` map so a newer server cannot be silently lossy. The mapping onto the
//! neutral core domain lives in [`crate::tool`] and [`crate::server`]; this
//! module stays a pure description of the protocol.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The protocol revision this crate speaks. Servers may negotiate a different
/// one; whatever they answer with is stored on the session.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Identifies a client or server implementation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Implementation {
    /// The implementation name.
    pub name: String,
    /// The implementation version.
    pub version: String,
    /// An optional human-facing display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Any additional fields the peer sent.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl Implementation {
    /// Builds an implementation descriptor without a title.
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            title: None,
            extra: BTreeMap::new(),
        }
    }
}

impl Default for Implementation {
    fn default() -> Self {
        Self::new("rustychain-mcp", env!("CARGO_PKG_VERSION"))
    }
}

/// A client's filesystem-roots capability.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootsCapability {
    /// Whether the client can notify the server when roots change.
    #[serde(default)]
    pub list_changed: bool,
}

/// The capabilities a client advertises during `initialize`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    /// Present when the client can expose filesystem roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<RootsCapability>,
    /// Present when the client can service sampling requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling: Option<Value>,
    /// Present when the client can service elicitation requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elicitation: Option<Value>,
    /// Any additional capabilities the client advertises.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The tools capability of a server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    /// Whether the server notifies the client when the tool list changes.
    #[serde(default)]
    pub list_changed: bool,
}

/// The resources capability of a server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesCapability {
    /// Whether the server supports resource-change subscriptions.
    #[serde(default)]
    pub subscribe: bool,
    /// Whether the server notifies the client when the resource list changes.
    #[serde(default)]
    pub list_changed: bool,
}

/// The prompts capability of a server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptsCapability {
    /// Whether the server notifies the client when the prompt list changes.
    #[serde(default)]
    pub list_changed: bool,
}

/// The capabilities a server advertises during `initialize`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    /// Present when the server exposes tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    /// Present when the server exposes resources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    /// Present when the server exposes prompts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
    /// Present when the server exposes logging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<Value>,
    /// Present when the server exposes completions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completions: Option<Value>,
    /// Present when the server exposes experimental capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental: Option<Value>,
    /// Any additional capabilities the server advertises.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The parameters of an `initialize` request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    /// The protocol revision the client wants to use.
    pub protocol_version: String,
    /// The client's capabilities.
    pub capabilities: ClientCapabilities,
    /// The client's implementation descriptor.
    pub client_info: Implementation,
}

/// The result of an `initialize` request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// The protocol revision the server settled on.
    pub protocol_version: String,
    /// The server's capabilities.
    pub capabilities: ServerCapabilities,
    /// The server's implementation descriptor.
    pub server_info: Implementation,
    /// Optional guidance for the model, e.g. how to use the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Any additional fields the server sent.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A tool the server exposes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    /// The tool name, unique within the server.
    pub name: String,
    /// An optional display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// A human-readable description of what the tool does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The JSON Schema for the tool arguments.
    pub input_schema: Value,
    /// An optional JSON Schema for the structured result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// Optional behavioural annotations (read-only, destructive, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    /// Any additional fields the server sent.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The result of `tools/list`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListToolsResult {
    /// The tools in this page.
    pub tools: Vec<ToolDefinition>,
    /// The cursor for the next page, when the list is paginated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// The parameters of `tools/call`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallToolParams {
    /// The name of the tool to invoke.
    pub name: String,
    /// The arguments, matching the tool's input schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

/// A single block of tool or prompt content.
///
/// Modelled as a lossless struct rather than a tagged enum so an unknown block
/// type from a newer server survives in [`ContentBlock::extra`] instead of
/// failing the whole response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContentBlock {
    /// The block kind: `text`, `image`, `audio`, `resource` or `resource_link`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The text payload, for text blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The base64 payload, for image and audio blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// The media type, for image, audio and resource-link blocks.
    #[serde(default, rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The embedded resource, for resource blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<ResourceContents>,
    /// The target URI, for resource-link blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// The display name, for resource-link blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// An optional display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// An optional description, for resource-link blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Any additional fields the server sent.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl ContentBlock {
    /// Builds a text block.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::empty("text").with_text(text)
    }

    /// Builds an image block from a base64 payload.
    #[must_use]
    pub fn image(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::empty("image").with_data(data, mime_type)
    }

    /// Builds an audio block from a base64 payload.
    #[must_use]
    pub fn audio(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::empty("audio").with_data(data, mime_type)
    }

    /// Builds an embedded-resource block.
    #[must_use]
    pub fn resource(resource: ResourceContents) -> Self {
        Self {
            resource: Some(resource),
            ..Self::empty("resource")
        }
    }

    /// Builds a resource-link block pointing at an external URI.
    #[must_use]
    pub fn resource_link(uri: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uri: Some(uri.into()),
            name: Some(name.into()),
            ..Self::empty("resource_link")
        }
    }

    fn empty(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            text: None,
            data: None,
            mime_type: None,
            resource: None,
            uri: None,
            name: None,
            title: None,
            description: None,
            extra: BTreeMap::new(),
        }
    }

    fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    fn with_data(mut self, data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        self.data = Some(data.into());
        self.mime_type = Some(mime_type.into());
        self
    }
}

/// A resource's contents, either inline text or a base64 blob.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceContents {
    /// The resource URI.
    pub uri: String,
    /// The media type, when known.
    #[serde(default, rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The inline text, for textual resources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The base64 blob, for binary resources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    /// Any additional fields the server sent.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl ResourceContents {
    /// Builds textual resource contents.
    #[must_use]
    pub fn text(uri: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            mime_type: Some("text/plain".to_owned()),
            text: Some(text.into()),
            blob: None,
            extra: BTreeMap::new(),
        }
    }

    /// Builds binary resource contents from a base64 payload.
    #[must_use]
    pub fn blob(uri: impl Into<String>, mime_type: impl Into<String>, blob: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            mime_type: Some(mime_type.into()),
            text: None,
            blob: Some(blob.into()),
            extra: BTreeMap::new(),
        }
    }
}

/// The result of `tools/call`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    /// The content blocks the tool produced.
    pub content: Vec<ContentBlock>,
    /// Whether the tool failed; the content carries the explanation.
    #[serde(default)]
    pub is_error: bool,
    /// A structured result matching the tool's output schema, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    /// Any additional fields the server sent.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl CallToolResult {
    /// Builds a result from content blocks.
    #[must_use]
    pub fn new(content: Vec<ContentBlock>) -> Self {
        Self {
            content,
            is_error: false,
            structured_content: None,
            extra: BTreeMap::new(),
        }
    }

    /// Builds a single-block error result.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::new(vec![ContentBlock::text(message)])
        }
    }
}

/// An MCP resource descriptor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    /// The resource URI.
    pub uri: String,
    /// A human-readable name.
    pub name: String,
    /// An optional display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// An optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The media type, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The size in bytes, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// A parameterised MCP resource template.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTemplate {
    /// The RFC 6570 URI template.
    pub uri_template: String,
    /// A human-readable name.
    pub name: String,
    /// An optional display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// An optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The media type, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// The result of `resources/list`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResourcesResult {
    /// The resources in this page.
    pub resources: Vec<Resource>,
    /// The cursor for the next page, when the list is paginated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// The result of `resources/templates/list`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResourceTemplatesResult {
    /// The templates in this page.
    pub resource_templates: Vec<ResourceTemplate>,
    /// The cursor for the next page, when the list is paginated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// The parameters of `resources/read`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadResourceParams {
    /// The URI of the resource to read.
    pub uri: String,
}

/// The result of `resources/read`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadResourceResult {
    /// The contents of the resource.
    pub contents: Vec<ResourceContents>,
}

/// A prompt argument descriptor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptArgument {
    /// The argument name.
    pub name: String,
    /// An optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the argument must be supplied.
    #[serde(default)]
    pub required: bool,
}

/// An MCP prompt descriptor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Prompt {
    /// The prompt name.
    pub name: String,
    /// An optional display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// An optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The arguments the prompt accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
}

/// The result of `prompts/list`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPromptsResult {
    /// The prompts in this page.
    pub prompts: Vec<Prompt>,
    /// The cursor for the next page, when the list is paginated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// The parameters of `prompts/get`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GetPromptParams {
    /// The prompt name.
    pub name: String,
    /// The prompt arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<BTreeMap<String, String>>,
}

/// The role of a prompt message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptRole {
    /// A message authored by the user.
    User,
    /// A message authored by the assistant.
    Assistant,
}

/// One message in a prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptMessage {
    /// Who authored the message.
    pub role: PromptRole,
    /// The message content.
    pub content: ContentBlock,
}

/// The result of `prompts/get`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GetPromptResult {
    /// An optional description of the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The messages the prompt expands to.
    pub messages: Vec<PromptMessage>,
}

/// The shared `{ cursor }` parameter of the paginated list methods.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PaginatedParams {
    /// The cursor returned by the previous page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initialize_uses_camel_case_on_the_wire() {
        let params = InitializeParams {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            capabilities: ClientCapabilities::default(),
            client_info: Implementation::new("c", "1"),
        };
        let value = serde_json::to_value(&params).expect("encode");
        assert_eq!(value["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(value["clientInfo"]["name"], "c");
        assert!(value["capabilities"].is_object());
    }

    #[test]
    fn initialize_result_decodes_and_keeps_unknown_fields() {
        let value = json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": { "listChanged": true }, "vendor": { "x": 1 } },
            "serverInfo": { "name": "s", "version": "2" },
            "instructions": "be nice",
            "future": true
        });
        let result: InitializeResult = serde_json::from_value(value).expect("decode");
        assert_eq!(result.server_info.name, "s");
        assert_eq!(
            result
                .capabilities
                .tools
                .as_ref()
                .map(|tools| tools.list_changed),
            Some(true)
        );
        assert_eq!(result.instructions.as_deref(), Some("be nice"));
        assert_eq!(result.extra.get("future"), Some(&json!(true)));
        assert!(result.capabilities.extra.contains_key("vendor"));
    }

    #[test]
    fn content_blocks_round_trip_and_capture_unknown_kinds() {
        let text = ContentBlock::text("hello");
        assert_eq!(
            serde_json::to_value(&text).expect("encode"),
            json!({ "type": "text", "text": "hello" })
        );

        let image = ContentBlock::image("AAAA", "image/png");
        assert_eq!(
            serde_json::to_value(&image).expect("encode"),
            json!({ "type": "image", "data": "AAAA", "mimeType": "image/png" })
        );

        let unknown = json!({ "type": "video", "url": "https://x", "extra": 1 });
        let block: ContentBlock = serde_json::from_value(unknown.clone()).expect("decode");
        assert_eq!(block.kind, "video");
        assert_eq!(
            serde_json::to_value(&block).expect("encode"),
            unknown
        );
    }

    #[test]
    fn call_tool_result_serializes_its_flags() {
        let result = CallToolResult::new(vec![ContentBlock::text("ok")]);
        let value = serde_json::to_value(&result).expect("encode");
        assert_eq!(value["isError"], false);
        assert!(value.get("structuredContent").is_none());
        let decoded: CallToolResult = serde_json::from_value(value).expect("decode");
        assert!(!decoded.is_error);
    }

    #[test]
    fn resource_contents_support_text_and_blob() {
        let text = serde_json::to_value(ResourceContents::text("file://a", "body")).expect("encode");
        assert_eq!(text["uri"], "file://a");
        assert_eq!(text["text"], "body");

        let blob =
            serde_json::to_value(ResourceContents::blob("file://b", "image/png", "BBBB"))
                .expect("encode");
        assert_eq!(blob["blob"], "BBBB");
        assert!(blob.get("text").is_none());
    }
}
