use crate::util::formatters::{Cleaner, Formatter, whitespace::WhitespaceFormatter};
use serde_json::{Value, json};

pub struct JsonFormatter;

impl Formatter for JsonFormatter {
    /// Convert JSON to a Markdown code block
    fn to_markdown(json_str: &str) -> crate::Result<String> {
        // Validate and prettify the JSON first to ensure it is readable in Markdown
        let value: Value = serde_json::from_str(json_str)?;

        let pretty_json = serde_json::to_string_pretty(&value)?;

        Ok(format!("```json\n{}\n```", pretty_json))
    }

    /// Convert Markdown text into a JSON object structure
    fn from_markdown(markdown: &str) -> crate::Result<String> {
        let clean_markdown = WhitespaceFormatter::clean_text(markdown);

        let json_output = json!({
            "content": clean_markdown,
            "format": "markdown"
        });

        Ok(json_output.to_string())
    }

    /// Convert JSON to an HTML syntax-highlighted code block
    fn to_html(json_str: &str) -> crate::Result<String> {
        // Validate and prettify
        let value: Value = serde_json::from_str(json_str)?;

        let pretty_json = serde_json::to_string_pretty(&value)?;

        // Simple HTML escaping for the content within the code block
        let escaped_json = pretty_json
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;");

        Ok(format!(
            "<pre><code class=\"language-json\">{}</code></pre>",
            escaped_json
        ))
    }

    /// Convert HTML text into a JSON object structure
    fn from_html(html: &str) -> crate::Result<String> {
        let clean_html = WhitespaceFormatter::clean_text(html);

        let json_output = json!({
            "content": clean_html,
            "format": "html"
        });

        Ok(json_output.to_string())
    }

    /// Format JSON to Pretty Print JSON (Identity transformation with formatting)
    fn to_json(text: &str) -> crate::Result<String> {
        let value: Value = serde_json::from_str(text)?;

        Ok(serde_json::to_string_pretty(&value)?)
    }

    /// Minify JSON (Identity transformation removing whitespace)
    fn from_json(text: &str) -> crate::Result<String> {
        let value: Value = serde_json::from_str(text)?;

        Ok(serde_json::to_string(&value)?)
    }
}
