use reqwest::header::{HeaderName, HeaderValue};

use norte_core::{Error, Result};

use crate::secret::Secret;

/// How a vendor expects credentials on the wire. Wire-compatible vendors differ
/// almost exclusively in this and in the base URL, so it is data, not code.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub enum AuthScheme {
    /// No credentials.
    #[default]
    None,
    /// `Authorization: Bearer <token>`.
    Bearer(Secret<String>),
    /// An arbitrary header, e.g. `x-api-key` or `api-key`.
    Header {
        /// The header's name.
        name: String,
        /// The header's value, held as a secret.
        value: Secret<String>,
    },
    /// A query parameter, for vendors that sign the URL.
    Query {
        /// The parameter's name.
        name: String,
        /// The parameter's value, held as a secret.
        value: Secret<String>,
    },
}

impl AuthScheme {
    /// A bearer-token scheme.
    #[must_use]
    pub fn bearer(token: impl Into<String>) -> Self {
        Self::Bearer(Secret::new(token.into()))
    }

    /// A scheme that sets one header.
    #[must_use]
    pub fn header(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self::Header {
            name: name.into(),
            value: Secret::new(value.into()),
        }
    }

    /// A scheme that sets one query parameter.
    #[must_use]
    pub fn query(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self::Query {
            name: name.into(),
            value: Secret::new(value.into()),
        }
    }

    /// The header this scheme sets, if any.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when the header name or value is not
    /// valid for `HTTP`.
    pub fn headers(&self) -> Result<Vec<(HeaderName, HeaderValue)>> {
        match self {
            AuthScheme::Bearer(token) => Ok(vec![(
                HeaderName::from_static("authorization"),
                header_value(&format!("Bearer {}", token.expose()))?,
            )]),
            AuthScheme::Header { name, value } => Ok(vec![(
                HeaderName::from_bytes(name.as_bytes())
                    .map_err(|error| Error::InvalidInput(format!("auth header name: {error}")))?,
                header_value(value.expose())?,
            )]),
            AuthScheme::None | AuthScheme::Query { .. } => Ok(Vec::new()),
        }
    }

    /// The query parameter this scheme sets, if any.
    #[must_use]
    pub fn query_param(&self) -> Option<(&str, &str)> {
        match self {
            AuthScheme::Query { name, value } => Some((name, value.expose())),
            _ => None,
        }
    }
}

fn header_value(value: &str) -> Result<HeaderValue> {
    HeaderValue::from_str(value)
        .map_err(|error| Error::InvalidInput(format!("auth header value: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_becomes_an_authorization_header() {
        let headers = AuthScheme::bearer("sk-1").headers().expect("headers");
        assert_eq!(headers[0].0.as_str(), "authorization");
        assert_eq!(headers[0].1.to_str().expect("value"), "Bearer sk-1");
    }

    #[test]
    fn header_scheme_exposes_a_query_param() {
        let scheme = AuthScheme::query("key", "abc");
        assert_eq!(scheme.query_param(), Some(("key", "abc")));
        assert!(
            scheme.headers().expect("headers").is_empty(),
            "a query-param scheme sends no headers"
        );
    }
}
