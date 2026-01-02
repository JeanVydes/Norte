//! HTML semantic text splitter.
//!
//! Splits HTML documents by semantic elements (headers, sections, articles, etc.)
//! while preserving the document structure.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{SplitterConfig, TextChunk, TextSplitter};

/// Metadata about an HTML section.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HtmlMetadata {
    /// The semantic tags hierarchy for this chunk.
    pub tags: HashMap<String, String>,
}

/// A chunk of HTML content with its semantic context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HtmlChunk {
    /// The text content (HTML stripped or preserved based on config).
    pub content: String,
    /// The semantic context for this chunk.
    pub metadata: HtmlMetadata,
}

/// Default semantic HTML tags to split on, ordered by priority.
pub const DEFAULT_HTML_HEADERS: &[(&str, &str)] = &[
    ("h1", "h1"),
    ("h2", "h2"),
    ("h3", "h3"),
    ("h4", "h4"),
    ("h5", "h5"),
    ("h6", "h6"),
];

pub const DEFAULT_HTML_SECTIONS: &[(&str, &str)] = &[
    ("article", "article"),
    ("section", "section"),
    ("nav", "nav"),
    ("aside", "aside"),
    ("header", "header"),
    ("footer", "footer"),
    ("main", "main"),
    ("div", "div"),
];

/// A splitter that breaks HTML documents by semantic structure.
///
/// It splits on semantic HTML elements like headers (h1-h6), sections, articles, etc.
/// The text content is extracted and organized by semantic hierarchy.
pub struct HtmlSemanticSplitter {
    config: SplitterConfig,
    /// Tags to split on with their metadata keys.
    headers_to_split_on: Vec<(String, String)>,
    /// Whether to strip HTML tags from output.
    strip_tags: bool,
    /// Whether to preserve some formatting tags.
    preserve_formatting: bool,
}

impl HtmlSemanticSplitter {
    /// Create a new HTML semantic splitter with default header tags.
    pub fn new(config: SplitterConfig) -> Self {
        Self {
            config,
            headers_to_split_on: DEFAULT_HTML_HEADERS
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            strip_tags: true,
            preserve_formatting: false,
        }
    }

    /// Set custom tags to split on.
    pub fn with_tags(mut self, tags: Vec<(String, String)>) -> Self {
        self.headers_to_split_on = tags;
        self
    }

