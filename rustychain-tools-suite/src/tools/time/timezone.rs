//! clock
//!
//! This module provides tools to retrieve current time and handle timezone conversions.

use chrono::Utc;
use chrono_tz::Tz;
use rustychain::FunctionDeclaration;
use rustychain::llm::function::{FnDeclarator, FnExecutor};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct ClockArgs {
    #[schemars(
        description = "The IANA timezone string (e.g., 'America/New_York', 'Europe/London', 'Asia/Tokyo'). If null, UTC is used."
    )]
    pub timezone: Option<String>,
}

#[derive(Clone, Default)]
pub struct ClockTool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClockResult {
    pub datetime: String,
    pub timezone: String,
    pub unix_timestamp: i64,
}

#[async_trait::async_trait]
impl FnExecutor<ClockArgs, ClockResult> for ClockTool {
    async fn call(&self, args: ClockArgs) -> rustychain::Result<ClockResult> {
        let now = Utc::now();

        let (datetime_str, tz_name) = match args.timezone {
            Some(tz_str) => match tz_str.parse::<Tz>() {
                Ok(tz) => {
                    let localized = now.with_timezone(&tz);
                    (localized.to_rfc3339(), tz_str)
                }
                Err(_) => (now.to_rfc3339(), "UTC".to_string()),
            },
            None => (now.to_rfc3339(), "UTC".to_string()),
        };

        Ok(ClockResult {
            datetime: datetime_str,
            timezone: tz_name,
            unix_timestamp: now.timestamp(),
        })
    }
}

impl FnDeclarator<ClockArgs, ClockResult> for ClockTool {
    fn declare(&self) -> FunctionDeclaration<ClockArgs, ClockResult> {
        FunctionDeclaration {
            name: "clock_tool",
            description: "Retrieves the current date and time. Can provide local time for specific timezones using IANA format.",
            parameters: schema_for!(ClockArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
