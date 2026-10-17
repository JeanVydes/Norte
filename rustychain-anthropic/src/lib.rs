#![forbid(unsafe_code)]
#![doc = "The `Anthropic` Messages wire family.\n\nThe crate ships no model catalog and no vendor table: the host supplies a\n[`Capabilities`], a [`VendorProfile`] naming the endpoint and credentials, and a\n[`ModelRef`]. The adapter maps the neutral core onto the Messages API in both\ndirections.\n\nPrompt caching is expressed with `cache_control` breakpoints on content blocks,\nplaced from the deterministic cache boundaries reconstruction computes, so the\ncached prefix is byte-stable across turns (FR-CACHE).\n\nStructured output has no native field on this wire, so `output_schema` is\ndropped rather than emulated; hosts that need it gate that request on a model\nwhose capability set advertises `StructuredOutputMode`.\n\n[`Capabilities`]: rustychain_core::Capabilities\n[`VendorProfile`]: rustychain_http::VendorProfile\n[`ModelRef`]: rustychain_core::ModelRef\n\nThe `Anthropic` vendor ships as a preset in [`presets::anthropic`], which fixes the base `URL`, the `x-api-key` credential and the required `anthropic-version`."]

pub mod continuity;
pub mod json;
pub mod messages;
pub mod presets;

pub use continuity::AnthropicContinuity;
pub use messages::AnthropicMessages;

/// The vendor-shared `HTTP` media port, re-exported for callers that wire the
/// `Anthropic` adapter.
pub use rustychain_http::HttpMedia as AnthropicMedia;

/// Wire family name of the Messages API.
pub const WIRE_MESSAGES: &str = "anthropic.messages";

/// Builds a capability descriptor from host-supplied `JSON`.
///
/// # Errors
///
/// Returns a [`rustychain_core::Error::Codec`] error when the value does not
/// match the [`rustychain_core::Capabilities`] schema.
pub fn capabilities_from_json(
    value: &serde_json::Value,
) -> rustychain_core::Result<rustychain_core::Capabilities> {
    serde_json::from_value(value.clone())
        .map_err(|error| rustychain_core::Error::Codec(error.to_string()))
}
