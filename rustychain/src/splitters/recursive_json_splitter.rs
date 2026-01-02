//! Recursive JSON splitter.
//!
//! Splits JSON documents while maintaining valid JSON structure.
//! Recursively breaks down JSON objects and arrays to fit within chunk limits.

use serde_json::Value;

use super::{SplitterConfig, TextChunk, TextSplitter};

/// A splitter that recursively breaks down JSON documents.
///
/// It traverses JSON structures and splits them into smaller valid JSON chunks.
/// Objects are split by keys, arrays are split by elements.
pub struct RecursiveJsonSplitter {
    config: SplitterConfig,
    /// Maximum depth to traverse into nested structures.
    max_depth: usize,
    /// Minimum size for a JSON value to be considered for splitting.
    min_value_size: usize,
}

impl RecursiveJsonSplitter {
    /// Create a new JSON splitter with the given configuration.
    pub fn new(config: SplitterConfig) -> Self {
        Self {
            config,
            max_depth: 100,
            min_value_size: 10,
        }
    }

    /// Set the maximum depth for traversal.
    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }

    /// Set the minimum value size for splitting consideration.
    pub fn with_min_value_size(mut self, size: usize) -> Self {
        self.min_value_size = size;
        self
    }

    pub fn json_to_string(&self, value: &Value) -> String {
        serde_json::to_string(value).unwrap_or_default()
    }

    pub fn json_to_string_pretty(&self, value: &Value) -> String {
        serde_json::to_string_pretty(value).unwrap_or_default()
    }

    /// Recursively split a JSON value into chunks.
    fn split_value(&self, value: &Value, current_depth: usize) -> Vec<String> {
        let serialized = self.json_to_string(value);

        // If it fits, return as-is
        if serialized.len() <= self.config.chunk_size {
            return vec![serialized];
        }

        // If we've reached max depth, return even if too large
        if current_depth >= self.max_depth {
            return vec![serialized];
        }

        match value {
            Value::Object(map) => self.split_object(map, current_depth),
            Value::Array(arr) => self.split_array(arr, current_depth),
            // Primitives can't be split further
            _ => vec![serialized],
        }
    }

    /// Split a JSON object by its keys.
    fn split_object(
        &self,
        map: &serde_json::Map<String, Value>,
        current_depth: usize,
    ) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current_obj = serde_json::Map::new();
        let mut current_size = 2; // "{}"

        for (key, value) in map {
            let value_str = self.json_to_string(value);
            let entry_size = key.len() + 3 + value_str.len(); // "key":value,

            // Check if this single value is too large
            if value_str.len() > self.config.chunk_size {
                // Save current object first
                if !current_obj.is_empty() {
                    chunks.push(self.json_to_string(&Value::Object(current_obj.clone())));
                    current_obj.clear();
                    current_size = 2;
                }

                // Recursively split the large value
                let sub_chunks = self.split_value(value, current_depth + 1);
                for (i, sub_chunk) in sub_chunks.into_iter().enumerate() {
                    // Wrap each sub-chunk with a key reference
                    let wrapper = format!("{{\"{}[{}]\":{}}}", key, i, sub_chunk);
                    chunks.push(wrapper);
                }
                continue;
            }

            // Check if adding this entry would exceed chunk size
            if current_size + entry_size > self.config.chunk_size && !current_obj.is_empty() {
                chunks.push(self.json_to_string(&Value::Object(current_obj.clone())));
                current_obj.clear();
                current_size = 2;
            }

            current_obj.insert(key.clone(), value.clone());
            current_size += entry_size + 1; // +1 for comma
        }

        // Don't forget remaining entries
        if !current_obj.is_empty() {
            chunks.push(self.json_to_string(&Value::Object(current_obj)));
        }

        chunks
    }

    /// Split a JSON array by its elements.
    fn split_array(&self, arr: &[Value], current_depth: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current_arr = Vec::new();
        let mut current_size = 2; // "[]"

        for value in arr {
            let value_str = self.json_to_string(value);
            let entry_size = value_str.len() + 1; // value,

            // Check if this single value is too large
            if value_str.len() > self.config.chunk_size {
                // Save current array first
                if !current_arr.is_empty() {
                    chunks.push(self.json_to_string(&Value::Array(current_arr.clone())));
                    current_arr.clear();
                    current_size = 2;
                }

                // Recursively split the large value
                let sub_chunks = self.split_value(value, current_depth + 1);
                for sub_chunk in sub_chunks {
                    // Each sub-chunk becomes its own array element representation
                    chunks.push(format!("[{}]", sub_chunk));
                }
                continue;
            }

            // Check if adding this entry would exceed chunk size
            if current_size + entry_size > self.config.chunk_size && !current_arr.is_empty() {
                chunks.push(self.json_to_string(&Value::Array(current_arr.clone())));
                current_arr.clear();
                current_size = 2;
            }

            current_arr.push(value.clone());
            current_size += entry_size;
        }

        // Don't forget remaining elements
        if !current_arr.is_empty() {
            chunks.push(self.json_to_string(&Value::Array(current_arr)));
        }

        chunks
    }

    /// Split a JSON string directly.
    pub fn split_json(&self, json_str: &str) -> Vec<String> {
        match serde_json::from_str::<Value>(json_str) {
            Ok(value) => self.split_value(&value, 0),
            Err(_) => {
                // Invalid JSON, return as-is or empty
                if json_str.is_empty() {
                    vec![]
                } else {
                    vec![json_str.to_string()]
                }
            }
        }
    }
}

