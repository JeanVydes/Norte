use rustychain_core::{Error, Result};
use serde_json::Value;

/// Reads a required `u32` field from a wire object.
///
/// # Errors
///
/// Returns a codec error when the field is missing or is not a `u32`.
pub(crate) fn u32_field(value: &Value, key: &str) -> Result<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| Error::Codec(format!("anthropic wire: missing or invalid `{key}`")))
}

/// Reads a required string field from a wire object.
///
/// # Errors
///
/// Returns a codec error when the field is missing or is not a string.
pub(crate) fn str_field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Codec(format!("anthropic wire: missing or invalid `{key}`")))
}

/// Reads an optional string field from a wire object.
pub(crate) fn opt_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Merges a later wire object into an earlier one.
///
/// Anthropic spreads one record over several events — `message_start` carries
/// the input and cache counters, `message_delta` the output — so the object is
/// only complete once every event has been folded in. A value seen later
/// replaces what was seen earlier, and two objects merge field by field.
pub(crate) fn merge(into: &mut Value, from: &Value) {
    match (into, from) {
        (Value::Object(target), Value::Object(source)) => {
            for (key, value) in source {
                match target.entry(key.clone()) {
                    serde_json::map::Entry::Occupied(mut slot) => merge(slot.get_mut(), value),
                    serde_json::map::Entry::Vacant(slot) => {
                        slot.insert(value.clone());
                    }
                }
            }
        }
        (slot, value) => *slot = value.clone(),
    }
}

/// Encodes bytes as standard base64.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
