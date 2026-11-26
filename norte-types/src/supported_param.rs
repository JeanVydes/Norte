use serde::{Deserialize, Serialize};

/// A model request parameter declared by a host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportedParam {
    /// Sampling temperature.
    Temperature,
    /// Nucleus sampling.
    TopP,
    /// Top-k sampling.
    TopK,
    /// Minimum probability.
    MinP,
    /// Top-a sampling.
    TopA,
    /// Frequency penalty.
    FrequencyPenalty,
    /// Presence penalty.
    PresencePenalty,
    /// Repetition penalty.
    RepetitionPenalty,
    /// Logit bias.
    LogitBias,
    /// Maximum output tokens.
    #[serde(alias = "max_tokens")]
    MaxOutputTokens,
    /// Stop sequences.
    #[serde(alias = "stop")]
    StopSequences,
    /// Random seed.
    Seed,
    /// Model tool declarations.
    Tools,
    /// Tool selection.
    ToolChoice,
    /// Token log probabilities.
    Logprobs,
    /// Alternative token log probabilities.
    TopLogprobs,
    /// Reasoning controls.
    Reasoning,
    /// Reasoning effort.
    ReasoningEffort,
    /// Include reasoning output.
    IncludeReasoning,
    /// Structured response format.
    ResponseFormat,
    /// Structured output support.
    StructuredOutput,
    /// Provider-defined output verbosity.
    Verbosity,
}
