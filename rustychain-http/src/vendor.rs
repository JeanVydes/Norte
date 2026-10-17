use std::collections::BTreeMap;

use url::Url;

use rustychain_core::{Error, ProviderId, Result, WireFamily};

use crate::auth::AuthScheme;

/// Everything that makes a wire-compatible vendor reachable: where to send
/// requests, how to authenticate and which extra headers to attach.
///
/// The framework ships no vendor table. Hosts build a profile from their own
/// configuration and hand it to an adapter.
#[derive(Clone, Debug)]
pub struct VendorProfile {
    pub provider: ProviderId,
    pub wire: WireFamily,
    pub base_url: Url,
    pub auth: AuthScheme,
    pub headers: BTreeMap<String, String>,
}

impl VendorProfile {
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when `base_url` is not a valid absolute
    /// `URL`.
    pub fn new(provider: ProviderId, wire: WireFamily, base_url: &str) -> Result<Self> {
        let base_url = Url::parse(base_url)
            .map_err(|error| Error::InvalidInput(format!("vendor base url: {error}")))?;
        Ok(Self {
            provider,
            wire,
            base_url,
            auth: AuthScheme::None,
            headers: BTreeMap::new(),
        })
    }

    #[must_use]
    pub fn with_auth(mut self, auth: AuthScheme) -> Self {
        self.auth = auth;
        self
    }

    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    /// Joins `path` onto the base URL, keeping the base path intact even when it
    /// has no trailing slash.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when the joined `URL` is invalid.
    pub fn endpoint(&self, path: &str) -> Result<Url> {
        let mut base = self.base_url.clone();
        if !base.path().ends_with('/') {
            let with_slash = format!("{}/", base.path());
            base.set_path(&with_slash);
        }
        base.join(path.trim_start_matches('/'))
            .map_err(|error| Error::InvalidInput(format!("endpoint: {error}")))
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn profile(base: &str) -> VendorProfile {
        VendorProfile::new(
            ProviderId::new("acme").expect("provider"),
            WireFamily::new("openai-chat").expect("wire"),
            base,
        )
        .expect("profile")
    }

    #[test]
    fn endpoint_keeps_the_base_path_without_a_trailing_slash() {
        let url = profile("https://api.example.com/v1")
            .endpoint("/responses")
            .expect("url");
        assert_eq!(url.as_str(), "https://api.example.com/v1/responses");
    }

    #[test]
    fn endpoint_handles_a_trailing_slash() {
        let url = profile("https://api.example.com/v1/")
            .endpoint("responses")
            .expect("url");
        assert_eq!(url.as_str(), "https://api.example.com/v1/responses");
    }

    #[test]
    fn wire_family_round_trips() {
        let model_wire = WireFamily::from_str("anthropic-messages").expect("wire");
        assert_eq!(model_wire.as_str(), "anthropic-messages");
    }
}
