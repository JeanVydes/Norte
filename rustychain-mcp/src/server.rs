//! The inbound MCP server.
//!
//! An [`McpServer`] wraps a host [`ToolRegistry`] and answers the tool methods of
//! the protocol. Registration stays where it belongs — the registry built when
//! the agent is constructed — so this type is a thin, stateless adapter that lets
//! the very same tools a local model sees be published to any MCP client.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;

use rustychain_core::{CallId, Error as CoreError, ToolContext, ToolName, ToolRegistry, ToolSpec};

use crate::convert::output_to_result;
use crate::error::McpResult;
use crate::jsonrpc::codes::INVALID_PARAMS;
use crate::jsonrpc::{Message, Request, Response, RpcError};
use crate::protocol::{
    CallToolParams, CallToolResult, Implementation, InitializeParams, InitializeResult,
    ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PROTOCOL_VERSION, PaginatedParams, ServerCapabilities, ToolDefinition, ToolsCapability,
};
use crate::transport::Transport;

/// The default number of tools returned in one `tools/list` page.
const DEFAULT_PAGE_SIZE: usize = 100;

/// Serves a [`ToolRegistry`] over any [`Transport`].
pub struct McpServer {
    registry: Arc<ToolRegistry>,
    server_info: Implementation,
    instructions: Option<String>,
    page_size: usize,
}

impl McpServer {
    /// Wraps a registry for serving.
    #[must_use]
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self {
            registry,
            server_info: Implementation::default(),
            instructions: None,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }

    /// Overrides the implementation descriptor reported to clients.
    #[must_use]
    pub fn with_server_info(mut self, server_info: Implementation) -> Self {
        self.server_info = server_info;
        self
    }

    /// Sets optional usage guidance reported to clients.
    #[must_use]
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Sets the maximum tools per `tools/list` page.
    #[must_use]
    pub fn with_page_size(mut self, page_size: usize) -> Self {
        self.page_size = page_size.max(1);
        self
    }

    /// Serves requests until the transport ends.
    ///
    /// # Errors
    ///
    /// Returns a transport error when reading or writing fails, and the registry's
    /// error only when a response cannot be encoded.
    pub async fn serve(&self, transport: Arc<dyn Transport>) -> McpResult<()> {
        loop {
            let Some(message) = transport.recv().await? else {
                break;
            };
            match message {
                Message::Request(request) => {
                    let response = self.dispatch(&request).await;
                    transport.send(Message::Response(response)).await?;
                }
                Message::Notification(notification) => {
                    tracing::debug!(method = %notification.method, "mcp client notification");
                }
                Message::Response(response) => {
                    tracing::debug!(id = %response.id, "unexpected mcp response on the server side");
                }
            }
        }
        Ok(())
    }

    async fn dispatch(&self, request: &Request) -> Response {
        let id = request.id.clone();
        match self.handle(&request.method, request.params.clone()).await {
            Ok(result) => Response::success(id, result),
            Err(error) => Response::error(id, RpcError::from(error)),
        }
    }

    async fn handle(&self, method: &str, params: Option<Value>) -> McpResult<Value> {
        match method {
            "initialize" => self.initialize(params),
            "ping" => Ok(Value::Object(serde_json::Map::new())),
            "tools/list" => self.tools_list(params),
            "tools/call" => self.tools_call(params).await,
            "resources/list" => Ok(serde_json::to_value(ListResourcesResult {
                resources: Vec::new(),
                next_cursor: None,
            })?),
            "resources/templates/list" => Ok(serde_json::to_value(ListResourceTemplatesResult {
                resource_templates: Vec::new(),
                next_cursor: None,
            })?),
            "prompts/list" => Ok(serde_json::to_value(ListPromptsResult {
                prompts: Vec::new(),
                next_cursor: None,
            })?),
            other => Err(crate::error::McpError::Unsupported(format!(
                "method not found: {other}"
            ))),
        }
    }

    fn capabilities(&self) -> ServerCapabilities {
        ServerCapabilities {
            tools: (!self.registry.is_empty()).then(ToolsCapability::default),
            ..ServerCapabilities::default()
        }
    }

    fn initialize(&self, params: Option<Value>) -> McpResult<Value> {
        let _: InitializeParams = decode_params(params)?;
        let result = InitializeResult {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            capabilities: self.capabilities(),
            server_info: self.server_info.clone(),
            instructions: self.instructions.clone(),
            extra: BTreeMap::new(),
        };
        Ok(serde_json::to_value(result)?)
    }

