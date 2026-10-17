//! The outbound MCP client.
//!
//! A client owns one transport and one background read loop. Calls are sent
//! directly on the transport and the response is routed back by request id, so
//! many calls may be in flight at once without serializing on the reader.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{Mutex, oneshot};

use rustychain_core::{AnyFunction, ToolRegistry};

use crate::error::{McpError, McpResult};
use crate::jsonrpc::codes::METHOD_NOT_FOUND;
use crate::jsonrpc::{Message, Notification, Request, RequestId, Response, RpcError};
use crate::protocol::{
    CallToolParams, CallToolResult, ClientCapabilities, GetPromptParams, GetPromptResult,
    Implementation, InitializeParams, InitializeResult, ListPromptsResult, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, PaginatedParams, PROTOCOL_VERSION, Prompt, ReadResourceParams,
    ReadResourceResult, Resource, ResourceContents, ResourceTemplate, ServerCapabilities, ToolDefinition,
};
use crate::tool::RemoteTool;
use crate::transport::Transport;

type Pending = Arc<Mutex<HashMap<RequestId, oneshot::Sender<McpResult<Value>>>>>;

/// How a client introduces itself and how patient it is.
#[derive(Clone, Debug, Default)]
pub struct ClientOptions {
    /// The implementation descriptor sent during `initialize`.
    pub client_info: Implementation,
    /// The capabilities advertised during `initialize`.
    pub capabilities: ClientCapabilities,
    /// An optional deadline for a single request. `None` waits indefinitely.
    pub request_timeout: Option<Duration>,
}

impl ClientOptions {
    /// Sets the per-request deadline.
    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }
}

/// A connected MCP client.
pub struct McpClient {
    transport: Arc<dyn Transport>,
    pending: Pending,
    next_id: AtomicI64,
    init: OnceLock<InitializeResult>,
    client_info: Implementation,
    capabilities: ClientCapabilities,
    request_timeout: Option<Duration>,
    reader: tokio::task::JoinHandle<()>,
}

impl McpClient {
    /// Connects to a peer and completes the `initialize` handshake.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the handshake cannot be delivered, and an
    /// RPC or protocol error when the peer rejects or malforms it.
    pub async fn connect(
        transport: Arc<dyn Transport>,
        options: ClientOptions,
    ) -> McpResult<Self> {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader = tokio::spawn(read_loop(Arc::clone(&transport), Arc::clone(&pending)));
        let client = Self {
            transport,
            pending,
            next_id: AtomicI64::new(1),
            init: OnceLock::new(),
            client_info: options.client_info,
            capabilities: options.capabilities,
            request_timeout: options.request_timeout,
            reader,
        };
        if let Err(error) = client.handshake().await {
            client.reader.abort();
            let _ = client.transport.close().await;
            return Err(error);
        }
        Ok(client)
    }

    /// Spawns `program` and connects over its stdio streams.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the process cannot be spawned and whatever
    /// [`McpClient::connect`] would return.
    pub async fn connect_stdio(
        program: impl AsRef<std::ffi::OsStr>,
        args: &[String],
        options: ClientOptions,
    ) -> McpResult<Self> {
        let transport = crate::transport::StdioTransport::spawn(program, args)?;
        Self::connect(Arc::new(transport), options).await
    }

    async fn handshake(&self) -> McpResult<()> {
        let params = InitializeParams {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            capabilities: self.capabilities.clone(),
            client_info: self.client_info.clone(),
        };
        let value = self
            .request("initialize", Some(serde_json::to_value(params)?))
            .await?;
        let result: InitializeResult = serde_json::from_value(value)?;
        self.init
            .set(result)
            .map_err(|_| McpError::Protocol("initialize ran twice".to_owned()))?;
        self.notify("notifications/initialized", None).await
    }

    /// Sends a request and waits for its response.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::Closed`] when the transport ends, a transport error on
    /// write failure, and an RPC error when the peer answers with one.
    pub async fn request(&self, method: &str, params: Option<Value>) -> McpResult<Value> {
        let id = RequestId::Number(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), sender);
        let request = Request::new(id.clone(), method, params);
        if let Err(error) = self.transport.send(Message::Request(request)).await {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        let result = self.await_response(&id, receiver).await;
        if result.is_err() {
            self.pending.lock().await.remove(&id);
        }
        result
    }

