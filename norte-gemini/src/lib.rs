#![forbid(unsafe_code)]
#![doc = "The `Google` Gemini wire family.\n\nThe crate ships no model catalog and no vendor table: the host supplies a\n[`Capabilities`], a [`VendorProfile`] naming the endpoint and credentials, and a\n[`ModelRef`]. The adapter maps the neutral core onto the `generateContent` API in\nboth directions.\n\nGemini caches prompt prefixes implicitly, so the deterministic cache boundaries\nreconstruction computes are not sent as breakpoints; the request is still built\nbyte-stably so the implicit cache can hit (FR-CACHE). An explicit cached content\ncan also be created with [`GeminiGenerate::create_cached_content`] and referenced\non a turn through the `google.gemini:cachedContent` vendor extension.\n\n[`Capabilities`]: norte_core::Capabilities\n[`VendorProfile`]: norte_http::VendorProfile\n[`ModelRef`]: norte_core::ModelRef"]

pub mod continuity;
pub mod generate;
pub mod json;
pub mod presets;

pub use continuity::GeminiContinuity;
pub use generate::GeminiGenerate;

/// The vendor-shared `HTTP` media port, re-exported for callers that wire the
/// `Gemini` adapter.
pub use norte_http::HttpMedia as GeminiMedia;

/// Wire family name of the `generateContent` API.
pub const WIRE_GENERATE: &str = "google.gemini";

/// Builds a capability descriptor from host-supplied `JSON`.
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
