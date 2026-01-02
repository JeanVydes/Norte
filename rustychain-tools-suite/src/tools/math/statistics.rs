//! statistics
use rustychain::{
    FunctionDeclaration,
    llm::function::{FnDeclarator, FnExecutor},
};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct StatsArgs {
    pub data: Vec<f64>,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct StatsResult {
    pub mean: f64,
    pub min: f64,
    pub max: f64,
    pub count: usize,
    pub sum: f64,
}

#[derive(Clone, Default)]
pub struct StatisticsTool;

#[async_trait::async_trait]
impl FnExecutor<StatsArgs, StatsResult> for StatisticsTool {
    async fn call(&self, args: StatsArgs) -> rustychain::Result<StatsResult> {
        if args.data.is_empty() {
            return Err(rustychain::Error::Internal("Data vector is empty".into()));
        }

        let count = args.data.len();
        let sum: f64 = args.data.iter().sum();
        let mean = sum / count as f64;
        let min = args.data.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let max = args.data.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));

        Ok(StatsResult {
            mean,
            min,
            max,
            count,
            sum,
        })
    }
}

impl FnDeclarator<StatsArgs, StatsResult> for StatisticsTool {
    fn declare(&self) -> FunctionDeclaration<StatsArgs, StatsResult> {
        FunctionDeclaration {
            name: "statistics_tool",
            description: "Calculates basic statistics (mean, min, max, sum) for a list of numbers.",
            parameters: schema_for!(StatsArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
