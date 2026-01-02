use crate::util::formatters::Cleaner;

pub struct WhitespaceFormatter;

impl Cleaner for WhitespaceFormatter {
    /// Clean HTML by removing extra whitespace
    fn clean_html(html: &str) -> String {
        html.lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Clean text by normalizing whitespace
    fn clean_text(text: &str) -> String {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string()
    }

    fn clean_markdown(markdown: &str) -> String {
        Self::clean_text(markdown)
    }

    fn clean_json(json: &str) -> String {
        Self::clean_text(json)
    }
}
