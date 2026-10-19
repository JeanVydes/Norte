use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A vendor moderation label reported for a generated candidate.
///
/// `category`, `probability`, `severity` and `blocked` are the vendor's own
/// labels, kept verbatim: the neutral model does not invent a moderation
/// taxonomy. Every other field — numeric scores included — rides in
/// `extension`, so a vendor label survives without a lossy guess. The neutral
/// model never gates on these; a blocked candidate is signalled by a
/// [`FinishReason::ContentFilter`](crate::FinishReason).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyRating {
    /// The vendor's label for what was detected.
    pub category: String,
    /// The vendor's probability label, kept verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probability: Option<String>,
    /// The vendor's severity label, kept verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    /// Whether the vendor blocked the candidate over this rating.
    #[serde(default)]
    pub blocked: bool,
    /// Anything else the vendor reported, under its own key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extension: BTreeMap<String, Value>,
}
