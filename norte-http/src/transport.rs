//! The transport's own policy, and the parsing of the `Retry-After` header the
//! host reads on errors.
//!
//! [`TransportPolicy`] is deliberately narrow. The transport retries only a
//! connection that never reached the provider, and it bounds how long it waits
//! for one. Anything that came *from* the provider — a status code, a rate
//! limit, an exhausted quota — is handed to the host with the vendor's
//! `Retry-After` attached, because only the host knows how to react: wait,
//! switch model, shrink its concurrency, or tell the user.

use std::time::Duration;

/// Parses a `Retry-After` header value: either a whole number of seconds or an
/// `HTTP`-date. Dates in the past become a zero delay.
#[must_use]
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let target = parse_http_date(value)?;
    let now = chrono::Utc::now();
    let delta = target - now;
    if delta <= chrono::Duration::zero() {
        Some(Duration::ZERO)
    } else {
        delta.to_std().ok()
    }
}

fn parse_http_date(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc2822(value) {
        return Some(parsed.with_timezone(&chrono::Utc));
    }
    chrono::DateTime::parse_from_str(value, "%a, %d %b %Y %H:%M:%S GMT")
        .ok()
        .map(|parsed| parsed.with_timezone(&chrono::Utc))
}

/// What the `HTTP` transport does by itself: how it retries a connection that
/// never reached the provider, and the timeouts it enforces.
///
/// **Retries.** Only a failure to *establish* a connection is retried: nothing
/// was sent, so the provider cannot have started working, and the retry is
/// invisible. A timeout is not retried even though it is a transport failure,
/// because by then the request may be running and a generation must never run
/// twice. Neither is anything the provider answered with — a rate limit, a
/// quota, a `5xx` — since that is application policy: the error reaches the host
/// with [`retry_after`](norte_core::Error::retry_after) intact.
///
/// **Timeouts.** `connect_timeout` bounds establishing a connection,
/// `request_timeout` bounds a whole `JSON` call, and `stream_idle_timeout`
/// bounds the silence *inside* an event stream: a provider that stops sending
/// mid-answer fails with a transport error instead of hanging the turn forever.
/// A host whose models think for minutes before the first token raises it or
/// turns it off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportPolicy {
    /// Attempts after the first one, for a connection that never reached the
    /// provider. `0` disables retrying and leaves the decision to the host.
    pub max_retries: u32,
    /// Base of the exponential backoff between attempts.
    pub base: Duration,
    /// Cap on one backoff delay.
    pub max: Duration,
    /// Cap on establishing one connection.
    pub connect_timeout: Duration,
    /// Cap on one `JSON` request, response included. `None` waits forever.
    pub request_timeout: Option<Duration>,
    /// Cap on the silence between two events of a stream. `None` waits forever.
    pub stream_idle_timeout: Option<Duration>,
}

impl Default for TransportPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base: Duration::from_millis(500),
            max: Duration::from_secs(8),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Some(Duration::from_secs(120)),
            stream_idle_timeout: Some(Duration::from_secs(300)),
        }
    }
}

impl TransportPolicy {
    /// A transport that never retries anything: for a host that owns its own
    /// retry loop and only wants the timeouts.
    #[must_use]
    pub fn without_retries() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// The delay before the retry that follows `retries` completed retries, or
    /// `None` once the policy is exhausted.
    #[must_use]
    pub fn delay(&self, retries: u32) -> Option<Duration> {
        if retries >= self.max_retries {
            return None;
        }
        let factor = 1u32.checked_shl(retries.min(16)).unwrap_or(u32::MAX);
        Some((self.base * factor).min(self.max))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_accepts_seconds() {
        assert_eq!(parse_retry_after("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after("  2 "), Some(Duration::from_secs(2)));
    }

    #[test]
    fn retry_after_accepts_an_http_date() {
        let parsed = parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT");
        assert_eq!(parsed, Some(Duration::ZERO));
    }

    #[test]
    fn retry_after_rejects_garbage() {
        assert_eq!(parse_retry_after("soon"), None);
    }

    #[test]
    fn backoff_grows_then_caps_and_exhausts() {
        let policy = TransportPolicy {
            max_retries: 3,
            base: Duration::from_millis(100),
            max: Duration::from_millis(250),
            ..TransportPolicy::default()
        };
        assert_eq!(policy.delay(0), Some(Duration::from_millis(100)));
        assert_eq!(policy.delay(1), Some(Duration::from_millis(200)));
        assert_eq!(policy.delay(2), Some(Duration::from_millis(250)));
        assert_eq!(policy.delay(3), None);
    }

    #[test]
    fn a_policy_without_retries_never_waits() {
        let policy = TransportPolicy::without_retries();
        assert_eq!(policy.max_retries, 0);
        assert_eq!(policy.delay(0), None);
        assert!(
            policy.connect_timeout > Duration::ZERO,
            "no retries is not no timeouts"
        );
    }
}
