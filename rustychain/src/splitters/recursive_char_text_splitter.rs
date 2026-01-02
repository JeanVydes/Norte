//! Recursive character text splitter.
//!
//! This splitter attempts to split text by a list of separators in order of priority,
//! recursively splitting chunks that are too large with progressively finer separators.
//!
//! # Example
//!
//! ```rust
//! use rustychain::splitters::{RecursiveCharacterTextSplitter, SplitterConfig, TextSplitter};
//!
//! let config = SplitterConfig::new(100, 20);
//! let splitter = RecursiveCharacterTextSplitter::new(config);
//!
//! let text = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
//! let chunks = splitter.split_text(text);
//! ```

use super::{LengthFn, SplitterConfig, TextChunk, TextSplitter};

/// Default separators for recursive splitting, ordered from coarsest to finest.
pub const DEFAULT_SEPARATORS: &[&str] = &[
    "\n\n", // Paragraphs
    "\n",   // Lines
    ".",    // Sentences
    ",",    // Clauses
    " ",    // Words
    "",     // Characters (last resort)
];

/// A text splitter that recursively splits text using a hierarchy of separators.
///
/// It first tries to split by the coarsest separator (e.g., double newlines for paragraphs).
/// If a resulting chunk is still too large, it recursively splits using finer separators
/// (single newlines, spaces, then individual characters).
pub struct RecursiveCharacterTextSplitter {
    config: SplitterConfig,
    separators: Vec<String>,
    length_fn: Option<LengthFn>,
    keep_separator: SeparatorBehavior,
}

/// How to handle separators when splitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeparatorBehavior {
    /// Remove the separator from the output.
    #[default]
    Remove,
    /// Keep the separator at the start of each chunk.
    Start,
    /// Keep the separator at the end of each chunk.
    End,
}

impl RecursiveCharacterTextSplitter {
    /// Create a new splitter with the given configuration and default separators.
    pub fn new(config: SplitterConfig) -> Self {
        Self {
            config,
            separators: DEFAULT_SEPARATORS.iter().map(|s| s.to_string()).collect(),
            length_fn: None,
            keep_separator: SeparatorBehavior::default(),
        }
    }

    /// Create a splitter with custom separators.
    pub fn with_separators(mut self, separators: Vec<String>) -> Self {
        self.separators = separators;
        self
    }

    /// Set a custom length function (e.g., for token counting).
    pub fn with_length_fn<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> usize + Send + Sync + 'static,
    {
        self.length_fn = Some(Box::new(f));
        self
    }

    /// Set separator behavior.
    pub fn with_separator_behavior(mut self, behavior: SeparatorBehavior) -> Self {
        self.keep_separator = behavior;
        self
    }

    /// Calculate length using custom function if provided, otherwise character count.
    fn calc_length(&self, text: &str) -> usize {
        match &self.length_fn {
            Some(f) => f(text),
            None => text.chars().count(),
        }
    }