    fn tools_list(&self, params: Option<Value>) -> McpResult<Value> {
        let PaginatedParams { cursor } = match params {
            Some(value) => serde_json::from_value(value)
                .map_err(|error| crate::error::McpError::rpc(INVALID_PARAMS, error.to_string()))?,
            None => PaginatedParams::default(),
        };
        let specs = self.registry.specs();
        let start = match cursor.as_deref() {
            Some(cursor) => specs
                .iter()
                .position(|spec| spec.name.as_str() == cursor)
                .map_or(0, |index| index + 1),
            None => 0,
        };
        let end = (start + self.page_size).min(specs.len());
        let tools = specs[start..end].iter().map(spec_to_definition).collect();
        let next_cursor = if end < specs.len() && end > start {
            Some(specs[end - 1].name.as_str().to_owned())
        } else {
            None
        };
        Ok(serde_json::to_value(ListToolsResult {
            tools,
            next_cursor,
        })?)
    }

    async fn tools_call(&self, params: Option<Value>) -> McpResult<Value> {
        let params: CallToolParams = decode_params(params)?;
        let name = ToolName::new(params.name.clone())
            .map_err(|error| crate::error::McpError::rpc(INVALID_PARAMS, error.to_string()))?;
        let arguments = params
            .arguments
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        let context = ToolContext::new().with_call(CallId::new());
        match self.registry.execute(&context, &name, arguments).await {
            Ok(output) => Ok(serde_json::to_value(output_to_result(output))?),
            Err(error @ (CoreError::ToolNotFound(_) | CoreError::InvalidToolArguments { .. })) => {
                Err(crate::error::McpError::rpc(
                    INVALID_PARAMS,
                    error.to_string(),
                ))
            }
            Err(error) => Ok(serde_json::to_value(CallToolResult::error(
                error.to_string(),
            ))?),
        }
    }
}

impl std::fmt::Debug for McpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServer")
            .field("server_info", &self.server_info)
            .field("tools", &self.registry.len())
            .field("page_size", &self.page_size)
            .finish_non_exhaustive()
    }
}

fn spec_to_definition(spec: &ToolSpec) -> ToolDefinition {
    let mut extra = BTreeMap::new();
    if let Some(approval) = &spec.approval {
        extra.insert(
            "_meta".to_owned(),
            serde_json::json!({ "rustychain/approval": approval }),
        );
    }
    ToolDefinition {
        name: spec.name.as_str().to_owned(),
        title: None,
        description: Some(spec.description.clone()),
        input_schema: spec.parameters.clone(),
        output_schema: None,
        annotations: None,
        extra,
    }
}

