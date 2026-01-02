use async_trait::async_trait;
use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParam, CallToolResult, ClientInfo, ListToolsResult},
    transport::IntoTransport,
};
use rustychain::{FnDeclarator, FunctionDeclaration, llm::function::FnExecutor};
use std::sync::Arc;

#[derive(Clone, serde::Deserialize, serde::Serialize, Debug, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpAction {
    CallTool,
    ListTools,
}

#[derive(Clone, serde::Deserialize, serde::Serialize, Debug, schemars::JsonSchema)]
pub struct McpArgs {
    #[schemars(description = "Action to perform on the MCP server")]
    pub action: McpAction,
    #[schemars(description = "Parameters for calling a tool, required if action is CallTool")]
    pub tool_params: Option<CallToolRequestParam>,
    #[schemars(
        description = "Pagination parameters for the request, only applies to ListTools action"
    )]
    pub paginated_request_param: Option<rmcp::model::PaginatedRequestParam>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize, Debug, schemars::JsonSchema)]
pub struct McpResult {
    #[schemars(description = "Result of listing tools, if action was ListTools")]
    pub list_tools_result: Option<ListToolsResult>,
    #[schemars(description = "Result of calling a tool, if action was CallTool")]
    pub tool_result: Option<CallToolResult>,
}

pub struct McpTool<F, T, E, A>
where
    F: Fn() -> rustychain::Result<T> + Send + Sync + 'static,
    T: IntoTransport<RoleClient, E, A> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
    A: Send + Sync + 'static,
{
    name: String,
    server: ClientInfo,
    transport_factory: Arc<F>,
    _transport: std::marker::PhantomData<fn(T) -> T>,
    _error: std::marker::PhantomData<fn(E) -> E>,
    _args: std::marker::PhantomData<fn(A) -> A>,
}

unsafe impl<F, T, E, A> Send for McpTool<F, T, E, A>
where
    F: Fn() -> rustychain::Result<T> + Send + Sync + 'static,
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
    A: Send + Sync + 'static,
{
}

unsafe impl<F, T, E, A> Sync for McpTool<F, T, E, A>
where
    F: Fn() -> rustychain::Result<T> + Send + Sync + 'static,
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
    A: Send + Sync + 'static,
{
}

impl<F, T, E, A> Clone for McpTool<F, T, E, A>
where
    F: Fn() -> rustychain::Result<T> + Send + Sync + 'static,
    T: IntoTransport<RoleClient, E, A> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
    A: Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            server: self.server.clone(),
            transport_factory: Arc::clone(&self.transport_factory),
            _transport: std::marker::PhantomData,
            _error: std::marker::PhantomData,
            _args: std::marker::PhantomData,
        }
    }
}

impl<F, T, E, A> McpTool<F, T, E, A>
where
    F: Fn() -> rustychain::Result<T> + Send + Sync + 'static,
    T: IntoTransport<RoleClient, E, A> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
    A: Send + Sync + 'static,
{
    pub fn new(self_name: impl ToString, server: ClientInfo, transport_factory: F) -> Self {
        Self {
            name: self_name.to_string(),
            server,
            transport_factory: Arc::new(transport_factory),
            _transport: std::marker::PhantomData,
            _error: std::marker::PhantomData,
            _args: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<F, T, E, A> FnExecutor<McpArgs, McpResult> for McpTool<F, T, E, A>
where
    F: Fn() -> rustychain::Result<T> + Send + Sync + 'static,
    T: IntoTransport<RoleClient, E, A> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
    A: Send + Sync + 'static,
{
    async fn call(&self, args: McpArgs) -> rustychain::Result<McpResult> {
        let transport = (self.transport_factory)()?;

        let client = self.server.clone().serve(transport).await.map_err(|e| {
            rustychain::error::Error::Generic(format!("MCP Tool client creation failed: {}", e))
        })?;

        match args.action {
            McpAction::ListTools => client
                .list_tools(args.paginated_request_param.clone())
                .await
                .map_err(|e| {
                    rustychain::error::Error::Generic(format!("MCP Tool list failed: {}", e))
                })
                .map(|res| McpResult {
                    list_tools_result: Some(res),
                    tool_result: None,
                }),
            McpAction::CallTool => {
                let params = args.tool_params.ok_or_else(|| {
                    rustychain::error::Error::Generic(
                        "MCP Tool CallTool action requires tool_params".to_string(),
                    )
                })?;

                client
                    .call_tool(params)
                    .await
                    .map_err(|e| {
                        rustychain::error::Error::Generic(format!("MCP Tool call failed: {}", e))
                    })
                    .map(|res| McpResult {
                        list_tools_result: None,
                        tool_result: Some(res),
                    })
            }
        }
    }
}

impl<F, T, E, A> FnDeclarator<McpArgs, McpResult> for McpTool<F, T, E, A>
where
    F: Fn() -> rustychain::Result<T> + Send + Sync + 'static,
    T: IntoTransport<RoleClient, E, A> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
    A: Send + Sync + 'static,
{
    fn declare(&self) -> FunctionDeclaration<McpArgs, McpResult> {
        let name: &'static str = Box::leak(self.name.clone().into_boxed_str());

        FunctionDeclaration {
            name,
            description: "Tool to interact with an MCP server",
            parameters: schemars::schema_for!(McpArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
