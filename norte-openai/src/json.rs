use norte_core::{Error, Result};
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
        .ok_or_else(|| Error::Codec(format!("openai wire: missing or invalid `{key}`")))
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
        .ok_or_else(|| Error::Codec(format!("openai wire: missing or invalid `{key}`")))
}

/// Reads an optional string field from a wire object.
pub(crate) fn opt_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Builds an `RFC 2397` data `URL` from a MIME type and bytes.
pub(crate) fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", base64_encode(bytes))
}

/// Encodes bytes as standard base64.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decodes standard base64 into bytes.
///
/// # Errors
///
/// Returns a codec error when the input is not valid base64.
pub(crate) fn base64_decode(text: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|error| Error::Codec(format!("openai wire: invalid base64: {error}")))
}
