//! Markdown header-aware text splitter.
//!
//! Splits Markdown documents by headers, preserving document structure.
//! Each chunk includes the header hierarchy for context.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{SplitterConfig, TextChunk, TextSplitter};

/// Metadata about a markdown section.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MarkdownMetadata {
    /// The headers leading to this section (h1, h2, h3, etc.)
    pub headers: HashMap<String, String>,
}

/// A chunk of markdown with its header context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownChunk {
    /// The text content of this chunk.
    pub content: String,
    /// The header hierarchy for this chunk.
    pub metadata: MarkdownMetadata,
}

/// A splitter that breaks Markdown documents by header structure.
///
/// It splits on headers and optionally includes
/// the header hierarchy in each chunk for context.
pub struct MarkdownHeaderTextSplitter {
    config: SplitterConfig,
    /// Headers to split on
    headers_to_split_on: Vec<(String, String)>,
    /// Whether to include headers in the chunk content.
    include_headers: bool,
    /// Whether to strip headers from chunk content.
    strip_headers: bool,
}

impl MarkdownHeaderTextSplitter {
    /// Create a new Markdown splitter with default headers (# through ######).
    pub fn new(config: SplitterConfig) -> Self {
        Self {
            config,
            headers_to_split_on: vec![
                ("#".to_string(), "h1".to_string()),
                ("##".to_string(), "h2".to_string()),
                ("###".to_string(), "h3".to_string()),
                ("####".to_string(), "h4".to_string()),
                ("#####".to_string(), "h5".to_string()),
                ("######".to_string(), "h6".to_string()),
            ],
            include_headers: true,
            strip_headers: false,
        }
    }

    /// Set custom headers to split on.
    /// Each tuple is (markdown_header, metadata_key), e.g., ("##", "h2").
    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.headers_to_split_on = headers;
        self
    }

    /// Set whether to include header text in chunk content.
    pub fn with_include_headers(mut self, include: bool) -> Self {
        self.include_headers = include;
        self
    }

    /// Set whether to strip header markers from content.
    pub fn with_strip_headers(mut self, strip: bool) -> Self {
        self.strip_headers = strip;
        self
    }

    /// Parse a line to check if it's a header.
    fn parse_header_line(&self, line: &str) -> Option<(String, String, String)> {
        let trimmed = line.trim_start();

        // Sort headers by length descending to match longer ones first (### before ##)
        let mut sorted_headers = self.headers_to_split_on.clone();
        sorted_headers.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

        for (marker, key) in &sorted_headers {
            if trimmed.starts_with(marker) {
                let after_marker = &trimmed[marker.len()..];
                // Must be followed by space or end of line
                if after_marker.is_empty() || after_marker.starts_with(' ') {
                    let header_text = after_marker.trim().to_string();
                    return Some((marker.clone(), key.clone(), header_text));
                }
            }
        }
        None
    }

    /// Get the header level (1-6) from a marker.
    fn header_level(&self, marker: &str) -> usize {
        marker.chars().filter(|c| *c == '#').count()
    }

    /// Split markdown and return chunks with metadata.
    pub fn split_markdown(&self, text: &str) -> Vec<MarkdownChunk> {
        let mut chunks = Vec::new();
        let mut current_content = String::new();
        let mut current_headers: HashMap<String, String> = HashMap::new();
        let lines: Vec<&str> = text.lines().collect();

        for line in lines {
            if let Some((marker, key, header_text)) = self.parse_header_line(line) {
                // Save current chunk if not empty
                let trimmed_content = if self.config.trim_chunks {
                    current_content.trim().to_string()
                } else {
                    current_content.clone()
                };

                if !trimmed_content.is_empty() {
                    chunks.push(MarkdownChunk {
                        content: trimmed_content,
                        metadata: MarkdownMetadata {
                            headers: current_headers.clone(),
                        },
                    });
                }
                current_content.clear();

                // Update header hierarchy
                let level = self.header_level(&marker);

                // Clear lower-level headers
                let keys_to_remove: Vec<String> = current_headers
                    .keys()
                    .filter(|k| {
                        let k_level = k
                            .strip_prefix('h')
                            .and_then(|n| n.parse::<usize>().ok())
                            .unwrap_or(0);
                        k_level >= level
                    })
                    .cloned()
                    .collect();

                for k in keys_to_remove {
                    current_headers.remove(&k);
                }

                // Set current header
                current_headers.insert(key.clone(), header_text.clone());

                // Add header to content if configured
                if self.include_headers {
                    if self.strip_headers {
                        current_content.push_str(&header_text);
                    } else {
                        current_content.push_str(line);
                    }
                    current_content.push('\n');
                }
            } else {
                current_content.push_str(line);
                current_content.push('\n');
            }
        }

        // Don't forget the last chunk
        let trimmed_content = if self.config.trim_chunks {
            current_content.trim().to_string()
        } else {
            current_content
        };

        if !trimmed_content.is_empty() {
            chunks.push(MarkdownChunk {
                content: trimmed_content,
                metadata: MarkdownMetadata {
                    headers: current_headers,
                },
            });
        }

        // If chunks are too large, split them further
        self.split_large_chunks(chunks)
    }

    /// Split chunks that exceed chunk_size.
    fn split_large_chunks(&self, chunks: Vec<MarkdownChunk>) -> Vec<MarkdownChunk> {
        let mut result = Vec::new();

        for chunk in chunks {
            if chunk.content.len() <= self.config.chunk_size {
                result.push(chunk);
            } else {
                // Split by paragraphs first, then by lines
                let sub_chunks = self.split_content(&chunk.content);
                for sub in sub_chunks {
                    result.push(MarkdownChunk {
                        content: sub,
                        metadata: chunk.metadata.clone(),
                    });
                }
            }
        }

        result
    }

    /// Split content that's too large.
    fn split_content(&self, content: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut current = String::new();

        // Try splitting by paragraphs first
        for para in content.split("\n\n") {
            let para_trimmed = para.trim();
            if para_trimmed.is_empty() {
                continue;
            }

            if current.len() + para_trimmed.len() + 2 > self.config.chunk_size {
                if !current.is_empty() {
                    result.push(current.trim().to_string());
                    current.clear();
                }

                // If single paragraph is too large, split by lines
                if para_trimmed.len() > self.config.chunk_size {
                    for line in para_trimmed.lines() {
                        if current.len() + line.len() + 1 > self.config.chunk_size
                            && !current.is_empty()
                        {
                            result.push(current.trim().to_string());
                            current.clear();
                        }
                        if !current.is_empty() {
                            current.push('\n');
                        }
                        current.push_str(line);
                    }
                } else {
                    current.push_str(para_trimmed);
                }
            } else {
                if !current.is_empty() {
                    current.push_str("\n\n");
                }
                current.push_str(para_trimmed);
            }
        }

        if !current.is_empty() {
            result.push(current.trim().to_string());
        }

        result
    }
}

