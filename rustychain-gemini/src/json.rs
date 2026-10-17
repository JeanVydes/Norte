use rustychain_core::{Error, Result};
use serde_json::Value;

/// Reads an optional string field from a wire object.
pub(crate) fn opt_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Encodes bytes as standard base64.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decodes standard base64.
///
/// # Errors
///
/// Returns a codec error when the payload is not valid base64.
pub(crate) fn base64_decode(encoded: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| Error::Codec(format!("gemini wire: {error}")))
}