    /// Split text by a separator, handling separator retention.
    fn split_by_separator<'a>(&self, text: &'a str, separator: &str) -> Vec<&'a str> {
        if separator.is_empty() {
            // Split into individual characters
            return text
                .char_indices()
                .map(|(i, c)| &text[i..i + c.len_utf8()])
                .collect();
        }

        match self.keep_separator {
            SeparatorBehavior::Remove => text.split(separator).collect(),
            SeparatorBehavior::Start => {
                // Keep separator at the start of each split (except first)
                let mut result = Vec::new();
                let mut last_end = 0;

                for (idx, _) in text.match_indices(separator) {
                    if last_end < idx {
                        result.push(&text[last_end..idx]);
                    }
                    last_end = idx;
                }

                if last_end < text.len() {
                    result.push(&text[last_end..]);
                }

                result
            }
            SeparatorBehavior::End => {
                // Keep separator at the end of each split (except last)
                let mut result = Vec::new();
                let mut last_end = 0;

                for (idx, matched) in text.match_indices(separator) {
                    let end = idx + matched.len();
                    result.push(&text[last_end..end]);
                    last_end = end;
                }

                if last_end < text.len() {
                    result.push(&text[last_end..]);
                }

                result
            }
        }
    }

    /// Merge splits into chunks respecting chunk_size and chunk_overlap.
    fn merge_splits(&self, splits: Vec<&str>, separator: &str) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current_chunk: Vec<&str> = Vec::new();
        let mut current_length = 0;
        let separator_len = self.calc_length(separator);

        for split in splits {
            let split_len = self.calc_length(split);

            // Check if adding this split would exceed chunk_size
            let total_len = if current_chunk.is_empty() {
                split_len
            } else {
                current_length + separator_len + split_len
            };

            if total_len > self.config.chunk_size && !current_chunk.is_empty() {
                // Finalize current chunk
                let chunk_text = current_chunk.join(separator);
                chunks.push(self.maybe_trim(&chunk_text));

                // Handle overlap: keep some elements for the next chunk
                while !current_chunk.is_empty() {
                    let overlap_len: usize = current_chunk
                        .iter()
                        .map(|s| self.calc_length(s))
                        .sum::<usize>()
                        + (current_chunk.len().saturating_sub(1)) * separator_len;

                    if overlap_len <= self.config.chunk_overlap {
                        break;
                    }
                    current_chunk.remove(0);
                }

                current_length = current_chunk
                    .iter()
                    .map(|s| self.calc_length(s))
                    .sum::<usize>()
                    + current_chunk.len().saturating_sub(1) * separator_len;
            }

            current_chunk.push(split);
            current_length = if current_chunk.len() == 1 {
                split_len
            } else {
                current_length + separator_len + split_len
            };
        }

        // Don't forget the last chunk
        if !current_chunk.is_empty() {
            let chunk_text = current_chunk.join(separator);
            chunks.push(self.maybe_trim(&chunk_text));
        }

        chunks
    }

    /// Trim chunk if configured.
    fn maybe_trim(&self, text: &str) -> String {
        if self.config.trim_chunks {
            text.trim().to_string()
        } else {
            text.to_string()
        }
    }

    /// Recursively split text using the separator hierarchy.
    fn split_recursive(&self, text: &str, separator_idx: usize) -> Vec<String> {
        // Base case: no more separators or text fits
        if self.calc_length(text) <= self.config.chunk_size {
            let trimmed = self.maybe_trim(text);
            if trimmed.is_empty() {
                return vec![];
            }
            return vec![trimmed];
        }

        if separator_idx >= self.separators.len() {
            // No more separators, just return the text as-is (even if too large)
            let trimmed = self.maybe_trim(text);
            if trimmed.is_empty() {
                return vec![];
            }
            return vec![trimmed];
        }

        let separator = &self.separators[separator_idx];

        // Check if this separator exists in the text
        if !separator.is_empty() && !text.contains(separator) {
            // Try next separator
            return self.split_recursive(text, separator_idx + 1);
        }

        let splits = self.split_by_separator(text, separator);

        // Merge small splits and recursively split large ones
        let mut final_chunks = Vec::new();
        let mut good_splits = Vec::new();

        for split in splits {
            if self.calc_length(split) <= self.config.chunk_size {
                good_splits.push(split);
            } else {
                // First merge any accumulated good splits
                if !good_splits.is_empty() {
                    let merged = self.merge_splits(good_splits.clone(), separator);
                    final_chunks.extend(merged);
                    good_splits.clear();
                }
                // Recursively split the large chunk with finer separator
                let sub_chunks = self.split_recursive(split, separator_idx + 1);
                final_chunks.extend(sub_chunks);
            }
        }

        // Merge any remaining good splits
        if !good_splits.is_empty() {
            let merged = self.merge_splits(good_splits, separator);
            final_chunks.extend(merged);
        }

        // Filter by min_chunk_size if configured
        if let Some(min_size) = self.config.min_chunk_size {
            final_chunks.retain(|c| self.calc_length(c) >= min_size);
        }

        final_chunks
    }
}

impl TextSplitter for RecursiveCharacterTextSplitter {
    fn split(&self, text: &str) -> Vec<TextChunk> {
        let chunks = self.split_recursive(text, 0);

        if self.config.track_indices {
            // Track indices in original text
            let mut result = Vec::new();
            let mut search_start = 0;

            for chunk in chunks {
                if let Some(start) = text[search_start..].find(&chunk) {
                    let abs_start = search_start + start;
                    let abs_end = abs_start + chunk.len();
                    result.push(TextChunk::with_indices(chunk, abs_start, abs_end));
                    // For overlapping chunks, don't advance search_start too far
                    search_start = abs_start + 1;
                } else {
                    result.push(TextChunk::new(chunk));
                }
            }

            result
        } else {
            chunks.into_iter().map(TextChunk::new).collect()
        }
    }

