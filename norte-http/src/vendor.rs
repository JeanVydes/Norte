use std::collections::BTreeMap;

use url::Url;

use norte_core::{Error, ProviderId, Result, WireFamily};

use crate::auth::AuthScheme;
use crate::quirk::{Quirk, Quirks};

/// Everything that makes a wire-compatible vendor reachable: where to send
/// requests, how to authenticate, which extra headers to attach and which known
/// deviations to honour.
///
/// The framework ships no vendor table. Hosts build a profile from their own
/// configuration and hand it to an adapter.
#[derive(Clone, Debug)]
pub struct VendorProfile {
    /// The provider this profile reaches.
    pub provider: ProviderId,
    /// The wire family the provider speaks.
    pub wire: WireFamily,
    /// Where requests are sent.
    pub base_url: Url,
    /// How requests are authenticated.
    pub auth: AuthScheme,
    /// Extra headers attached to every request.
    pub headers: BTreeMap<String, String>,
    /// The deviations this vendor has from the reference wire.
    pub quirks: Quirks,
}

impl VendorProfile {
    /// A profile for `provider` on `wire`, with `base_url` and no auth, extra
    /// headers or quirks.
    ///
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
            quirks: Quirks::new(),
        })
    }

    /// Sets the authentication scheme.
    #[must_use]
    pub fn with_auth(mut self, auth: AuthScheme) -> Self {
        self.auth = auth;
        self
    }

    /// Adds one header attached to every request.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    /// Declares a known deviation from the reference wire (FR-OAI-2).
    #[must_use]
    pub fn with_quirk(mut self, quirk: Quirk) -> Self {
        self.quirks.insert(quirk);
        self
    }

    /// The deviations this profile declares.
    #[must_use]
    pub fn quirks(&self) -> &Quirks {
        &self.quirks
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

    #[test]
    fn quirks_are_declared_on_the_profile() {
        let profile = profile("https://api.example.com/v1")
            .with_quirk(Quirk::NoStreamUsage)
            .with_quirk(Quirk::NoSeed);
        assert!(profile.quirks().contains(Quirk::NoStreamUsage));
        assert!(profile.quirks().contains(Quirk::NoSeed));
        assert!(!profile.quirks().contains(Quirk::NoStructuredOutput));
    }
}