    /// Add section tags to the split list.
    pub fn with_sections(mut self) -> Self {
        self.headers_to_split_on.extend(
            DEFAULT_HTML_SECTIONS
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string())),
        );
        self
    }

    /// Set whether to strip HTML tags from output.
    pub fn with_strip_tags(mut self, strip: bool) -> Self {
        self.strip_tags = strip;
        self
    }

    /// Set whether to preserve formatting tags (b, i, em, strong, code).
    pub fn with_preserve_formatting(mut self, preserve: bool) -> Self {
        self.preserve_formatting = preserve;
        self
    }

    /// Extract text content from HTML, optionally stripping tags.
    fn extract_text(&self, html: &str) -> String {
        if !self.strip_tags {
            return html.to_string();
        }

        let mut result = String::new();
        let mut in_tag = false;
        let mut tag_name = String::new();
        let mut skip_content = false;

        // Tags whose content should be skipped
        let skip_tags = ["script", "style", "noscript"];

        let chars: Vec<char> = html.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            let c = chars[i];

            if c == '<' {
                in_tag = true;
                tag_name.clear();

                // Check if closing tag
                if i + 1 < chars.len() && chars[i + 1] == '/' {
                    i += 2;
                    while i < chars.len() && chars[i] != '>' {
                        tag_name.push(chars[i].to_ascii_lowercase());
                        i += 1;
                    }
                    if skip_tags.contains(&tag_name.as_str()) {
                        skip_content = false;
                    }
                    // Add space for block elements
                    if [
                        "p", "div", "br", "h1", "h2", "h3", "h4", "h5", "h6", "li", "tr",
                    ]
                    .contains(&tag_name.as_str())
                        && !result.ends_with(' ')
                        && !result.ends_with('\n')
                    {
                        result.push(' ');
                    }
                    in_tag = false;
                }
            } else if c == '>' {
                in_tag = false;
                if skip_tags.contains(&tag_name.as_str()) {
                    skip_content = true;
                }
                // Add newline for headers
                if tag_name.starts_with('h') && tag_name.len() == 2 {
                    result.push('\n');
                }
            } else if in_tag {
                if c.is_alphanumeric() && tag_name.len() < 20 {
                    tag_name.push(c.to_ascii_lowercase());
                }
            } else if !skip_content {
                // Decode common HTML entities
                if c == '&' {
                    let remaining: String = chars[i..].iter().take(10).collect();
                    if remaining.starts_with("&nbsp;") {
                        result.push(' ');
                        i += 5;
                    } else if remaining.starts_with("&lt;") {
                        result.push('<');
                        i += 3;
                    } else if remaining.starts_with("&gt;") {
                        result.push('>');
                        i += 3;
                    } else if remaining.starts_with("&amp;") {
                        result.push('&');
                        i += 4;
                    } else if remaining.starts_with("&quot;") {
                        result.push('"');
                        i += 5;
                    } else {
                        result.push(c);
                    }
                } else {
                    result.push(c);
                }
            }

            i += 1;
        }

        // Clean up whitespace
        let mut cleaned = String::new();
        let mut last_was_space = false;

        for c in result.chars() {
            if c.is_whitespace() {
                if !last_was_space {
                    cleaned.push(' ');
                    last_was_space = true;
                }
            } else {
                cleaned.push(c);
                last_was_space = false;
            }
        }

        cleaned.trim().to_string()
    }

    /// Find all occurrences of a tag and its content.
    fn find_tag_sections(&self, html: &str, tag: &str) -> Vec<(usize, usize, String)> {
        let mut sections = Vec::new();
        let open_tag = format!("<{}", tag);
        let close_tag = format!("</{}>", tag);

        let html_lower = html.to_lowercase();
        let mut search_start = 0;

        while let Some(start) = html_lower[search_start..].find(&open_tag) {
            let abs_start = search_start + start;

            // Find the end of opening tag
            if let Some(tag_end) = html[abs_start..].find('>') {
                let content_start = abs_start + tag_end + 1;

                // Find closing tag
                if let Some(close) = html_lower[content_start..].find(&close_tag) {
                    let content_end = content_start + close;
                    let content = &html[content_start..content_end];
                    sections.push((
                        abs_start,
                        content_end + close_tag.len(),
                        content.to_string(),
                    ));
                    search_start = content_end + close_tag.len();
                } else {
                    search_start = content_start;
                }
            } else {
                search_start = abs_start + 1;
            }
        }

        sections
    }

    /// Split HTML and return chunks with metadata.
    pub fn split_html(&self, html: &str) -> Vec<HtmlChunk> {
        let mut chunks = Vec::new();
        let mut current_headers: HashMap<String, String> = HashMap::new();

        // Find all header sections
        let mut all_sections: Vec<(usize, usize, String, String, String)> = Vec::new();

        for (tag, key) in &self.headers_to_split_on {
            for (start, end, content) in self.find_tag_sections(html, tag) {
                // Extract header text (first line or tag content)
                let header_text = self.extract_text(&content);
                all_sections.push((
                    start,
                    end,
                    tag.clone(),
                    key.clone(),
                    header_text
                        .lines()
                        .next()
                        .unwrap_or(&header_text)
                        .to_string(),
                ));
            }
        }

        // Sort by position
        all_sections.sort_by_key(|s| s.0);

        if all_sections.is_empty() {
            // No headers found, return entire content as one chunk
            let text = self.extract_text(html);
            if !text.is_empty() {
                return self.split_large_text(&text, &HashMap::new());
            }
            return chunks;
        }

        // Process content before first header
        if all_sections[0].0 > 0 {
            let before = &html[..all_sections[0].0];
            let text = self.extract_text(before);
            let trimmed = if self.config.trim_chunks {
                text.trim().to_string()
            } else {
                text
            };
            if !trimmed.is_empty() {
                chunks.extend(self.split_large_text(&trimmed, &HashMap::new()));
            }
        }

        // Process each section
        for i in 0..all_sections.len() {
            let (start, _end, tag, key, header_text) = &all_sections[i];

            // Update header hierarchy
            let level = self.tag_level(tag);
            let keys_to_remove: Vec<String> = current_headers
                .keys()
                .filter(|k| self.tag_level(k) >= level)
                .cloned()
                .collect();
            for k in keys_to_remove {
                current_headers.remove(&k);
            }
            current_headers.insert(key.clone(), header_text.clone());

            // Get content until next section
            let content_end = if i + 1 < all_sections.len() {
                all_sections[i + 1].0
            } else {
                html.len()
            };

            let section_html = &html[*start..content_end];
            let text = self.extract_text(section_html);
            let trimmed = if self.config.trim_chunks {
                text.trim().to_string()
            } else {
                text
            };

            if !trimmed.is_empty() {
                chunks.extend(self.split_large_text(&trimmed, &current_headers));
            }
        }

        chunks
    }

    /// Get tag level for hierarchy (h1 = 1, h2 = 2, etc.)
    fn tag_level(&self, tag: &str) -> usize {
        if tag.starts_with('h') && tag.len() == 2 {
            tag[1..].parse().unwrap_or(100)
        } else {
            100 // Non-header tags get high level
        }
    }

    /// Split text that exceeds chunk_size.
    fn split_large_text(&self, text: &str, headers: &HashMap<String, String>) -> Vec<HtmlChunk> {
        if text.len() <= self.config.chunk_size {
            return vec![HtmlChunk {
                content: text.to_string(),
                metadata: HtmlMetadata {
                    tags: headers.clone(),
                },
            }];
        }

        let mut result = Vec::new();
        let mut current = String::new();

        // Split by sentences (roughly)
        for part in text.split(". ") {
            let part_with_period = if current.is_empty() && !part.is_empty() {
                part.to_string()
            } else {
                format!(". {}", part)
            };

            if current.len() + part_with_period.len() > self.config.chunk_size
                && !current.is_empty()
            {
                result.push(HtmlChunk {
                    content: current.trim().to_string(),
                    metadata: HtmlMetadata {
                        tags: headers.clone(),
                    },
                });
                current.clear();
            }

            current.push_str(&part_with_period);
        }

        if !current.is_empty() {
            result.push(HtmlChunk {
                content: current.trim().to_string(),
                metadata: HtmlMetadata {
                    tags: headers.clone(),
                },
            });
        }

        result
    }
}

