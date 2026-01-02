//! math
//!
//! Tool for evaluating mathematical expressions using meval.

use rustychain::FunctionDeclaration;
use rustychain::llm::function::{FnDeclarator, FnExecutor};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct MathArgs {
    #[schemars(description = "The mathematical expression to evaluate (e.g., 'sqrt(144) + 2^3').")]
    pub expression: String,
}

#[derive(Clone, Default)]
pub struct MathTool;

#[async_trait::async_trait]
impl FnExecutor<MathArgs, f64> for MathTool {
    async fn call(&self, args: MathArgs) -> rustychain::Result<f64> {
        // meval supports basic ops, trig, powers, and constants
        meval::eval_str(&args.expression).map_err(|e| {
            rustychain::Error::Internal(format!("Math evaluation error: {}", e).into())
        })
    }
}

impl FnDeclarator<MathArgs, f64> for MathTool {
    fn declare(&self) -> FunctionDeclaration<MathArgs, f64> {
        FunctionDeclaration {
            name: "math_tool",
            description: "Evaluates mathematical expressions. Supports +, -, *, /, ^, sqrt(), sin(), cos(), tan(), pi, e, etc.",
            parameters: schema_for!(MathArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
