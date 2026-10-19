use std::pin::Pin;
use std::time::Duration;

use futures_core::Stream;
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, RETRY_AFTER};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde_json::Value;

use norte_core::{Error, ProviderError, ProviderErrorKind, Result};

use crate::sse::{SseDecoder, SseEvent};
use crate::transport::{TransportPolicy, parse_retry_after};
use crate::vendor::VendorProfile;

/// A stream of decoded `SSE` events.
pub type SseStream = Pin<Box<dyn Stream<Item = Result<SseEvent>> + Send + 'static>>;

/// Names the stage a transport failure happened in, so a retry can tell "never
/// reached the provider" from "may already be running".
const STAGE: &str = "transport";

/// The shared transport. It owns the [`TransportPolicy`], auth application and
/// vendor headers, so adapters only deal with their own wire mapping.
#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    policy: TransportPolicy,
}

impl HttpClient {
    /// # Errors
    ///
    /// Returns [`Error::Provider`] when the underlying `HTTP` client cannot be
    /// built.
    pub fn new() -> Result<Self> {
        Self::with_policy(TransportPolicy::default())
    }

    /// # Errors
    ///
    /// Returns [`Error::Provider`] when the underlying `HTTP` client cannot be
    /// built.
    pub fn with_policy(policy: TransportPolicy) -> Result<Self> {
        let inner = reqwest::Client::builder()
            .connect_timeout(policy.connect_timeout)
            .build()
            .map_err(|error| transport(&error))?;
        Ok(Self { inner, policy })
    }

    /// The policy this transport follows.
    #[must_use]
    pub fn policy(&self) -> TransportPolicy {
        self.policy
    }

    /// The underlying `reqwest` client, for a request the ad-hoc API does not
    /// cover.
    #[must_use]
    pub fn raw(&self) -> &reqwest::Client {
        &self.inner
    }

