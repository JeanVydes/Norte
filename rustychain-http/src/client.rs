use std::pin::Pin;
use std::time::Duration;

use futures_core::Stream;
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, RETRY_AFTER};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde_json::Value;

use rustychain_core::{Error, ProviderError, ProviderErrorKind, Result};

use crate::retry::{RetryPolicy, parse_retry_after};
use crate::sse::{SseDecoder, SseEvent};
use crate::vendor::VendorProfile;

/// A stream of decoded `SSE` events.
pub type SseStream = Pin<Box<dyn Stream<Item = Result<SseEvent>> + Send + 'static>>;

/// The shared transport. It owns retries, auth application and vendor headers,
/// so adapters only deal with their own wire mapping.
#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    retry: RetryPolicy,
}

impl HttpClient {
    /// # Errors
    ///
    /// Returns [`Error::Provider`] when the underlying `HTTP` client cannot be
    /// built.
    pub fn new() -> Result<Self> {
        let inner = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| transport(&error))?;
        Ok(Self {
            inner,
            retry: RetryPolicy::default(),
        })
    }

    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    #[must_use]
    pub fn raw(&self) -> &reqwest::Client {
        &self.inner
    }

    /// Sends a `JSON` `POST`, retrying on retryable failures, and decodes the
    /// `JSON` response body.
    ///
    /// # Errors
    ///
    /// Returns a mapped [`Error::Provider`] for any non-success status or
    /// transport failure, and [`Error::Json`] when the body is not valid `JSON`.
    pub async fn send_post_json(
        &self,
        profile: &VendorProfile,
        path: &str,
        body: &Value,
    ) -> Result<Value> {
        self.send_json(profile, Method::POST, path, Some(body)).await
    }

    /// Sends a `JSON` request, retrying on retryable failures, and decodes the
    /// `JSON` response body.
    ///
    /// # Errors
    ///
    /// Returns a mapped [`Error::Provider`] for any non-success status or
    /// transport failure, and [`Error::Json`] when the body is not valid `JSON`.
    pub async fn send_json(
        &self,
        profile: &VendorProfile,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value> {
        let mut retries = 0u32;
        loop {
            let sent = self
                .send_once(profile, method.clone(), path, body, "application/json")
                .await;
            match sent {
                Ok(response) => return read_json(response).await,
                Err(error) => match self.backoff(retries, &error) {
                    Some(delay) => {
                        tracing::debug!(retries, delay = ?delay, "retrying HTTP request");
                        tokio::time::sleep(delay).await;
                        retries += 1;
                    }
                    None => return Err(error),
                },
            }
        }
    }

    /// Opens an `SSE` stream, retrying the connection on retryable failures.
    ///
    /// # Errors
    ///
    /// Returns a mapped [`Error::Provider`] when the connection cannot be
    /// established within the retry budget.
    pub async fn send_sse(
        &self,
        profile: &VendorProfile,
        path: &str,
        body: &Value,
    ) -> Result<SseStream> {
        let mut retries = 0u32;
        loop {
            let sent = self
                .send_once(profile, Method::POST, path, Some(body), "text/event-stream")
                .await;
            match sent {
                Ok(response) => return Ok(decode_sse(response)),
                Err(error) => match self.backoff(retries, &error) {
                    Some(delay) => {
                        tracing::debug!(retries, delay = ?delay, "retrying SSE connection");
                        tokio::time::sleep(delay).await;
                        retries += 1;
                    }
                    None => return Err(error),
                },
            }
        }
    }

    fn backoff(&self, retries: u32, error: &Error) -> Option<Duration> {
        if !is_retryable(error) {
            return None;
        }
        self.retry.delay(retries, error.retry_after())
    }

    fn request(&self, profile: &VendorProfile, method: Method, path: &str) -> Result<RequestBuilder> {
        let url = profile.endpoint(path)?;
        let mut builder = self.inner.request(method, url);
        for (name, value) in profile.auth.headers()? {
            builder = builder.header(name, value);
        }
        for (name, value) in &profile.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some((name, value)) = profile.auth.query_param() {
            builder = builder.query(&[(name, value)]);
        }
        Ok(builder)
    }

    async fn send_once(
        &self,
        profile: &VendorProfile,
        method: Method,
        path: &str,
        body: Option<&Value>,
        accept: &str,
    ) -> Result<Response> {
        let builder = self.request(profile, method, path)?.header(ACCEPT, accept);
        let builder = match body {
            Some(body) => builder.json(body),
            None => builder,
        };
        let response = builder.send().await.map_err(|error| transport(&error))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after);
        let text = response.text().await.unwrap_or_default();
        Err(status_error(status, &text, retry_after))
    }
}

async fn read_json(response: Response) -> Result<Value> {
    let bytes = response.bytes().await.map_err(|error| transport(&error))?;
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(Error::from)
}

fn decode_sse(response: Response) -> SseStream {
    let mut decoder = SseDecoder::new();
    let stream = response.bytes_stream().flat_map(move |chunk| {
        let events: Vec<Result<SseEvent>> = match chunk {
            Ok(bytes) => decoder.push(&bytes).into_iter().map(Ok).collect(),
            Err(error) => vec![Err(transport(&error))],
        };
        futures_util::stream::iter(events)
    });
    Box::pin(stream)
}

