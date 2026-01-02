use schemars::Schema;
use serde::{Deserialize, Serialize};

use crate::{ThinkingMode, ToolCallingMode};

/// Configuration parameters for text generation by the LLM.
///
/// These parameters influence the behavior and output of the LLM during generation tasks.
///
/// - `temperature`: Controls the randomness of the output. Higher values yield more diverse results.
/// - `top_p`: Nucleus sampling parameter to limit the token selection to a subset of probable tokens.
/// - `top_k`: Limits the token selection to the top K most probable tokens.
/// - `max_output_tokens`: Maximum number of tokens to generate in the output.
/// - `thinking`: Mode for LLM thinking/planning before generating a response.
/// - `candidate_count`: Number of candidate responses to generate.
/// - `stop_sequences`: Optional sequences that, when generated, will stop further output.
/// - `output_schema`: Optional JSON schema to structure the output.
/// - `response_mime_type`: Optional MIME type for the response format.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerationConfig {
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: i32,
    pub max_output_tokens: i32,
    pub thinking_mode: ThinkingMode,
    pub candidate_count: i32,
    pub stop_sequences: Option<Vec<String>>,
    pub output_schema: Option<Schema>,
    pub response_mime_type: Option<String>,
    pub tool_calling_mode: ToolCallingMode,
    pub include_thoughts: bool,
    pub native_tool_handling: bool,
}

impl GenerationConfig {
    /// Creates a new `GenerationConfig` with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = temperature;
        self
    }

    pub fn with_top_p(mut self, top_p: f32) -> Self {
        self.top_p = top_p;
        self
    }

    pub fn with_top_k(mut self, top_k: i32) -> Self {
        self.top_k = top_k;
        self
    }

    pub fn with_max_output_tokens(mut self, max_output_tokens: i32) -> Self {
        self.max_output_tokens = max_output_tokens;
        self
    }

    pub fn with_thinking_mode(mut self, thinking: ThinkingMode) -> Self {
        self.thinking_mode = thinking;
        self
    }

    pub fn with_candidate_count(mut self, candidate_count: i32) -> Self {
        self.candidate_count = candidate_count;
        self
    }

    pub fn with_stop_sequences(mut self, stop_sequences: Vec<String>) -> Self {
        self.stop_sequences = Some(stop_sequences);
        self
    }

    pub fn with_output_schema(mut self, output_schema: Schema) -> Self {
        self.output_schema = Some(output_schema);
        self
    }

    pub fn with_response_mime_type(mut self, mime_type: String) -> Self {
        self.response_mime_type = Some(mime_type);
        self
    }

    pub fn with_tool_calling_mode(mut self, mode: ToolCallingMode) -> Self {
        self.tool_calling_mode = mode;
        self
    }

    pub fn with_include_thoughts(mut self, include: bool) -> Self {
        self.include_thoughts = include;
        self
    }

    pub fn with_native_tool_handling(mut self, native: bool) -> Self {
        self.native_tool_handling = native;
        self
    }
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            temperature: 0.5,
            top_p: 0.9,
            top_k: 40,
            max_output_tokens: 2048,
            thinking_mode: ThinkingMode::None,
            candidate_count: 1,
            stop_sequences: None,
            output_schema: None,
            response_mime_type: None,
            tool_calling_mode: ToolCallingMode::Auto,
            include_thoughts: false,
            native_tool_handling: true,
        }
    }
}
