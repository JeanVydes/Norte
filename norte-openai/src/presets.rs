//! The native `OpenAI` vendor, as a named constructor.
//!
//! OpenAI-compatible vendors — `OpenRouter` among them — need no preset: they
//! differ from `OpenAI` only in base `URL` and credentials, which the host
//! already expresses with a [`VendorProfile`] and the generic
//! [`OpenAiChat::new`](crate::OpenAiChat::new) /
//! [`OpenAiResponses::new`] constructors.

use norte_core::{Capabilities, ModelRef, ProviderId, Result, WireFamily};
use norte_http::{AuthScheme, VendorProfile};

use crate::WIRE_RESPONSES;
use crate::responses::OpenAiResponses;

/// Provider id of `OpenAI`.
const OPENAI_PROVIDER: &str = "openai";
/// Base `URL` of the `OpenAI` API.
const OPENAI_BASE: &str = "https://api.openai.com/v1";

/// The `OpenAI` vendor profile: the Responses wire, bearer authentication.
///
/// # Errors
///
/// Returns an error when the provider id, wire family or base `URL` is invalid.
pub fn openai_profile(api_key: impl Into<String>) -> Result<VendorProfile> {
    let profile = VendorProfile::new(
        ProviderId::new(OPENAI_PROVIDER)?,
        WireFamily::new(WIRE_RESPONSES)?,
        OPENAI_BASE,
    )?;
    Ok(profile.with_auth(AuthScheme::bearer(api_key)))
}

/// An `OpenAI` model over the Responses wire, the primary surface.
///
/// # Errors
///
/// Returns an error when the model id is invalid or the default `HTTP` client
/// cannot be built.
pub fn openai(
    model: &str,
    api_key: impl Into<String>,
    capabilities: Capabilities,
) -> Result<OpenAiResponses> {
    let profile = openai_profile(api_key)?;
    let reference = ModelRef::new(profile.provider.clone(), model.parse()?);
    OpenAiResponses::new(reference, profile, capabilities)
}

#[cfg(test)]
mod tests {
    use norte_core::{Modality, Model};

    use super::*;

    fn capabilities() -> Capabilities {
        Capabilities::builder()
            .input(Modality::Text)
            .output(Modality::Text)
            .build()
    }

    #[test]
    fn openai_targets_the_responses_endpoint() {
        let profile = openai_profile("sk-test").expect("profile");
        assert_eq!(profile.provider.as_str(), "openai");
        assert_eq!(profile.wire.as_str(), WIRE_RESPONSES);
        assert_eq!(
            profile.endpoint("responses").expect("url").as_str(),
            "https://api.openai.com/v1/responses"
        );
        let headers = profile.auth.headers().expect("headers");
        assert_eq!(headers[0].1.to_str().expect("value"), "Bearer sk-test");
    }

    #[test]
    fn openai_builds_a_model_from_host_inputs() {
        let model = openai("gpt-5", "sk-test", capabilities()).expect("model");
        assert_eq!(model.reference().provider.as_str(), "openai");
        assert_eq!(model.reference().model.as_str(), "gpt-5");
    }
}
