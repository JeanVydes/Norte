use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// A known deviation a wire-compatible vendor has from the reference wire
/// (FR-OAI-2).
///
/// The framework ships no vendor table: a host declares the deviations it knows
/// about on the [`VendorProfile`](crate::VendorProfile). Adapters omit automatic
/// fields and reject explicitly requested incompatible features with a typed
/// capability error, including features supplied through vendor extensions.
/// Deviations are explicit host decisions rather than runtime discoveries.
///
/// The set is non-exhaustive: new quirks may be added without a breaking change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Quirk {
    /// Chat Completions rejects `stream_options`; omit automatic stream usage.
    /// Does not apply to Responses, which has no such request field.
    NoStreamUsage,
    /// The vendor rejects structured output: `response_format` on Chat
    /// Completions or `text.format` on Responses. Explicit requests are typed
    /// capability errors, never silent omissions.
    NoStructuredOutput,
    /// Chat Completions rejects `stop` sequences. Does not apply to Responses,
    /// which does not map neutral stop sequences.
    NoStopSequences,
    /// Chat Completions rejects `seed`. Does not apply to Responses, which does
    /// not map the neutral seed parameter.
    NoSeed,
    /// The vendor rejects `reasoning_effort` on Chat Completions or
    /// `reasoning.effort` on Responses.
    NoReasoningEffort,
    /// The vendor rejects `prompt_cache_key` on Chat Completions and Responses.
    /// Omit the automatic session key; reject an explicit vendor extension.
    NoPromptCacheKey,
}

/// An ordered set of [`Quirk`]s with the standard [`BTreeSet`] API.
///
/// Iteration borrows the quirks in deterministic enum order. Serialization is
/// an array of snake-case names, such as `["no_stream_usage", "no_seed"]`.
pub type Quirks = BTreeSet<Quirk>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quirks_preserve_the_serialized_array_of_names() {
        let quirks: Quirks = [Quirk::NoSeed, Quirk::NoStreamUsage, Quirk::NoSeed]
            .into_iter()
            .collect();
        let wire = serde_json::json!(["no_stream_usage", "no_seed"]);
        assert_eq!(serde_json::to_value(&quirks).expect("serialize"), wire);
        assert_eq!(
            serde_json::from_value::<Quirks>(wire).expect("deserialize"),
            quirks
        );
    }
}
