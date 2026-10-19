use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// A known deviation a wire-compatible vendor has from the reference wire
/// (FR-OAI-2).
///
/// The framework ships no vendor table: a host declares the deviations it knows
/// about on the [`VendorProfile`](crate::VendorProfile), and the adapter omits
/// the corresponding optional request fields. A deviation is therefore an
/// explicit, documented host decision rather than a runtime discovery.
///
/// The set is non-exhaustive: new quirks may be added without a breaking change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Quirk {
    /// The vendor rejects `stream_options` and never reports usage on the
    /// stream, so the adapter omits the object.
    NoStreamUsage,
    /// The vendor rejects `response_format` (structured output). A request that
    /// asks for a structured output schema against such a vendor is a typed
    /// error, never a silent omission.
    NoStructuredOutput,
    /// The vendor rejects `stop` sequences.
    NoStopSequences,
    /// The vendor rejects `seed`.
    NoSeed,
    /// The vendor rejects `reasoning_effort`.
    NoReasoningEffort,
    /// The vendor rejects the `prompt_cache_key` extension field.
    NoPromptCacheKey,
}

/// An ordered set of [`Quirk`]s. Ordered so iteration and serialization are
/// deterministic.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quirks(BTreeSet<Quirk>);

impl Quirks {
    #[must_use]
    pub fn new() -> Self {
        Self(BTreeSet::new())
    }

    pub fn insert(&mut self, quirk: Quirk) {
        self.0.insert(quirk);
    }

    #[must_use]
    pub fn contains(&self, quirk: Quirk) -> bool {
        self.0.contains(&quirk)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = Quirk> + '_ {
        self.0.iter().copied()
    }
}

impl FromIterator<Quirk> for Quirks {
    fn from_iter<I: IntoIterator<Item = Quirk>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quirks_are_an_ordered_set() {
        let quirks: Quirks = [Quirk::NoSeed, Quirk::NoStreamUsage, Quirk::NoSeed]
            .into_iter()
            .collect();
        assert!(quirks.contains(Quirk::NoSeed));
        assert!(quirks.contains(Quirk::NoStreamUsage));
        assert!(!quirks.contains(Quirk::NoStopSequences));
        assert_eq!(
            quirks.iter().collect::<Vec<_>>(),
            vec![Quirk::NoStreamUsage, Quirk::NoSeed]
        );
        assert!(!Quirks::new().contains(Quirk::NoSeed));
    }
}
