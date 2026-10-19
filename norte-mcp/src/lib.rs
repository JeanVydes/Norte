#![forbid(unsafe_code)]
#![doc = "Model Context Protocol client and server for `Norte`.

MCP is how a `Norte` agent reaches tools, resources and prompts that live in
another process. This crate keeps the protocol on the edges and the neutral core
untouched:

- **Outbound** — [`McpClient`] performs the `initialize` handshake and wraps every
  remote tool as one host tool via [`McpClient::tools`] /
  [`McpClient::register_into`]. Each remote tool becomes a single
  [`AnyFunction`](norte_core::AnyFunction) named `mcp__<server>__<tool>`, so
  per-tool schemas, approval policy and audit records survive. Resources and
  prompts are read through [`McpClient::read_resource`] and
  [`McpClient::get_prompt`].
- **Inbound** — [`McpServer`] publishes a host
  [`ToolRegistry`](norte_core::ToolRegistry) over any [`Transport`], so the
  same tools a local model sees can be exposed to other MCP clients.

Two transports ship: [`StdioTransport`] for a child process and
[`ChannelTransport`] for in-process embedding and tests. Both implement
[`Transport`], which a host can implement for any other channel.

The crate carries no model catalog and no vendor table: everything is
host-supplied data and wire mapping."]

pub mod client;
pub mod convert;
pub mod error;
pub mod jsonrpc;
pub mod protocol;
pub mod server;
pub mod tool;
pub mod transport;

pub use client::{ClientOptions, McpClient};
pub use error::{McpError, McpResult};
pub use jsonrpc::{Message, Notification, Request, RequestId, Response, RpcError};
pub use protocol::{
    CallToolParams, CallToolResult, ClientCapabilities, ContentBlock, GetPromptResult,
    Implementation, InitializeParams, InitializeResult, PROTOCOL_VERSION, Prompt, PromptArgument,
    PromptMessage, PromptRole, Resource, ResourceContents, ResourceTemplate, ServerCapabilities,
    ToolDefinition,
};
pub use server::McpServer;
pub use tool::RemoteTool;
pub use transport::{ChannelTransport, StdioTransport, Transport};
