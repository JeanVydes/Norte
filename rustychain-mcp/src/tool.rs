//! Remote MCP tools as first-class host tools.
//!
//! Every remote tool becomes one [`AnyFunction`] rather than a single generic
//! "call an MCP tool" meta-tool. That keeps each remote tool's own schema,
//! description, approval policy and audit record, so the engine and the adapters
//! treat it exactly like a local tool. Names are namespaced
//! `mcp__<server>__<tool>` to keep same-named tools from different servers apart.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::Schema;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use rustychain_core::{AnyFunction, Result as CoreResult, ToolContext, ToolName, ToolOutput};

use crate::client::McpClient;
use crate::convert::result_to_output;
use crate::error::{McpError, McpResult};
use crate::protocol::ToolDefinition;

/// The maximum length of a tool name, mirroring the core identifier rule.
const NAME_LIMIT: usize = 128;
/// Hex characters of the disambiguating hash appended when a name is truncated.
const HASH_LEN: usize = 8;
/// The namespace prefix every wrapped tool shares.
const PREFIX: &str = "mcp";

/// A tool discovered on an MCP server, adapted to the host tool contract.
#[derive(Debug)]
pub struct RemoteTool {
    client: Arc<McpClient>,
    name: ToolName,
    definition: ToolDefinition,
    parameters: Schema,
}

impl RemoteTool {
    /// Wraps one remote tool definition as a host tool named
    /// `mcp__<namespace>__<tool>`.
    ///
    /// # Errors
    ///
    /// Returns a core error when the namespaced name is not a valid tool name.
    pub fn new(
        client: Arc<McpClient>,
        namespace: &str,
        definition: ToolDefinition,
    ) -> McpResult<Self> {
        let name =
            ToolName::new(namespaced_name(namespace, &definition.name)).map_err(McpError::from)?;
        let parameters =
            Schema::try_from(definition.input_schema.clone()).unwrap_or_else(|_| object_schema());
        Ok(Self {
            client,
            name,
            definition,
            parameters,
        })
    }

    /// The name the server knows this tool by.
    #[must_use]
    pub fn remote_name(&self) -> &str {
        &self.definition.name
    }

    /// The namespaced name this tool is registered under.
    #[must_use]
    pub fn namespaced_name(&self) -> &ToolName {
        &self.name
    }
}

#[async_trait]
impl AnyFunction for RemoteTool {
    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn description(&self) -> &str {
        self.definition.description.as_deref().unwrap_or("")
    }

    fn parameters_schema(&self) -> &Schema {
        &self.parameters
    }

    async fn execute(&self, context: &ToolContext, arguments: &Value) -> CoreResult<ToolOutput> {
        let arguments = if arguments.is_null() {
            None
        } else {
            Some(arguments.clone())
        };
        let call = self.client.call_tool(&self.definition.name, arguments);
        let result = tokio::select! {
            result = call => result,
            () = context.cancelled() => return Err(rustychain_core::Error::Cancelled),
        };
        let result = result.map_err(|error| error.into_core(self.name.clone()))?;
        result_to_output(result).map_err(|error| error.into_core(self.name.clone()))
    }
}

fn namespaced_name(namespace: &str, tool: &str) -> String {
    let base = format!("{PREFIX}__{}__{}", sanitize(namespace), sanitize(tool));
    if base.len() <= NAME_LIMIT {
        return base;
    }
    let keep = NAME_LIMIT - HASH_LEN - 1;
    let prefix: String = base.chars().take(keep).collect();
    format!("{prefix}_{}", short_hash(&base))
}

fn sanitize(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "_".to_owned()
    } else {
        sanitized
    }
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}").chars().take(HASH_LEN).collect()
}

fn object_schema() -> Schema {
    let mut map = serde_json::Map::new();
    map.insert("type".to_owned(), Value::String("object".to_owned()));
    Schema::from(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ClientOptions;
    use crate::jsonrpc::{Message, Response};
    use crate::protocol::ContentBlock;
    use crate::transport::{ChannelTransport, Transport};
    use serde_json::json;

    #[test]
    fn names_are_namespaced_and_sanitized() {
        assert_eq!(
            namespaced_name("files", "read_file"),
            "mcp__files__read_file"
        );
        assert_eq!(namespaced_name("a.b/c", "x y"), "mcp__a_b_c__x_y");
        assert_eq!(namespaced_name("", ""), "mcp______");
    }

    #[test]
    fn long_names_are_truncated_with_a_stable_hash() {
        let long = "t".repeat(200);
        let name = namespaced_name("server", &long);
        assert!(name.len() <= NAME_LIMIT);
        assert_eq!(name, namespaced_name("server", &long));
        assert_ne!(name, namespaced_name("server", &format!("{long}x")));
    }

    #[tokio::test]
    async fn declarations_expose_the_remote_schema() {
        let definition = ToolDefinition {
            name: "read_file".to_owned(),
            title: None,
            description: Some("Read a file".to_owned()),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
            output_schema: None,
            annotations: None,
            extra: std::collections::BTreeMap::new(),
        };
        let tool = RemoteTool::new(client_stub().await, "files", definition).expect("wrap");
        assert_eq!(tool.name(), "mcp__files__read_file");
        assert_eq!(tool.description(), "Read a file");
        assert!(tool.parameters_schema().as_object().is_some());
        let spec = tool.spec().expect("spec");
        assert_eq!(spec.name.as_str(), "mcp__files__read_file");
    }

    #[tokio::test]
    async fn execute_maps_a_remote_result() {
        let definition = ToolDefinition {
            name: "echo".to_owned(),
            title: None,
            description: None,
            input_schema: json!({ "type": "object" }),
            output_schema: None,
            annotations: None,
            extra: std::collections::BTreeMap::new(),
        };
        let tool = RemoteTool::new(client_stub().await, "srv", definition).expect("wrap");
        let output = tool
            .execute(&ToolContext::new(), &json!({ "x": 1 }))
            .await
            .expect("execute");
        assert_eq!(output.parts.len(), 1);
        assert!(
            matches!(&output.parts[0], rustychain_core::Part::Text(text) if text.text == "stub")
        );
    }

    /// A client whose peer completes the handshake and echoes a fixed text for
    /// every `tools/call`.
    async fn client_stub() -> Arc<McpClient> {
        let (client_transport, server_transport) = ChannelTransport::pair();
        tokio::spawn(async move {
            while let Ok(Some(message)) = server_transport.recv().await {
                if let Message::Request(request) = message {
                    let result = if request.method == "initialize" {
                        json!({
                            "protocolVersion": crate::protocol::PROTOCOL_VERSION,
                            "capabilities": {},
                            "serverInfo": { "name": "stub", "version": "1" }
                        })
                    } else {
                        json!({ "content": [ContentBlock::text("stub")] })
                    };
                    let response = Response::success(request.id, result);
                    let _ = server_transport.send(Message::Response(response)).await;
                }
            }
        });
        Arc::new(
            McpClient::connect(Arc::new(client_transport), ClientOptions::default())
                .await
                .expect("connect"),
        )
    }
}
