//! wait
//!
//! This module provides a non-blocking sleep tool for the agent.

use rustychain::FunctionDeclaration;
use rustychain::llm::function::{FnDeclarator, FnExecutor};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::time::{Duration, sleep};

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct WaitArgs {
    #[schemars(description = "The number of seconds to wait/sleep.")]
    pub seconds: u64,
    #[schemars(description = "A brief reason why the agent is waiting.")]
    pub reason: Option<String>,
}

#[derive(Clone, Default)]
pub struct WaitTool;

impl WaitTool {
    pub fn new() -> Self {
        Self {}
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitResult {
    pub waited_seconds: u64,
    pub status: String,
}

#[async_trait::async_trait]
impl FnExecutor<WaitArgs, WaitResult> for WaitTool {
    async fn call(&self, args: WaitArgs) -> rustychain::Result<WaitResult> {
        sleep(Duration::from_secs(args.seconds)).await;

        Ok(WaitResult {
            waited_seconds: args.seconds,
            status: "Completed".to_string(),
        })
    }
}

impl FnDeclarator<WaitArgs, WaitResult> for WaitTool {
    fn declare(&self) -> FunctionDeclaration<WaitArgs, WaitResult> {
        FunctionDeclaration {
            name: "wait_tool",
            description: "Pauses execution for a specified number of seconds. Use this when you need to wait for a process to complete or rate-limit actions.",
            parameters: schema_for!(WaitArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
