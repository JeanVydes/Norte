use std::collections::BTreeSet;
use std::time::Duration;

use norte_core::{Error, Result};

use super::error::{BrowserErrorCode, tool_fail_code};

/// URL schemes a browser action may open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum BrowserScheme {
    /// An unencrypted web page.
    Http,
    /// An encrypted web page.
    Https,
    /// A local file, for tool development rather than normal agent use.
    File,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct HostName(String);

impl HostName {
    fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into().to_ascii_lowercase();
        let valid = !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
            && !value.starts_with('.')
            && !value.ends_with('.');
        if valid {
            Ok(Self(value))
        } else {
            Err(Error::InvalidInput(format!("invalid host name: {value}")))
        }
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where the browser tool may navigate.
///
/// The model names a URL in every navigation, so leaving that URL unchecked
/// turns the tool into a local-file and internal-network reader. HTTP and HTTPS
/// are the only defaults; a host must opt in to `file:` explicitly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlPolicy {
    allowed_schemes: BTreeSet<BrowserScheme>,
    allowed_hosts: BTreeSet<HostName>,
    denied_hosts: BTreeSet<HostName>,
}

impl Default for UrlPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl UrlPolicy {
    /// A policy that allows HTTP and HTTPS to any host.
    #[must_use]
    pub fn new() -> Self {
        Self {
            allowed_schemes: [BrowserScheme::Http, BrowserScheme::Https]
                .into_iter()
                .collect(),
            allowed_hosts: BTreeSet::new(),
            denied_hosts: BTreeSet::new(),
        }
    }

    /// Adds one scheme to the allowlist.
    #[must_use]
    pub fn allow_scheme(mut self, scheme: BrowserScheme) -> Self {
        self.allowed_schemes.insert(scheme);
        self
    }

    /// Restricts navigation to one host. Repeated calls add hosts.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] for an invalid host name.
    pub fn allow_host(mut self, host: impl Into<String>) -> Result<Self> {
        self.allowed_hosts.insert(HostName::new(host)?);
        Ok(self)
    }

    /// Refuses one host even when the scheme is allowed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] for an invalid host name.
    pub fn deny_host(mut self, host: impl Into<String>) -> Result<Self> {
        self.denied_hosts.insert(HostName::new(host)?);
        Ok(self)
    }

    /// Validates a URL against this policy.
    ///
    /// `about:blank` is always safe and is allowed even on a fresh policy.
    ///
    /// # Errors
    ///
    /// Returns a tool failure with the `url_policy` code for an invalid URL,
    /// a scheme outside the allowlist, or a host the policy excludes.
    pub fn validate(&self, url: &str) -> Result<reqwest::Url> {
        let reject = |reason: String| tool_fail_code(reason, BrowserErrorCode::UrlPolicy);
        let parsed =
            reqwest::Url::parse(url).map_err(|error| reject(format!("invalid URL: {error}")))?;
        if url == "about:blank" {
            return Ok(parsed);
        }
        let scheme = match parsed.scheme() {
            "http" => BrowserScheme::Http,
            "https" => BrowserScheme::Https,
            "file" => BrowserScheme::File,
            other => {
                return Err(reject(format!(
                    "the browser tool cannot open a `{other}` URL"
                )));
            }
        };
        if !self.allowed_schemes.contains(&scheme) {
            return Err(reject(format!(
                "the browser tool is not allowed to open a `{parsed_scheme}` URL",
                parsed_scheme = parsed.scheme()
            )));
        }
        if let Some(host) = parsed.host_str() {
            let host = HostName::new(host).map_err(|error| reject(error.to_string()))?;
            if self.denied_hosts.contains(&host) {
                return Err(reject(format!(
                    "the browser tool is not allowed to open {}",
                    host.as_str()
                )));
            }
            if !self.allowed_hosts.is_empty() && !self.allowed_hosts.contains(&host) {
                return Err(reject(format!(
                    "the browser tool is only allowed to open hosts on its allowlist, not {}",
                    host.as_str()
                )));
            }
        }
        Ok(parsed)
    }
}

/// Host-owned limits for the browser tool.
///
/// None of these values are model-visible. A request that can name the CDP
/// endpoint or raise its own bounds can reach another browser and spend far
/// more context than the host chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserLimits {
    /// The CDP endpoint the tool drives.
    pub endpoint: String,
    /// Which URLs the tool may open.
    pub url_policy: UrlPolicy,
    /// Longest sleep a `wait` action may request, in seconds.
    pub max_wait_seconds: u64,
    /// How long to wait for a page load, in seconds.
    pub load_timeout_seconds: u64,
    /// Timeout for one CDP command, in seconds.
    pub cdp_timeout_seconds: u64,
    /// Most interactive elements one answer carries.
    pub max_elements: usize,
    /// Most characters one `read_page` answer carries.
    pub max_page_chars: usize,
}

impl Default for BrowserLimits {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:9222".to_owned(),
            url_policy: UrlPolicy::new(),
            max_wait_seconds: 30,
            load_timeout_seconds: 10,
            cdp_timeout_seconds: 15,
            max_elements: 80,
            max_page_chars: 20_000,
        }
    }
}

impl BrowserLimits {
    /// Points the tool at a different CDP endpoint.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        let endpoint = endpoint.into();
        self.endpoint.clear();
        self.endpoint.push_str(endpoint.trim_end_matches('/'));
        self
    }

    /// Replaces the URL policy.
    #[must_use]
    pub fn with_url_policy(mut self, policy: UrlPolicy) -> Self {
        self.url_policy = policy;
        self
    }

    /// Validates a URL the model asked the browser to open.
    ///
    /// # Errors
    ///
    /// Returns a tool failure with the `url_policy` code when the URL is
    /// outside the policy.
    pub fn validate_url(&self, url: &str) -> Result<reqwest::Url> {
        self.url_policy.validate(url)
    }

    pub(super) fn command_timeout(&self) -> Duration {
        Duration::from_secs(self.cdp_timeout_seconds)
    }

    pub(super) fn load_timeout(&self) -> Duration {
        Duration::from_secs(self.load_timeout_seconds)
    }
}
