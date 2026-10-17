//! Known vendors, expressed as named constructors.
//!
//! The preset fixes the Messages wire's vendor data — base `URL`, provider id,
//! the `x-api-key` credential header and the required `anthropic-version` — so
//! the host supplies only its API key, the model id and the capability
//! descriptor. [`AnthropicMessages::new`] stays available for proxies and
//! self-hosted endpoints.

use rustychain_core::{Capabilities, ModelRef, ProviderId, Result, WireFamily};
use rustychain_http::{AuthScheme, VendorProfile};

use crate::WIRE_MESSAGES;
use crate::messages::AnthropicMessages;

/// Provider id of `Anthropic`.
const ANTHROPIC_PROVIDER: &str = "anthropic";
/// Base `URL` of the `Anthropic` Messages API.
const ANTHROPIC_BASE: &str = "https://api.anthropic.com/v1";
/// The Messages API version this adapter targets.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The `Anthropic` vendor profile.
///
/// # Errors
///
/// Returns an error when the provider id, wire family or base `URL` is invalid.
pub fn anthropic_profile(api_key: impl Into<String>) -> Result<VendorProfile> {
    let profile = VendorProfile::new(
        ProviderId::new(ANTHROPIC_PROVIDER)?,
        WireFamily::new(WIRE_MESSAGES)?,
        ANTHROPIC_BASE,
    )?;
    Ok(profile
        .with_auth(AuthScheme::header("x-api-key", api_key))
        .with_header("anthropic-version", ANTHROPIC_VERSION))
}

/// An `Anthropic` model over the Messages wire.
///
/// # Errors
///
/// Returns an error when the model id is invalid or the default `HTTP` client
/// cannot be built.
pub fn anthropic(
    model: &str,
    api_key: impl Into<String>,
    capabilities: Capabilities,
) -> Result<AnthropicMessages> {
    let profile = anthropic_profile(api_key)?;
    let reference = ModelRef::new(profile.provider.clone(), model.parse()?);
    AnthropicMessages::new(reference, profile, capabilities)
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
    fn anthropic_targets_the_messages_endpoint_with_required_headers() {
        let profile = anthropic_profile("sk-ant-test").expect("profile");
        assert_eq!(profile.provider.as_str(), "anthropic");
        assert_eq!(profile.wire.as_str(), WIRE_MESSAGES);
        assert_eq!(
            profile.endpoint("messages").expect("url").as_str(),
            "https://api.anthropic.com/v1/messages"
        );
        let headers = profile.auth.headers().expect("headers");
        assert_eq!(headers[0].0.as_str(), "x-api-key");
        assert_eq!(
            profile.headers.get("anthropic-version").map(String::as_str),
            Some(ANTHROPIC_VERSION)
        );
    }

    #[test]
    fn anthropic_builds_a_model_from_host_inputs() {
        let model = anthropic("claude-sonnet-4-5", "sk-ant-test", capabilities()).expect("model");
        assert_eq!(model.reference().provider.as_str(), "anthropic");
        assert_eq!(model.reference().model.as_str(), "claude-sonnet-4-5");
    }
}