impl TextSplitter for MarkdownHeaderTextSplitter {
    fn split(&self, text: &str) -> Vec<TextChunk> {
        let md_chunks = self.split_markdown(text);

        if self.config.track_indices {
            let mut result = Vec::new();
            let mut search_start = 0;

            for chunk in md_chunks {
                // Try to find the chunk content in original text
                let search_text = chunk.content.lines().next().unwrap_or(&chunk.content);
                if let Some(pos) = text[search_start..].find(search_text) {
                    let start = search_start + pos;
                    let end = start + chunk.content.len();
                    result.push(TextChunk::with_indices(chunk.content, start, end));
                    search_start = start + 1;
                } else {
                    result.push(TextChunk::new(chunk.content));
                }
            }
            result
        } else {
            md_chunks
                .into_iter()
                .map(|c| TextChunk::new(c.content))
                .collect()
        }
    }

    fn config(&self) -> &SplitterConfig {
        &self.config
    }
}

impl Default for MarkdownHeaderTextSplitter {
    fn default() -> Self {
        Self::new(SplitterConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_headers() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = MarkdownHeaderTextSplitter::new(config);

        let text = r#"# Header 1

Content under header 1.

## Header 2

Content under header 2.

### Header 3

Content under header 3."#;

        let chunks = splitter.split_markdown(text);

        assert_eq!(chunks.len(), 3);
        assert!(chunks[0].metadata.headers.contains_key("h1"));
        assert!(chunks[1].metadata.headers.contains_key("h2"));
        assert!(chunks[2].metadata.headers.contains_key("h3"));
    }

    #[test]
    fn test_header_hierarchy() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = MarkdownHeaderTextSplitter::new(config);

        let text = r#"# Main

## Sub 1

Content 1

## Sub 2

Content 2"#;

        let chunks = splitter.split_markdown(text);

        // Each chunk under Sub 1 and Sub 2 should have h1 in metadata
        for chunk in &chunks[1..] {
            assert!(chunk.metadata.headers.contains_key("h1"));
        }
    }

    #[test]
    fn test_no_headers() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = MarkdownHeaderTextSplitter::new(config);

        let text = "Just some text without any headers.";
        let chunks = splitter.split_markdown(text);

        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].metadata.headers.is_empty());
    }

    #[test]
    fn test_large_section_split() {
        let config = SplitterConfig::new(50, 0);
        let splitter = MarkdownHeaderTextSplitter::new(config);

        let text = r#"# Header

This is a very long paragraph that exceeds the chunk size limit and should be split into multiple chunks.

Another paragraph here."#;

        let chunks = splitter.split_markdown(text);

        // Should be split due to chunk size
        assert!(chunks.len() >= 2);
    }

    #[test]
    fn test_strip_headers() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = MarkdownHeaderTextSplitter::new(config)
            .with_include_headers(true)
            .with_strip_headers(true);

        let text = "# My Header\n\nContent here.";
        let chunks = splitter.split_markdown(text);

        assert!(!chunks[0].content.contains('#'));
        assert!(chunks[0].content.contains("My Header"));
    }

    #[test]
    fn test_exclude_headers() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = MarkdownHeaderTextSplitter::new(config).with_include_headers(false);

        let text = "# Header\n\nContent only.";
        let chunks = splitter.split_markdown(text);

        assert!(!chunks[0].content.contains("Header"));
        assert!(chunks[0].content.contains("Content only"));
    }

    #[test]
    fn test_empty_text() {
        let splitter = MarkdownHeaderTextSplitter::default();
        let chunks = splitter.split_markdown("");
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_custom_headers() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = MarkdownHeaderTextSplitter::new(config)
            .with_headers(vec![("##".to_string(), "section".to_string())]);

        let text = "# Ignored\n\n## Used\n\nContent";
        let chunks = splitter.split_markdown(text);

        // Only ## should trigger a split
        assert!(
            chunks
                .iter()
                .any(|c| c.metadata.headers.contains_key("section"))
        );
    }
}
