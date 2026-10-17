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
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extension: BTreeMap<String, Value>,
}
