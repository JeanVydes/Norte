use rmcp::model::{ClientCapabilities, ClientInfo};
use rmcp::transport::ConfigureCommandExt;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rustychain::prelude::*;
use rustychain_tools_suite::mcp::McpTool;
use std::sync::Arc;

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // MCP is based on RMCP (https://github.com/modelcontextprotocol/rust-sdk/) the Rust SDK for Model Context Protocol
    // Using example https://github.com/modelcontextprotocol/rust-sdk/blob/main/examples/clients/src/streamable_http.rs

    // this is the client info that will be sent to the MCP server, this is your application's identity
    let client_info = ClientInfo {
        protocol_version: Default::default(),
        capabilities: ClientCapabilities::default(),
        client_info: rmcp::model::Implementation {
            name: "test rustychain mcp client".to_string(),
            title: None,
            version: "0.0.1".to_string(),
            website_url: None,
            icons: None,
        },
    };

    let mcp_tool = McpTool::new("custom_mcp_server", client_info.clone(), move || {
        Ok(StreamableHttpClientTransport::from_uri(
            "http://localhost:8000/mcp",
        ))
    });

    let mcp_context7 = McpTool::new(
        "@modelcontextprotocol/server-everything",
        client_info,
        move || {
            Ok(TokioChildProcess::new(
                tokio::process::Command::new("npx").configure(|cmd| {
                    cmd.arg("-y").arg("@modelcontextprotocol/server-everything");
                }),
            )?)
        },
    );

    let llm = Arc::new(
        LLM::builder()
            .set_authorization("your_api_key_here")
            .set_model("gemini-2.5-flash")
            .set_provider(LLMProvider::Google)
            .add_tool(mcp_tool.declare())
            .add_tool(mcp_context7.declare())
            .build()?,
    );

    llm.get_tools().iter().for_each(|tool| {
        println!("Registered tool: {}", tool.name());
    });

    Ok(())
}
