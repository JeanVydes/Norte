//! regex
use regex::Regex;
use rustychain::FunctionDeclaration;
use rustychain::llm::function::{FnDeclarator, FnExecutor};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct RegexArgs {
    #[schemars(description = "Action to perform: 'find' or 'replace'.")]
    pub action: String,
    #[schemars(description = "The regex pattern (e.g., r'(\\d{4})-(\\d{2})').")]
    pub pattern: String,
    pub text: String,
    #[schemars(description = "Replacement string (only for 'replace' action).")]
    pub replacement: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RegexMatch {
    pub full_match: String,
    pub groups: Vec<String>,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Default)]
pub struct RegexTool;

impl RegexTool {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait::async_trait]
impl FnExecutor<RegexArgs, serde_json::Value> for RegexTool {
    async fn call(&self, args: RegexArgs) -> rustychain::Result<serde_json::Value> {
        let re = Regex::new(&args.pattern)
            .map_err(|e| rustychain::Error::Input(format!("Invalid regex pattern: {}", e)))?;

        match args.action.to_lowercase().as_str() {
            "find" => {
                let matches: Vec<RegexMatch> = re
                    .captures_iter(&args.text)
                    .map(|cap| RegexMatch {
                        full_match: cap[0].to_string(),
                        groups: cap
                            .iter()
                            .skip(1)
                            .filter_map(|m| m.map(|g| g.as_str().to_string()))
                            .collect(),
                        start: cap.get(0).unwrap().start(),
                        end: cap.get(0).unwrap().end(),
                    })
                    .collect();
                Ok(serde_json::to_value(matches).unwrap())
            }
            "replace" => {
                let rep = args.replacement.ok_or_else(|| {
                    rustychain::Error::Input("Replacement string required".into())
                })?;
                let result = re.replace_all(&args.text, rep.as_str()).into_owned();
                Ok(serde_json::json!({ "result": result }))
            }
            _ => Err(rustychain::Error::Input(
                "Invalid action. Use 'find' or 'replace'.".into(),
            )),
        }
    }
}

impl FnDeclarator<RegexArgs, serde_json::Value> for RegexTool {
    fn declare(&self) -> FunctionDeclaration<RegexArgs, serde_json::Value> {
        FunctionDeclaration {
            name: "regex_tool",
            description: "Applies regular expressions for finding matches (with groups) or replacing text patterns.",
            parameters: schema_for!(RegexArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
