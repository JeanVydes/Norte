//! diff
use rustychain::FunctionDeclaration;
use rustychain::llm::function::{FnDeclarator, FnExecutor};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct DiffArgs {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Clone, Default)]
pub struct DiffTool;

impl DiffTool {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait::async_trait]
impl FnExecutor<DiffArgs, String> for DiffTool {
    async fn call(&self, args: DiffArgs) -> rustychain::Result<String> {
        let diff = TextDiff::from_lines(&args.old_text, &args.new_text);
        let mut result = String::new();

        for change in diff.iter_all_changes() {
            let sign = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };
            result.push_str(&format!("{}{}", sign, change));
        }

        Ok(result)
    }
}

impl FnDeclarator<DiffArgs, String> for DiffTool {
    fn declare(&self) -> FunctionDeclaration<DiffArgs, String> {
        FunctionDeclaration {
            name: "diff_tool",
            description: "Compares two strings and returns the differences in unified format.",
            parameters: schema_for!(DiffArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
