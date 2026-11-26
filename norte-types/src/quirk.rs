use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A known deviation from the expected behavior of a wire protocol.
///
/// Model parameter support belongs in capabilities. Quirks describe how an
/// adapter must accommodate an endpoint or model's deviation from its wire.
///
/// A host declares applicable deviations on the
/// `VendorProfile`. Adapters omit automatic behavior
/// where safe and return typed capability errors for explicitly requested
/// incompatible features, including features supplied through extensions.
///
/// Each quirk must be declared by the host and supported by the selected adapter.
/// Adapter constructors reject quirks that have no meaning for their wire.
///
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quirk {
    /// The wire does not accept a request to include token usage in the stream.
    /// Omit the automatic usage request.
    NoStreamUsage,
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
/// an array of snake-case names, such as `["no_stream_usage", "no_prompt_cache_key"]`.
pub type Quirks = BTreeSet<Quirk>;
