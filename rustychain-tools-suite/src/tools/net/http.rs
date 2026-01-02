//! http
//!
//! This module provides a tool for making HTTP requests using reqwest.

use rustychain::FunctionDeclaration;
use rustychain::llm::function::{FnDeclarator, FnExecutor};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct HttpArgs {
    #[schemars(description = "The HTTP method to use (GET, POST, PUT, DELETE, etc.).")]
    pub method: String,
    #[schemars(description = "The full URL for the request.")]
    pub url: String,
    #[schemars(description = "Optional JSON body for the request.")]
    pub body: Option<serde_json::Value>,
    #[schemars(description = "Optional headers for the request.")]
    pub headers: Option<HashMap<String, String>>,
}

#[derive(Clone, Default)]
pub struct HttpTool {
    client: reqwest::Client,
}

impl HttpTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResult {
    pub status: u16,
    pub response: serde_json::Value,
}

#[async_trait::async_trait]
impl FnExecutor<HttpArgs, HttpResult> for HttpTool {
    async fn call(&self, args: HttpArgs) -> rustychain::Result<HttpResult> {
        let method = reqwest::Method::from_bytes(args.method.to_uppercase().as_bytes())
            .map_err(|e| rustychain::Error::Internal(e.into()))?;

        let mut rb = self.client.request(method, &args.url);

        if let Some(headers) = args.headers {
            for (k, v) in headers {
                rb = rb.header(k, v);
            }
        }

        if let Some(body) = args.body {
            rb = rb.json(&body);
        }

        let res = rb.send().await.map_err(|e| {
            rustychain::Error::Internal(format!("HTTP request failed: {}", e).into())
        })?;
        let status = res.status().as_u16();

        // Attempt to parse as JSON, fallback to string if not possible
        let body_val = match res.json::<serde_json::Value>().await {
            Ok(v) => v,
            Err(_) => serde_json::json!({"error": "Response was not valid JSON"}),
        };

        Ok(HttpResult {
            status,
            response: body_val,
        })
    }
}

impl FnDeclarator<HttpArgs, HttpResult> for HttpTool {
    fn declare(&self) -> FunctionDeclaration<HttpArgs, HttpResult> {
        FunctionDeclaration {
            name: "http_tool",
            description: "Makes HTTP requests to external APIs or websites. Support GET, POST, and JSON bodies.",
            parameters: schema_for!(HttpArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