    async fn await_response(
        &self,
        id: &RequestId,
        receiver: oneshot::Receiver<McpResult<Value>>,
    ) -> McpResult<Value> {
        match self.request_timeout {
            Some(timeout) => match tokio::time::timeout(timeout, receiver).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err(McpError::Closed),
                Err(_) => Err(McpError::Transport(format!("request {id} timed out"))),
            },
            None => receiver.await.map_err(|_| McpError::Closed)?,
        }
    }

    /// Sends a one-way notification.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the notification cannot be delivered.
    pub async fn notify(&self, method: &str, params: Option<Value>) -> McpResult<()> {
        self.transport
            .send(Message::Notification(Notification::new(method, params)))
            .await
    }

    /// Pings the peer, verifying the link is alive.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the peer rejects the ping.
    pub async fn ping(&self) -> McpResult<()> {
        self.request("ping", None).await.map(|_| ())
    }

    /// Lists every tool the server exposes, following pagination cursors.
    ///
    /// # Errors
    ///
    /// Returns a transport or RPC error when a page cannot be fetched or decoded.
    pub async fn list_tools(&self) -> McpResult<Vec<ToolDefinition>> {
        self.paginate("tools/list", |value| {
            let page: ListToolsResult = serde_json::from_value(value)?;
            Ok((page.tools, page.next_cursor))
        })
        .await
    }

    /// Invokes a remote tool.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the request itself fails. A tool that ran and
    /// failed comes back as a successful [`CallToolResult`] with `is_error` set.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Option<Value>,
    ) -> McpResult<CallToolResult> {
        let params = CallToolParams {
            name: name.to_owned(),
            arguments,
        };
        let value = self
            .request("tools/call", Some(serde_json::to_value(params)?))
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Lists every resource the server exposes.
    ///
    /// # Errors
    ///
    /// Returns a transport or RPC error when a page cannot be fetched or decoded.
    pub async fn list_resources(&self) -> McpResult<Vec<Resource>> {
        self.paginate("resources/list", |value| {
            let page: ListResourcesResult = serde_json::from_value(value)?;
            Ok((page.resources, page.next_cursor))
        })
        .await
    }

    /// Lists every resource template the server exposes.
    ///
    /// # Errors
    ///
    /// Returns a transport or RPC error when a page cannot be fetched or decoded.
    pub async fn list_resource_templates(&self) -> McpResult<Vec<ResourceTemplate>> {
        self.paginate("resources/templates/list", |value| {
            let page: ListResourceTemplatesResult = serde_json::from_value(value)?;
            Ok((page.resource_templates, page.next_cursor))
        })
        .await
    }

    /// Reads a resource by URI.
    ///
    /// # Errors
    ///
    /// Returns a transport or RPC error when the resource cannot be read.
    pub async fn read_resource(&self, uri: &str) -> McpResult<Vec<ResourceContents>> {
        let params = serde_json::to_value(ReadResourceParams { uri: uri.to_owned() })?;
        let value = self.request("resources/read", Some(params)).await?;
        let result: ReadResourceResult = serde_json::from_value(value)?;
        Ok(result.contents)
    }

    /// Lists every prompt the server exposes.
    ///
    /// # Errors
    ///
    /// Returns a transport or RPC error when a page cannot be fetched or decoded.
    pub async fn list_prompts(&self) -> McpResult<Vec<Prompt>> {
        self.paginate("prompts/list", |value| {
            let page: ListPromptsResult = serde_json::from_value(value)?;
            Ok((page.prompts, page.next_cursor))
        })
        .await
    }

    /// Expands a prompt with its arguments.
    ///
    /// # Errors
    ///
    /// Returns a transport or RPC error when the prompt cannot be fetched.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<BTreeMap<String, String>>,
    ) -> McpResult<GetPromptResult> {
        let params = serde_json::to_value(GetPromptParams {
            name: name.to_owned(),
            arguments,
        })?;
        let value = self.request("prompts/get", Some(params)).await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Wraps every remote tool as a namespace host tool.
    ///
    /// Each remote tool becomes one [`AnyFunction`] named
    /// `mcp__<namespace>__<tool>`, so per-tool schemas, approvals and audit
    /// records survive intact.
    ///
    /// # Errors
    ///
    /// Returns a transport or RPC error when the tool list cannot be fetched, and
    /// a protocol error when a remote tool name cannot be mapped.
    pub async fn tools(self: &Arc<Self>, namespace: &str) -> McpResult<Vec<Arc<dyn AnyFunction>>> {
        let definitions = self.list_tools().await?;
        let mut tools: Vec<Arc<dyn AnyFunction>> = Vec::with_capacity(definitions.len());
        for definition in definitions {
            let tool = RemoteTool::new(Arc::clone(self), namespace, definition)?;
            tools.push(Arc::new(tool));
        }
        Ok(tools)
    }

    /// Fetches the remote tools and registers them in a host registry.
    ///
    /// # Errors
    ///
    /// Returns a transport or RPC error when the tool list cannot be fetched, and
    /// a core error when a namespaced name collides with an existing tool.
    pub async fn register_into(
        self: &Arc<Self>,
        registry: &mut ToolRegistry,
        namespace: &str,
    ) -> McpResult<usize> {
        let tools = self.tools(namespace).await?;
        let count = tools.len();
        for tool in tools {
            registry.register(tool)?;
        }
        Ok(count)
    }

    /// The server descriptor returned by `initialize`.
    #[must_use]
    pub fn server_info(&self) -> Option<&Implementation> {
        self.init.get().map(|init| &init.server_info)
    }

    /// The capabilities the server advertised.
    #[must_use]
    pub fn capabilities(&self) -> ServerCapabilities {
        self.init
            .get()
            .map(|init| init.capabilities.clone())
            .unwrap_or_default()
    }

    /// The protocol revision the server settled on.
    #[must_use]
    pub fn protocol_version(&self) -> Option<&str> {
        self.init.get().map(|init| init.protocol_version.as_str())
    }

    /// The optional usage guidance the server provided.
    #[must_use]
    pub fn instructions(&self) -> Option<&str> {
        self.init.get().and_then(|init| init.instructions.as_deref())
    }

    async fn paginate<T, F>(&self, method: &str, mut extract: F) -> McpResult<Vec<T>>
    where
        F: FnMut(Value) -> McpResult<(Vec<T>, Option<String>)>,
    {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(cursor) => Some(serde_json::to_value(PaginatedParams {
                    cursor: Some(cursor.clone()),
                })?),
                None => None,
            };
            let value = self.request(method, params).await?;
            let (mut page, next) = extract(value)?;
            items.append(&mut page);
            match next {
                Some(next) => cursor = Some(next),
                None => return Ok(items),
            }
        }
    }
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("server_info", &self.server_info())
            .field("protocol_version", &self.protocol_version())
            .finish_non_exhaustive()
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

async fn read_loop(transport: Arc<dyn Transport>, pending: Pending) {
    loop {
        match transport.recv().await {
            Ok(Some(Message::Response(response))) => {
                if let Some(sender) = pending.lock().await.remove(&response.id) {
                    let _ = sender.send(response.into_result());
                } else {
                    tracing::debug!(id = %response.id, "mcp response for an unknown request");
                }
            }
            Ok(Some(Message::Request(request))) => {
                tracing::debug!(method = %request.method, "mcp server request is not handled");
                let error = RpcError::new(
                    METHOD_NOT_FOUND,
                    format!("unsupported client method: {}", request.method),
                    None,
                );
                let answer = Message::Response(Response::error(request.id.clone(), error));
                if let Err(error) = transport.send(answer).await {
                    tracing::debug!(%error, "failed to answer an mcp server request");
                }
            }
            Ok(Some(Message::Notification(notification))) => {
                tracing::debug!(method = %notification.method, "mcp server notification");
            }
            Ok(None) => break,
            Err(error) => {
                tracing::debug!(%error, "mcp read loop stopped");
                break;
            }
        }
    }
    pending.lock().await.clear();
}
