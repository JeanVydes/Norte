use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// A known deviation from the expected behavior of a wire protocol.
///
/// A quirk describes an endpoint's deviation from its wire, not whether an
/// individual model supports a generation parameter. Model-specific support
/// belongs in model capability metadata. For example, `NoTemperature` is for
/// an endpoint that rejects the wire field across its models, not a model that
/// happens not to support temperature.
///
/// A host declares applicable deviations on the
/// [`VendorProfile`](crate::VendorProfile). Adapters omit automatic behavior
/// where safe and return typed capability errors for explicitly requested
/// incompatible features, including features supplied through extensions.
///
/// Each quirk must be declared by the host and supported by the selected adapter.
/// Adapter constructors reject quirks that have no meaning for their wire.
///
/// | Quirk | Chat | Responses | Messages | Generate |
/// | --- | --- | --- | --- | --- |
/// | NoStreamUsage | yes | — | — | — |
/// | NoStructuredOutput | yes | yes | — | yes |
/// | NoStopSequences | yes | — | yes | yes |
/// | NoTemperature, NoTopP | yes | yes | yes | yes |
/// | NoSeed | yes | — | — | yes |
/// | NoReasoningEffort | yes | yes | yes | yes |
/// | NoPromptCacheKey | yes | yes | — | — |
/// | NoPromptCacheBreakpoints | — | yes | yes | — |

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quirk {
    /// The wire does not accept a request to include token usage in the stream.
    /// Omit the automatic usage request.
    NoStreamUsage,
    /// The wire does not accept structured output. Explicit requests are typed
    /// capability errors, never silent omissions.
    NoStructuredOutput,
    /// The wire does not accept stop sequences.
    NoStopSequences,
    /// The wire does not accept a temperature parameter.
    NoTemperature,
    /// The wire does not accept a top-p sampling parameter.
    NoTopP,
    /// The wire does not accept a generation seed.
    NoSeed,
    /// The wire does not accept a requested reasoning effort.
    NoReasoningEffort,
    /// The wire does not accept a session-scoped prompt cache key. Omit the
    /// automatic key and reject an explicit extension that tries to add one.
    NoPromptCacheKey,
    /// The wire does not accept explicit prompt cache boundary markers. Omit
    /// automatic markers and reject an extension that tries to add one.
    NoPromptCacheBreakpoints,
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
