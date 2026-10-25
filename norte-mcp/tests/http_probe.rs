//! Prove the Streamable HTTP transport against a real server.
//!
//! Run with the server up:
//!   npx @modelcontextprotocol/server-everything streamableHttp
//! then:
//!   cargo test --manifest-path /home/Jean/norte/Cargo.toml -p norte-mcp
//!     --test `http_probe` -- --ignored --nocapture

use std::sync::Arc;
use std::time::Duration;

use norte_core::ToolContext;
use norte_mcp::{ClientOptions, HttpTransport, McpClient};
use serde_json::json;

async fn connect() -> Arc<McpClient> {
    let transport = HttpTransport::new("http://localhost:3001/mcp")
        .expect("transport")
        .with_timeout(Duration::from_secs(30));
    Arc::new(
        McpClient::connect(
            Arc::new(transport),
            ClientOptions::default().with_request_timeout(Duration::from_secs(30)),
        )
        .await
        .expect("handshake"),
    )
}

#[tokio::test]
#[ignore = "needs the reference server in streamableHttp mode"]
async fn a_real_http_server_handshakes_and_lists_its_tools() {
    let client = connect().await;
    println!("PROBE server_info: {:?}", client.server_info());
    println!("PROBE protocol: {:?}", client.protocol_version());

    let definitions = client.list_tools().await.expect("list tools");
    println!("PROBE {} tools over HTTP", definitions.len());
    assert!(!definitions.is_empty(), "a real server exposes tools");
}

#[tokio::test]
#[ignore = "needs the reference server in streamableHttp mode"]
async fn a_remote_tool_runs_over_http() {
    let client = connect().await;
    let tools = client.tools("everything").await.expect("wrap tools");
    let echo = tools
        .iter()
        .find(|tool| tool.name() == "mcp__everything__echo")
        .expect("the echo tool");
    let output = echo
        .execute(&ToolContext::new(), &json!({ "message": "over http" }))
        .await
        .expect("execute");
    let text = output
        .parts
        .iter()
        .filter_map(norte_core::Part::as_text)
        .collect::<Vec<_>>()
        .join("\n");
    println!("PROBE echo returned: {text:?}");
    assert!(text.contains("over http"), "{text}");
}
