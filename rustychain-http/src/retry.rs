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

/// Exponential backoff with a hard cap, honoring a server-provided
/// `Retry-After` when present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total number of retries attempted after the first failure.
    pub max_retries: u32,
    pub base: Duration,
    pub max: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base: Duration::from_millis(500),
            max: Duration::from_secs(8),
        }
    }
}

impl RetryPolicy {
    #[must_use]
    pub fn new(max_retries: u32, base: Duration, max: Duration) -> Self {
        Self {
            max_retries,
            base,
            max,
        }
    }

    /// The delay before the retry that follows `retries` completed retries, or
    /// `None` once the policy is exhausted. A `Retry-After` value always wins,
    /// capped at [`RetryPolicy::max`].
    #[must_use]
    pub fn delay(&self, retries: u32, retry_after: Option<Duration>) -> Option<Duration> {
        if retries >= self.max_retries {
            return None;
        }
        if let Some(retry_after) = retry_after {
            return Some(retry_after.min(self.max));
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
        let policy = RetryPolicy::new(3, Duration::from_millis(100), Duration::from_millis(250));
        assert_eq!(policy.delay(0, None), Some(Duration::from_millis(100)));
        assert_eq!(policy.delay(1, None), Some(Duration::from_millis(200)));
        assert_eq!(policy.delay(2, None), Some(Duration::from_millis(250)));
        assert_eq!(policy.delay(3, None), None);
    }

    #[test]
    fn retry_after_overrides_backoff_but_stays_capped() {
        let policy = RetryPolicy::new(3, Duration::from_millis(100), Duration::from_secs(1));
        assert_eq!(policy.delay(0, Some(Duration::from_secs(5))), Some(Duration::from_secs(1)));
        assert_eq!(policy.delay(0, Some(Duration::from_millis(20))), Some(Duration::from_millis(20)));
    }
}