    /// Sends a `JSON` `POST` and decodes the `JSON` response body.
    ///
    /// The call is bounded by the policy's `request_timeout`, and only a
    /// connection that never reached the provider is retried. A provider error
    /// — a rate limit, a quota, a `5xx` — is returned as is, `Retry-After`
    /// included, for the host to act on.
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
        self.send_json(profile, Method::POST, path, Some(body))
            .await
    }

    /// Sends a `JSON` request and decodes the `JSON` response body.
    ///
    /// The call is bounded by the policy's `request_timeout`, and only a
    /// connection that never reached the provider is retried. A provider error
    /// — a rate limit, a quota, a `5xx` — is returned as is, `Retry-After`
    /// included, for the host to act on.
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
                .send_once(
                    profile,
                    method.clone(),
                    path,
                    body,
                    "application/json",
                    self.policy.request_timeout,
                )
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

    /// Opens an `SSE` stream, retrying a connection that never reached the
    /// provider.
    ///
    /// Unlike a `JSON` call the stream is not bounded as a whole — a long answer
    /// is normal — but the policy's `stream_idle_timeout` fails it when the
    /// provider stops sending mid-answer.
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
                .send_once(
                    profile,
                    Method::POST,
                    path,
                    Some(body),
                    "text/event-stream",
                    None,
                )
                .await;
            match sent {
                Ok(response) => {
                    return Ok(guard_idle(
                        decode_sse(response),
                        self.policy.stream_idle_timeout,
                    ));
                }
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

    /// The delay before retrying `error`, or `None` when the transport does not
    /// retry it or the policy is exhausted.
    fn backoff(&self, retries: u32, error: &Error) -> Option<Duration> {
        if !never_reached_provider(error) {
            return None;
        }
        self.policy.delay(retries)
    }

    fn request(
        &self,
        profile: &VendorProfile,
        method: Method,
        path: &str,
    ) -> Result<RequestBuilder> {
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
        timeout: Option<Duration>,
    ) -> Result<Response> {
        let builder = self.request(profile, method, path)?.header(ACCEPT, accept);
        let builder = match body {
            Some(body) => builder.json(body),
            None => builder,
        };
        let builder = match timeout {
            Some(timeout) => builder.timeout(timeout),
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

/// Fails the stream with a transport error when the provider stays silent for
/// longer than `idle`, so a stalled answer cannot hang a turn forever.
fn guard_idle(stream: SseStream, idle: Option<Duration>) -> SseStream {
    let Some(idle) = idle else {
        return stream;
    };
    Box::pin(futures_util::stream::unfold(
        Some(stream),
        move |state| async move {
            let mut stream = state?;
            match tokio::time::timeout(idle, stream.next()).await {
                Ok(Some(item)) => Some((item, Some(stream))),
                Ok(None) => None,
                Err(_elapsed) => Some((Err(stalled(idle)), None)),
            }
        },
    ))
}

/// The error for a provider that went silent mid-stream. It is a transport
/// failure the transport does not retry, because the provider may still be
/// working on the request.
fn stalled(idle: Duration) -> Error {
    Error::provider(
        ProviderError::new(
            ProviderErrorKind::Transport,
            format!("no event from the provider for {idle:?}"),
        )
        .with_extension(STAGE, Value::from("timeout")),
    )
}

/// Whether a failure may clear on its own, so a host may retry the call: a rate
/// limit, an overloaded model, a server error, or a transport failure.
///
/// This is the host's helper, not the transport's rule. The transport itself
/// retries only a connection that never reached the provider (see
/// [`TransportPolicy`]), since a retry after the provider started working can
/// make it work twice. A host that retries these keeps the whole turn: a failed
/// turn leaves the session resumable, so `Engine::resume` picks it up where it
/// stopped — including any [`retry_after`](Error::retry_after) the provider
/// asked for.
#[must_use]
pub fn is_transient(error: &Error) -> bool {
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

/// Whether the request failed before reaching the provider, so retrying it
/// cannot make the provider work twice.
fn never_reached_provider(error: &Error) -> bool {
    matches!(
        error,
        Error::Provider(provider)
            if provider.kind == ProviderErrorKind::Transport
                && provider.extensions.get(STAGE).and_then(Value::as_str) == Some("connect")
    )
}

fn transport(error: &reqwest::Error) -> Error {
    let (kind, stage) = if error.is_connect() {
        (ProviderErrorKind::Transport, "connect")
    } else if error.is_timeout() {
        (ProviderErrorKind::Transport, "timeout")
    } else if error.is_decode() {
        (ProviderErrorKind::Decode, "decode")
    } else {
        (ProviderErrorKind::Transport, "stream")
    };
    Error::provider(
        ProviderError::new(kind, error.to_string()).with_extension(STAGE, Value::from(stage)),
    )
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
        assert_eq!(kind_of(503), ProviderErrorKind::ServerError);
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
    fn transient_kinds_are_the_ones_a_host_may_retry() {
        assert!(is_transient(&status_error(
            StatusCode::TOO_MANY_REQUESTS,
            "",
            None
        )));
        assert!(is_transient(&status_error(
            StatusCode::BAD_GATEWAY,
            "",
            None
        )));
        assert!(!is_transient(&status_error(
            StatusCode::UNAUTHORIZED,
            "",
            None
        )));
        assert!(!is_transient(&Error::InvalidInput("x".to_owned())));
    }

    #[test]
    fn exhausted_quota_is_not_transient_but_a_rate_limit_is() {
        let quota = status_error(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"insufficient_quota","message":"out of quota"}}"#,
            None,
        );
        assert_eq!(
            quota.provider_kind(),
            Some(ProviderErrorKind::QuotaExceeded)
        );
        assert!(!is_transient(&quota));

        let rate = status_error(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"rate_limit_exceeded","message":"slow down"}}"#,
            None,
        );
        assert_eq!(rate.provider_kind(), Some(ProviderErrorKind::RateLimited));
        assert!(is_transient(&rate));
    }

    #[test]
    fn only_a_connection_that_never_reached_the_provider_is_retried() {
        let client = HttpClient::new().expect("client");

        let connect = transport_error("connect");
        assert!(never_reached_provider(&connect));
        assert_eq!(
            client.backoff(0, &connect),
            Some(Duration::from_millis(500))
        );
        assert_eq!(client.backoff(3, &connect), None, "the policy is exhausted");

        let timeout = transport_error("timeout");
        assert!(!never_reached_provider(&timeout));
        assert_eq!(
            client.backoff(0, &timeout),
            None,
            "the request may be running"
        );

        let rate_limit = status_error(StatusCode::TOO_MANY_REQUESTS, "", None);
        assert_eq!(client.backoff(0, &rate_limit), None, "the host owns it");
    }

    #[test]
    fn a_policy_without_retries_never_backs_off() {
        let client = HttpClient::with_policy(TransportPolicy::without_retries()).expect("client");
        assert_eq!(client.backoff(0, &transport_error("connect")), None);
        assert_eq!(
            client.policy().stream_idle_timeout,
            TransportPolicy::default().stream_idle_timeout,
            "no retries is not no timeouts"
        );
    }

    #[tokio::test]
    async fn a_silent_stream_fails_instead_of_hanging() {
        let stream: SseStream = Box::pin(futures_util::stream::pending());
        let mut guarded = guard_idle(stream, Some(Duration::from_millis(10)));

        let error = guarded
            .next()
            .await
            .expect("an item")
            .expect_err("the idle timeout fires");
        assert_eq!(error.provider_kind(), Some(ProviderErrorKind::Transport));
        assert!(!never_reached_provider(&error), "a stall is not retried");
        assert!(guarded.next().await.is_none(), "and the stream ends there");
    }

    #[tokio::test]
    async fn a_stream_that_keeps_sending_is_left_alone() {
        let event = SseEvent {
            event: Some("message".to_owned()),
            data: "{}".to_owned(),
            ..SseEvent::default()
        };
        let stream: SseStream = Box::pin(futures_util::stream::iter([Ok(event.clone())]));
        let mut guarded = guard_idle(stream, Some(Duration::from_millis(10)));

        let received = guarded.next().await.expect("an item").expect("no failure");
        assert_eq!(received, event);
        assert!(guarded.next().await.is_none());
    }

    fn transport_error(stage: &str) -> Error {
        Error::provider(
            ProviderError::new(ProviderErrorKind::Transport, "boom")
                .with_extension(STAGE, Value::from(stage)),
        )
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