    fn config(&self) -> &SplitterConfig {
        &self.config
    }

    fn length(&self, text: &str) -> usize {
        self.calc_length(text)
    }
}

impl Default for RecursiveCharacterTextSplitter {
    fn default() -> Self {
        Self::new(SplitterConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_split() {
        let config = SplitterConfig::new(50, 0);
        let splitter = RecursiveCharacterTextSplitter::new(config);

        let text = "Hello world. This is a test.\n\nAnother paragraph here.";
        let chunks = splitter.split_text(text);

        assert!(!chunks.is_empty());
        for chunk in &chunks {
            assert!(chunk.len() <= 50);
        }
    }

    #[test]
    fn test_small_text_no_split() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = RecursiveCharacterTextSplitter::new(config);

        let text = "Small text.";
        let chunks = splitter.split_text(text);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "Small text.");
    }

    #[test]
    fn test_paragraph_splits() {
        let config = SplitterConfig::new(100, 0);
        let splitter = RecursiveCharacterTextSplitter::new(config);

        let text = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
        let chunks = splitter.split_text(text);

        assert_eq!(chunks.len(), 1); // All fits in one chunk
    }

    #[test]
    fn test_long_paragraphs_split() {
        let config = SplitterConfig::new(30, 0);
        let splitter = RecursiveCharacterTextSplitter::new(config);

        let text = "First paragraph with more text.\n\nSecond paragraph also long.";
        let chunks = splitter.split_text(text);

        assert!(chunks.len() > 1);
    }

    #[test]
    fn test_overlap() {
        let config = SplitterConfig::new(20, 5);
        let splitter = RecursiveCharacterTextSplitter::new(config);

        let text = "one two three four five six seven eight";
        let chunks = splitter.split_text(text);

        // With overlap, some content should appear in multiple chunks
        assert!(chunks.len() >= 2);
    }

    #[test]
    fn test_with_indices() {
        let config = SplitterConfig::new(50, 0).with_indices(true);
        let splitter = RecursiveCharacterTextSplitter::new(config);

        let text = "First part.\n\nSecond part.";
        let chunks = splitter.split(text);

        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].start_index.is_some());
    }

    #[test]
    fn test_custom_separators() {
        let config = SplitterConfig::new(20, 0);
        let splitter = RecursiveCharacterTextSplitter::new(config)
            .with_separators(vec!["|||".to_string(), "|".to_string()]);

        let text = "part1|||part2|||part3";
        let chunks = splitter.split_text(text);

        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_empty_text() {
        let splitter = RecursiveCharacterTextSplitter::default();
        let chunks = splitter.split_text("");
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_whitespace_only() {
        let config = SplitterConfig::new(100, 0).with_trim(true);
        let splitter = RecursiveCharacterTextSplitter::new(config);

        let chunks = splitter.split_text("   \n\n   ");
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_unicode_text() {
        let config = SplitterConfig::new(20, 0);
        let splitter = RecursiveCharacterTextSplitter::new(config);

        let text = "こんにちは世界。\n\nこれはテストです。";
        let chunks = splitter.split_text(text);

        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_min_chunk_size() {
        let config = SplitterConfig::new(50, 0).with_min_chunk_size(10);
        let splitter = RecursiveCharacterTextSplitter::new(config);

        let text = "a\n\nb\n\nThis is a longer piece of text.";
        let chunks = splitter.split_text(text);

        for chunk in &chunks {
            assert!(chunk.len() >= 10 || chunk == "a" || chunk == "b");
        }
    }

    #[test]
    fn test_separator_behavior_end() {
        let config = SplitterConfig::new(100, 0);
        let splitter = RecursiveCharacterTextSplitter::new(config)
            .with_separator_behavior(SeparatorBehavior::End);

        let text = "Line1\nLine2\nLine3";
        let chunks = splitter.split_text(text);

        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn test_config_builder() {
        let config = SplitterConfig::new(500, 50)
            .with_trim(false)
            .with_indices(true)
            .with_min_chunk_size(20);

        assert_eq!(config.chunk_size, 500);
        assert_eq!(config.chunk_overlap, 50);
        assert!(!config.trim_chunks);
        assert!(config.track_indices);
        assert_eq!(config.min_chunk_size, Some(20));
    }
}
