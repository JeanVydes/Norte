//! format_tool
//!
//! Tool for converting and cleaning data between HTML, JSON, and Markdown formats.

use rustychain::FunctionDeclaration;
use rustychain::llm::function::{FnDeclarator, FnExecutor};
use rustychain::util::formatters::{
    Cleaner, Formatter, html::HtmlFormatter, json::JsonFormatter, markdown::MarkdownFormatter,
    whitespace::WhitespaceFormatter,
};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct FormatArgs {
    #[schemars(description = "The target format: 'html', 'json', 'markdown', or 'clean'.")]
    pub target: String,
    #[schemars(description = "The source format of the data: 'html', 'json', 'markdown'.")]
    pub source: String,
    #[schemars(description = "The content to transform or clean.")]
    pub content: String,
    #[schemars(
        description = "If true, only performs whitespace normalization without format conversion."
    )]
    pub only_clean: Option<bool>,
}

#[derive(Clone, Default)]
pub struct FormatTool;

#[async_trait::async_trait]
impl FnExecutor<FormatArgs, String> for FormatTool {
    async fn call(&self, args: FormatArgs) -> rustychain::Result<String> {
        let only_clean = args.only_clean.unwrap_or(false);

        if only_clean {
            return match args.source.to_lowercase().as_str() {
                "html" => Ok(WhitespaceFormatter::clean_html(&args.content)),
                "json" => Ok(WhitespaceFormatter::clean_json(&args.content)),
                "markdown" => Ok(WhitespaceFormatter::clean_markdown(&args.content)),
                _ => Ok(WhitespaceFormatter::clean_text(&args.content)),
            };
        }

        match (
            args.source.to_lowercase().as_str(),
            args.target.to_lowercase().as_str(),
        ) {
            // Conversiones desde JSON
            ("json", "markdown") => JsonFormatter::to_markdown(&args.content),
            ("json", "html") => JsonFormatter::to_html(&args.content),
            ("json", "json") => JsonFormatter::to_json(&args.content), // Prettify

            // Conversiones desde HTML
            ("html", "markdown") => HtmlFormatter::to_markdown(&args.content),
            ("html", "json") => HtmlFormatter::to_json(&args.content),
            ("html", "text") => HtmlFormatter::from_html(&args.content),

            // Conversiones desde Markdown
            ("markdown", "html") => MarkdownFormatter::to_html(&args.content),
            ("markdown", "json") => MarkdownFormatter::to_json(&args.content),

            // Casos por defecto o limpieza
            (_, "clean") => Ok(WhitespaceFormatter::clean_text(&args.content)),
            (s, t) if s == t => Ok(args.content),
            _ => Err(rustychain::Error::Internal(
                format!("Unsupported conversion: {} to {}", args.source, args.target).into(),
            )),
        }
    }
}

impl FnDeclarator<FormatArgs, String> for FormatTool {
    fn declare(&self) -> FunctionDeclaration<FormatArgs, String> {
        FunctionDeclaration {
            name: "format_tool",
            description: "Converts and cleans content between different formats (JSON, HTML, Markdown). Use it to prettify logs, convert HTML to readable Markdown, or minify JSON.",
            parameters: schema_for!(FormatArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