fn decode_params<T: DeserializeOwned>(params: Option<Value>) -> McpResult<T> {
    let value = params.unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    serde_json::from_value(value)
        .map_err(|error| crate::error::McpError::rpc(INVALID_PARAMS, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ClientOptions, McpClient};
    use crate::jsonrpc::codes::METHOD_NOT_FOUND;
    use crate::jsonrpc::{Notification, RequestId};
    use crate::transport::ChannelTransport;
    use async_trait::async_trait;
    use rustychain_core::AnyFunction;
    use schemars::Schema;
    use serde_json::json;
    use std::sync::OnceLock;

    #[derive(Debug)]
    struct EchoTool;

    #[async_trait]
    impl AnyFunction for EchoTool {
        fn name(&self) -> &'static str {
            "echo"
        }

        fn description(&self) -> &'static str {
            "Echo the raw arguments as text"
        }

        fn parameters_schema(&self) -> &Schema {
            static SCHEMA: OnceLock<Schema> = OnceLock::new();
            SCHEMA.get_or_init(|| {
                serde_json::from_value(json!({ "type": "object" })).expect("schema")
            })
        }

        async fn execute(
            &self,
            _context: &ToolContext,
            arguments: &Value,
        ) -> rustychain_core::Result<rustychain_core::ToolOutput> {
            Ok(rustychain_core::ToolOutput::text(arguments.to_string()))
        }
    }

    #[derive(Debug)]
    struct BoomTool;

    #[async_trait]
    impl AnyFunction for BoomTool {
        fn name(&self) -> &'static str {
            "boom"
        }

        fn description(&self) -> &'static str {
            "Always fails"
        }

        fn parameters_schema(&self) -> &Schema {
            static SCHEMA: OnceLock<Schema> = OnceLock::new();
            SCHEMA.get_or_init(|| {
                serde_json::from_value(json!({ "type": "object" })).expect("schema")
            })
        }

        async fn execute(
            &self,
            _context: &ToolContext,
            _arguments: &Value,
        ) -> rustychain_core::Result<rustychain_core::ToolOutput> {
            Err(rustychain_core::Error::Other("kaboom".to_owned()))
        }
    }

    fn registry_with(tools: Vec<Arc<dyn AnyFunction>>) -> Arc<ToolRegistry> {
        Arc::new(ToolRegistry::with_tools(tools).expect("registry"))
    }

    async fn connect(server: McpServer) -> (Arc<McpClient>, tokio::task::JoinHandle<()>) {
        let (client_transport, server_transport) = ChannelTransport::pair();
        let handle = tokio::spawn(async move {
            let _ = server.serve(Arc::new(server_transport)).await;
        });
        let client = Arc::new(
            McpClient::connect(Arc::new(client_transport), ClientOptions::default())
                .await
                .expect("connect"),
        );
        (client, handle)
    }

    #[tokio::test]
    async fn round_trip_over_a_channel_transport() {
        let server = McpServer::new(registry_with(vec![Arc::new(EchoTool)]));
        let (client, handle) = connect(server).await;

        assert_eq!(client.protocol_version(), Some(PROTOCOL_VERSION));
        assert!(client.capabilities().tools.is_some());
        assert_eq!(
            client.server_info().map(|info| info.name.as_str()),
            Some("rustychain-mcp")
        );

        let tools = client.tools("local").await.expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "mcp__local__echo");
        assert_eq!(tools[0].description(), "Echo the raw arguments as text");

        let output = tools[0]
            .execute(&ToolContext::new(), &json!({ "hello": "world" }))
            .await
            .expect("execute");
        assert!(
            matches!(&output.parts[0], rustychain_core::Part::Text(text) if text.text.contains("world"))
        );

        client.ping().await.expect("ping");
        handle.abort();
    }

    #[tokio::test]
    async fn tool_failures_are_reported_as_error_results() {
        let server = McpServer::new(registry_with(vec![Arc::new(BoomTool)]));
        let (client, handle) = connect(server).await;
        let result = client
            .call_tool("boom", Some(json!({})))
            .await
            .expect("call returns a result");
        assert!(result.is_error);
        assert!(
            result.content[0]
                .text
                .as_deref()
                .is_some_and(|text| text.contains("kaboom"))
        );
        handle.abort();
    }

    #[tokio::test]
    async fn unknown_methods_and_arguments_are_rpc_errors() {
        let server = McpServer::new(registry_with(vec![Arc::new(EchoTool)]));
        let (client, handle) = connect(server).await;

        let error = client
            .request("does/not/exist", None)
            .await
            .expect_err("unknown");
        assert!(error.is_method_not_found());

        let error = client
            .call_tool("missing", Some(json!({})))
            .await
            .expect_err("invalid params");
        assert_eq!(error.code(), Some(INVALID_PARAMS));
        assert_eq!(METHOD_NOT_FOUND, -32601);

        handle.abort();
    }

    #[tokio::test]
    async fn tools_list_paginates() {
        let server = McpServer::new(registry_with(vec![Arc::new(EchoTool), Arc::new(BoomTool)]))
            .with_page_size(1);
        let (client, handle) = connect(server).await;
        let tools = client.list_tools().await.expect("tools");
        let mut names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["boom", "echo"]);
        assert_eq!(client.list_resources().await.expect("resources").len(), 0);
        assert_eq!(client.list_prompts().await.expect("prompts").len(), 0);
        handle.abort();
    }

    #[tokio::test]
    async fn invalid_initialize_params_are_rejected() {
        let server = McpServer::new(registry_with(vec![]));
        let (client_transport, server_transport) = ChannelTransport::pair();
        let handle = tokio::spawn(async move {
            let _ = server.serve(Arc::new(server_transport)).await;
        });

        let request = Request::new(RequestId::Number(1), "initialize", None);
        client_transport
            .send(Message::Request(request))
            .await
            .expect("send");
        let response = match client_transport.recv().await.expect("recv") {
            Some(Message::Response(response)) => response,
            other => panic!("unexpected message: {other:?}"),
        };
        let error = response.into_result().expect_err("invalid params");
        assert_eq!(error.code(), Some(INVALID_PARAMS));

        client_transport
            .send(Message::Notification(Notification::new(
                "notifications/initialized",
                None,
            )))
            .await
            .expect("send notification");
        drop(client_transport);
        handle.await.expect("server task");
    }
}