impl TextSplitter for HtmlSemanticSplitter {
    fn split(&self, text: &str) -> Vec<TextChunk> {
        let html_chunks = self.split_html(text);

        if self.config.track_indices {
            let mut result = Vec::new();
            let mut search_start = 0;

            for chunk in html_chunks {
                let search_text = &chunk.content[..chunk.content.len().min(30)];
                if let Some(pos) = text[search_start..].find(search_text) {
                    let start = search_start + pos;
                    result.push(TextChunk::with_indices(chunk.content, start, start));
                    search_start = start + 1;
                } else {
                    result.push(TextChunk::new(chunk.content));
                }
            }
            result
        } else {
            html_chunks
                .into_iter()
                .map(|c| TextChunk::new(c.content))
                .collect()
        }
    }

    fn config(&self) -> &SplitterConfig {
        &self.config
    }
}

impl Default for HtmlSemanticSplitter {
    fn default() -> Self {
        Self::new(SplitterConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_html() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = HtmlSemanticSplitter::new(config);

        let html = r#"<h1>Title</h1><p>Content here.</p>"#;
        let chunks = splitter.split_html(html);

        assert!(!chunks.is_empty());
        assert!(chunks[0].metadata.tags.contains_key("h1"));
    }

    #[test]
    fn test_nested_headers() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = HtmlSemanticSplitter::new(config);

        let html = r#"
            <h1>Main Title</h1>
            <p>Intro</p>
            <h2>Section 1</h2>
            <p>Section 1 content</p>
            <h2>Section 2</h2>
            <p>Section 2 content</p>
        "#;

        let chunks = splitter.split_html(html);

        assert!(chunks.len() >= 2);
    }

    #[test]
    fn test_strip_tags() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = HtmlSemanticSplitter::new(config);

        let html = r#"<div><p>Hello <strong>world</strong>!</p></div>"#;
        let chunks = splitter.split_html(html);

        assert!(!chunks.is_empty());
        assert!(!chunks[0].content.contains('<'));
        assert!(chunks[0].content.contains("Hello"));
        assert!(chunks[0].content.contains("world"));
    }

    #[test]
    fn test_preserve_tags() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = HtmlSemanticSplitter::new(config).with_strip_tags(false);

        let html = r#"<p>Hello <strong>world</strong>!</p>"#;
        let chunks = splitter.split_html(html);

        assert!(!chunks.is_empty());
        assert!(chunks[0].content.contains("<strong>"));
    }

    #[test]
    fn test_script_removal() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = HtmlSemanticSplitter::new(config);

        let html = r#"<p>Text</p><script>alert('hi');</script><p>More text</p>"#;
        let chunks = splitter.split_html(html);

        assert!(!chunks.is_empty());
        assert!(!chunks[0].content.contains("alert"));
    }

    #[test]
    fn test_html_entities() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = HtmlSemanticSplitter::new(config);

        let html = r#"<p>Hello&nbsp;world &amp; friends</p>"#;
        let text = splitter.extract_text(html);

        assert!(text.contains("Hello world"));
        assert!(text.contains("& friends"));
    }

    #[test]
    fn test_large_section_split() {
        let config = SplitterConfig::new(30, 0);
        let splitter = HtmlSemanticSplitter::new(config);

        let html = r#"<h1>Title</h1><p>This is a very long paragraph that should be split into multiple chunks because it definitely exceeds the small limit we set. More text here to make it even longer and ensure splitting happens properly.</p>"#;
        let chunks = splitter.split_html(html);

        assert!(
            chunks.len() >= 2,
            "Expected at least 2 chunks, got {}: {:?}",
            chunks.len(),
            chunks
        );
    }

    #[test]
    fn test_with_sections() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = HtmlSemanticSplitter::new(config).with_sections();

        let html = r#"<article><h1>Title</h1><section>Content</section></article>"#;
        let chunks = splitter.split_html(html);

        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_empty_html() {
        let splitter = HtmlSemanticSplitter::default();
        let chunks = splitter.split_html("");
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_no_headers() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = HtmlSemanticSplitter::new(config);

        let html = r#"<p>Just some text without headers.</p>"#;
        let chunks = splitter.split_html(html);

        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].metadata.tags.is_empty());
    }
}
