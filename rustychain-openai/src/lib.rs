#![forbid(unsafe_code)]
#![doc = "The `OpenAI` wire family.\n\nThe crate ships no model catalog and no vendor table. The host supplies a\n[`Capabilities`] built from whatever source it trusts, a [`VendorProfile`] that\nnames the endpoint and credentials, and a [`ModelRef`] that names the model; any\n`OpenAI`-compatible endpoint is expressed by those three values alone. The\nadapter translates between the neutral core and the vendor wire in both\ndirections.\n\nTwo surfaces are implemented:\n\n- [`OpenAiResponses`]: the Responses API, the primary surface.\n- [`OpenAiChat`]: the `Chat Completions` API, for wire-compatible vendors.\n\n[`Capabilities`]: rustychain_core::Capabilities\n[`VendorProfile`]: rustychain_http::VendorProfile\n[`ModelRef`]: rustychain_core::ModelRef\n\nThe native `OpenAI` vendor ships as a preset in [`presets::openai`]. Compatible vendors such as `OpenRouter` need none: they are a [`VendorProfile`] with a different base `URL`."]

pub mod chat;
pub mod continuity;
pub mod json;
pub mod presets;
pub mod responses;

pub use chat::OpenAiChat;
pub use continuity::OpenAiContinuity;
pub use responses::OpenAiResponses;

/// The vendor-shared `HTTP` media port, re-exported for callers that wire the
/// `OpenAI` adapters.
pub use rustychain_http::HttpMedia as OpenAiMedia;

/// Wire family name of the Responses API.
pub const WIRE_RESPONSES: &str = "openai.responses";

/// Wire family name of the `Chat Completions` API.
pub const WIRE_CHAT: &str = "openai.chat";

/// Builds a capability descriptor from host-supplied `JSON`.
///
/// The framework never carries a model table: the host reads this from its own
/// configuration, an endpoint, or provider documentation.
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