impl TextSplitter for RecursiveJsonSplitter {
    fn split(&self, text: &str) -> Vec<TextChunk> {
        let chunks = self.split_json(text);

        if self.config.track_indices {
            // For JSON, indices are less meaningful, but we can try to find positions
            let mut result = Vec::new();
            let mut search_start = 0;

            for chunk in chunks {
                // Try to find a key or value from the chunk in the original
                if let Some(pos) = text[search_start..].find(&chunk[..chunk.len().min(20)]) {
                    let start = search_start + pos;
                    result.push(TextChunk::with_indices(chunk, start, start));
                    search_start = start + 1;
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
}

impl Default for RecursiveJsonSplitter {
    fn default() -> Self {
        Self::new(SplitterConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_small_json_no_split() {
        let config = SplitterConfig::new(1000, 0);
        let splitter = RecursiveJsonSplitter::new(config);

        let json = r#"{"name": "John", "age": 30}"#;
        let chunks = splitter.split_text(json);

        assert_eq!(chunks.len(), 1);
        // Compare parsed values since serialization may change whitespace
        let original: Value = serde_json::from_str(json).unwrap();
        let result: Value = serde_json::from_str(&chunks[0]).unwrap();
        assert_eq!(original, result);
    }

    #[test]
    fn test_object_split() {
        let config = SplitterConfig::new(50, 0);
        let splitter = RecursiveJsonSplitter::new(config);

        let json = r#"{"key1": "value1", "key2": "value2", "key3": "value3", "key4": "value4"}"#;
        let chunks = splitter.split_text(json);

        assert!(chunks.len() > 1);
        for chunk in &chunks {
            // Each chunk should be valid JSON
            assert!(serde_json::from_str::<Value>(chunk).is_ok());
        }
    }

    #[test]
    fn test_array_split() {
        let config = SplitterConfig::new(20, 0);
        let splitter = RecursiveJsonSplitter::new(config);

        let json = r#"[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15]"#;
        let chunks = splitter.split_text(json);

        assert!(
            chunks.len() > 1,
            "Expected multiple chunks, got {}: {:?}",
            chunks.len(),
            chunks
        );
        for chunk in &chunks {
            assert!(
                serde_json::from_str::<Value>(chunk).is_ok(),
                "Invalid JSON: {}",
                chunk
            );
        }
    }

    #[test]
    fn test_nested_object_split() {
        let config = SplitterConfig::new(100, 0);
        let splitter = RecursiveJsonSplitter::new(config);

        let json = r#"{"outer": {"inner1": "value1", "inner2": "value2", "inner3": "value3", "inner4": "value4", "inner5": "value5"}}"#;
        let chunks = splitter.split_text(json);

        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_invalid_json() {
        let splitter = RecursiveJsonSplitter::default();
        let chunks = splitter.split_text("not valid json {");

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "not valid json {");
    }

    #[test]
    fn test_empty_json() {
        let splitter = RecursiveJsonSplitter::default();

        assert!(splitter.split_text("").is_empty());
        assert_eq!(splitter.split_text("{}"), vec!["{}"]);
        assert_eq!(splitter.split_text("[]"), vec!["[]"]);
    }

    #[test]
    fn test_large_array_elements() {
        let config = SplitterConfig::new(50, 0);
        let splitter = RecursiveJsonSplitter::new(config);

        let json = r#"[{"id": 1, "name": "Alice"}, {"id": 2, "name": "Bob"}, {"id": 3, "name": "Charlie"}]"#;
        let chunks = splitter.split_text(json);

        assert!(!chunks.is_empty());
    }
}
