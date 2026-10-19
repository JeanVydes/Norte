#![forbid(unsafe_code)]
#![doc = "The `OpenAI` wire family.\n\nThe crate ships no model catalog and no vendor table. The host supplies a\n[`Capabilities`] built from whatever source it trusts, a [`VendorProfile`] that\nnames the endpoint and credentials, and a [`ModelRef`] that names the model; any\n`OpenAI`-compatible endpoint is expressed by those three values alone. The\nadapter translates between the neutral core and the vendor wire in both\ndirections.\n\nTwo surfaces are implemented:\n\n- [`OpenAiResponses`]: the Responses API, the primary surface.\n- [`OpenAiChat`]: the `Chat Completions` API, for wire-compatible vendors. [`OpenAiFiles`] adds the Files API as an optional [`ProviderFiles`](norte_core::ProviderFiles) port for hosts that durable-ize media; it is not part of the request path.\n\n[`Capabilities`]: norte_core::Capabilities\n[`VendorProfile`]: norte_http::VendorProfile\n[`ModelRef`]: norte_core::ModelRef\n\nThe native `OpenAI` vendor ships as a preset in [`presets::openai`]. Compatible vendors such as `OpenRouter` need none: they are a [`VendorProfile`] with a different base `URL`."]

/// The Chat Completions surface, for wire-compatible vendors.
pub mod chat;
/// Continuity interpreted by the `OpenAI` adapters.
pub mod continuity;
/// The Files API, as a [`ProviderFiles`](norte_core::ProviderFiles) port.
pub mod files;
/// Helpers for reading and writing the wire bodies.
pub mod json;
pub mod presets;
/// The Responses API, the primary surface.
pub mod responses;

pub use chat::OpenAiChat;
pub use continuity::OpenAiContinuity;
pub use files::OpenAiFiles;
pub use responses::OpenAiResponses;

/// The vendor-shared `HTTP` media port, re-exported for callers that wire the
/// `OpenAI` adapters.
pub use norte_http::HttpMedia as OpenAiMedia;

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
/// Returns a [`norte_core::Error::Codec`] error when the value does not
/// match the [`norte_core::Capabilities`] schema.
pub fn capabilities_from_json(
    value: &serde_json::Value,
) -> norte_core::Result<norte_core::Capabilities> {
    serde_json::from_value(value.clone())
        .map_err(|error| norte_core::Error::Codec(error.to_string()))
}