#[must_use]
pub fn is_retryable(error: &Error) -> bool {
    matches!(
        error.provider_kind(),
        Some(
            ProviderErrorKind::RateLimited
                | ProviderErrorKind::ServerError
                | ProviderErrorKind::ModelOverloaded
                | ProviderErrorKind::Transport
        )
    )
}

fn transport(error: &reqwest::Error) -> Error {
    let kind = if error.is_timeout() || error.is_connect() {
        ProviderErrorKind::Transport
    } else if error.is_decode() {
        ProviderErrorKind::Decode
    } else {
        ProviderErrorKind::Transport
    };
    Error::provider(ProviderError::new(kind, error.to_string()))
}

fn status_error(status: StatusCode, body: &str, retry_after: Option<Duration>) -> Error {
    let code = error_code(body);
    let kind = match (status.as_u16(), code.as_deref()) {
        (429, Some("insufficient_quota" | "billing_hard_limit_reached")) => {
            ProviderErrorKind::QuotaExceeded
        }
        (429, _) => ProviderErrorKind::RateLimited,
        (401, _) => ProviderErrorKind::Authentication,
        (403, _) => ProviderErrorKind::Permission,
        (404, _) => ProviderErrorKind::NotFound,
        (529, _) => ProviderErrorKind::ModelOverloaded,
        (code, _) if (500..=599).contains(&code) => ProviderErrorKind::ServerError,
        (code, _) if (400..=499).contains(&code) => ProviderErrorKind::InvalidRequest,
        _ => ProviderErrorKind::Other,
    };
    let message = error_message(body).unwrap_or_else(|| body.to_owned());
    let mut provider = ProviderError::new(kind, message).with_status(status.as_u16());
    if let Some(code) = code {
        provider = provider.with_code(code);
    }
    if let Some(retry_after) = retry_after {
        provider = provider.with_retry_after(retry_after);
    }
    Error::provider(provider)
}

fn error_code(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let error = value.get("error").unwrap_or(&value);
    error
        .get("code")
        .or_else(|| error.get("type"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let error = value.get("error").unwrap_or(&value);
    if let Some(message) = error.get("message").and_then(Value::as_str) {
        return Some(message.to_owned());
    }
    error.as_str().map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_map_to_the_common_taxonomy() {
        assert_eq!(kind_of(401), ProviderErrorKind::Authentication);
        assert_eq!(kind_of(403), ProviderErrorKind::Permission);
        assert_eq!(kind_of(404), ProviderErrorKind::NotFound);
        assert_eq!(kind_of(429), ProviderErrorKind::RateLimited);
        assert_eq!(kind_of(500), ProviderErrorKind::ServerError);
        assert_eq!(kind_of(529), ProviderErrorKind::ModelOverloaded);
        assert_eq!(kind_of(400), ProviderErrorKind::InvalidRequest);
    }

    #[test]
    fn vendor_error_body_is_preserved() {
        let body = r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#;
        let error = status_error(
            StatusCode::TOO_MANY_REQUESTS,
            body,
            Some(Duration::from_secs(3)),
        );
        let Error::Provider(provider) = error else {
            panic!("expected a provider error");
        };
        assert_eq!(provider.code.as_deref(), Some("rate_limit_error"));
        assert_eq!(provider.message, "slow down");
        assert_eq!(provider.retry_after, Some(Duration::from_secs(3)));
        assert_eq!(provider.status, Some(429));
    }

    #[test]
    fn retryability_follows_the_kind() {
        assert!(is_retryable(&status_error(StatusCode::TOO_MANY_REQUESTS, "", None)));
        assert!(is_retryable(&status_error(StatusCode::BAD_GATEWAY, "", None)));
        assert!(!is_retryable(&status_error(StatusCode::UNAUTHORIZED, "", None)));
        assert!(!is_retryable(&Error::InvalidInput("x".to_owned())));
    }

    #[test]
    fn transport_error_is_classified() {
        assert_eq!(kind_of(503), ProviderErrorKind::ServerError);
    }

    #[test]
    fn exhausted_quota_is_not_retryable_but_a_rate_limit_is() {
        let quota = status_error(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"insufficient_quota","message":"out of quota"}}"#,
            None,
        );
        assert_eq!(quota.provider_kind(), Some(ProviderErrorKind::QuotaExceeded));
        assert!(!is_retryable(&quota));

        let rate = status_error(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"rate_limit_exceeded","message":"slow down"}}"#,
            None,
        );
        assert_eq!(rate.provider_kind(), Some(ProviderErrorKind::RateLimited));
        assert!(is_retryable(&rate));
    }

    fn kind_of(status: u16) -> ProviderErrorKind {
        let error = status_error(StatusCode::from_u16(status).expect("status"), "", None);
        error.provider_kind().expect("provider kind")
    }

    #[test]
    fn client_builds() {
        assert!(HttpClient::new().is_ok());
    }
}
