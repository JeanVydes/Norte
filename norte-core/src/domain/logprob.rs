use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// A token's log probability, kept as the IEEE-754 bit pattern of the value.
///
/// The number is opaque vendor data. Storing its bits keeps the event types
/// around it `Eq` (which `f64` is not) without changing what serde writes: the
/// wire form is still the JSON number the vendor reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LogprobValue(u64);

impl LogprobValue {
    /// Wraps a value.
    #[must_use]
    pub fn new(value: f64) -> Self {
        Self(value.to_bits())
    }

    /// Returns the value.
    #[must_use]
    pub fn get(self) -> f64 {
        f64::from_bits(self.0)
    }
}

impl From<f64> for LogprobValue {
    fn from(value: f64) -> Self {
        Self::new(value)
    }
}

impl From<LogprobValue> for f64 {
    fn from(value: LogprobValue) -> Self {
        value.get()
    }
}

impl Serialize for LogprobValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.get())
    }
}

impl<'de> Deserialize<'de> for LogprobValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        f64::deserialize(deserializer).map(Self::new)
    }
}

/// The probability the model assigned to one emitted token, with the runner-up
/// tokens the vendor reported alongside it.
///
/// Log probabilities are informational. They travel in
/// [`StreamEvent::Logprobs`](crate::StreamEvent::Logprobs), never become a
/// [`Part`](crate::Part), and never reach the committed message whether or not
/// the turn asked for them (FR-LP-2). A host that wants them keeps them from
/// the observer channel and decides how long to retain them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenLogprob {
    /// The token text as the vendor spelled it.
    pub token: String,
    /// The log probability of the chosen token.
    pub logprob: LogprobValue,
    /// The token's raw bytes, when the vendor reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<Vec<u8>>,
    /// The alternate tokens the vendor reported for this position, in its own
    /// order. The chosen token may appear among them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<LogprobAlternative>,
    /// Vendor fields this vocabulary does not model, kept verbatim.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extension: BTreeMap<String, Value>,
}

/// One of the runner-up tokens a vendor reported for an emitted position.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogprobAlternative {
    /// The token text as the vendor spelled it.
    pub token: String,
    /// The log probability the vendor assigned to it.
    pub logprob: LogprobValue,
    /// The token's raw bytes, when the vendor reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<Vec<u8>>,
    /// Vendor fields this vocabulary does not model, kept verbatim.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extension: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_round_trips_through_json_exactly() {
        let entry = TokenLogprob {
            token: "…".to_owned(),
            logprob: LogprobValue::new(-0.000_35),
            bytes: Some(vec![0xe2, 0x80, 0xa6]),
            alternatives: vec![LogprobAlternative {
                token: "a".to_owned(),
                logprob: LogprobValue::new(-1.5),
                bytes: None,
                extension: BTreeMap::new(),
            }],
            extension: BTreeMap::new(),
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        assert!(json.contains("-0.00035"), "{json}");
        let back: TokenLogprob = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, entry);
        assert_eq!(
            back.logprob.get().to_bits(),
            entry.logprob.get().to_bits(),
            "the bits survive the round trip"
        );
    }

    #[test]
    fn wrapping_a_value_is_lossless() {
        for value in [0.0, -0.0, 1.5, -12_345.678, f64::NEG_INFINITY] {
            assert_eq!(LogprobValue::new(value).get().to_bits(), value.to_bits());
            assert_eq!(
                f64::from(LogprobValue::from(value)).to_bits(),
                value.to_bits()
            );
        }
    }
}
