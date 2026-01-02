use serde::{Deserialize, Serialize};

/// A chunk of text with optional metadata about its position.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextChunk {
    /// The text content of this chunk.
    pub content: String,
    /// Start character index in the original text (if tracked).
    pub start_index: Option<usize>,
    /// End character index in the original text (if tracked).
    pub end_index: Option<usize>,
}

impl TextChunk {
    pub fn new(content: String) -> Self {
        Self {
            content,
            start_index: None,
            end_index: None,
        }
    }

    pub fn with_indices(content: String, start: usize, end: usize) -> Self {
        Self {
            content,
            start_index: Some(start),
            end_index: Some(end),
        }
    }
}

/// Configuration for text splitting operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitterConfig {
    /// Maximum size of each chunk (in characters or tokens depending on splitter).
    pub chunk_size: usize,
    /// Number of characters/tokens to overlap between adjacent chunks.
    /// This helps preserve context at chunk boundaries.
    pub chunk_overlap: usize,
    /// Whether to trim whitespace from chunks.
    pub trim_chunks: bool,
    /// Whether to track character indices in the original text.
    pub track_indices: bool,
    /// Minimum chunk size - chunks smaller than this will be merged with neighbors.
    pub min_chunk_size: Option<usize>,
}

impl Default for SplitterConfig {
    fn default() -> Self {
        Self {
            chunk_size: 1000,
            chunk_overlap: 200,
            trim_chunks: true,
            track_indices: false,
            min_chunk_size: None,
        }
    }
}

impl SplitterConfig {
    pub fn new(chunk_size: usize, chunk_overlap: usize) -> Self {
        Self {
            chunk_size,
            chunk_overlap,
            ..Default::default()
        }
    }

    pub fn with_trim(mut self, trim: bool) -> Self {
        self.trim_chunks = trim;
        self
    }

    pub fn with_indices(mut self, track: bool) -> Self {
        self.track_indices = track;
        self
    }

    pub fn with_min_chunk_size(mut self, min_size: usize) -> Self {
        self.min_chunk_size = Some(min_size);
        self
    }
}

/// Common trait for all text splitters.
pub trait TextSplitter: Send + Sync {
    /// Split the input text into chunks according to the splitter's strategy.
    fn split(&self, text: &str) -> Vec<TextChunk>;

    /// Split text and return only the content strings (convenience method).
    fn split_text(&self, text: &str) -> Vec<String> {
        self.split(text).into_iter().map(|c| c.content).collect()
    }

    /// Get the configuration for this splitter.
    fn config(&self) -> &SplitterConfig;

    /// Calculate the length of a text segment (characters by default, tokens for token-based splitters).
    fn length(&self, text: &str) -> usize {
        text.chars().count()
    }
}

/// Length function type for custom length calculations (e.g., token counting).
pub type LengthFn = Box<dyn Fn(&str) -> usize + Send + Sync>;
