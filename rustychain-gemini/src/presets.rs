//! Known vendors, expressed as named constructors.
//!
//! The preset fixes the `generateContent` wire's vendor data — base `URL`,
//! provider id and the `x-goog-api-key` credential header — so the host supplies
//! only its API key, the model id and the capability descriptor.
//! [`GeminiGenerate::new`] stays available for proxies and self-hosted
//! endpoints.

use rustychain_core::{Capabilities, ModelRef, ProviderId, Result, WireFamily};
use rustychain_http::{AuthScheme, VendorProfile};

use crate::WIRE_GENERATE;
use crate::generate::GeminiGenerate;

/// Provider id of `Google`.
const GOOGLE_PROVIDER: &str = "google";
/// Base `URL` of the `Google` `Generative Language` API.
const GOOGLE_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

/// The `Google` `Gemini` vendor profile.
///
/// # Errors
///
/// Returns an error when the provider id, wire family or base `URL` is invalid.
pub fn gemini_profile(api_key: impl Into<String>) -> Result<VendorProfile> {
    let profile = VendorProfile::new(
        ProviderId::new(GOOGLE_PROVIDER)?,
        WireFamily::new(WIRE_GENERATE)?,
        GOOGLE_BASE,
    )?;
    Ok(profile.with_auth(AuthScheme::header("x-goog-api-key", api_key)))
}

/// A `Gemini` model over the `generateContent` wire.
///
/// # Errors
///
/// Returns an error when the model id is invalid or the default `HTTP` client
/// cannot be built.
pub fn gemini(
    model: &str,
    api_key: impl Into<String>,
    capabilities: Capabilities,
) -> Result<GeminiGenerate> {
    let profile = gemini_profile(api_key)?;
    let reference = ModelRef::new(profile.provider.clone(), model.parse()?);
    GeminiGenerate::new(reference, profile, capabilities)
}

#[cfg(test)]
mod tests {
    use rustychain_core::{Modality, Model};

    use super::*;

    fn capabilities() -> Capabilities {
        Capabilities::builder()
            .input(Modality::Text)
            .output(Modality::Text)
            .build()
    }

    #[test]
    fn gemini_targets_the_generate_content_base() {
        let profile = gemini_profile("AIza-test").expect("profile");
        assert_eq!(profile.provider.as_str(), "google");
        assert_eq!(profile.wire.as_str(), WIRE_GENERATE);
        assert_eq!(
            profile.base_url.as_str(),
            "https://generativelanguage.googleapis.com/v1beta"
        );
        let headers = profile.auth.headers().expect("headers");
        assert_eq!(headers[0].0.as_str(), "x-goog-api-key");
        assert_eq!(headers[0].1.to_str().expect("value"), "AIza-test");
    }

    #[test]
    fn gemini_builds_a_model_from_host_inputs() {
        let model = gemini("gemini-2.5-pro", "AIza-test", capabilities()).expect("model");
        assert_eq!(model.reference().provider.as_str(), "google");
        assert_eq!(model.reference().model.as_str(), "gemini-2.5-pro");
    }
}
