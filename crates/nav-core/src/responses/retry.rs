//! Retry policy for transient transport failures.
//!
//! The Responses API can hand back 429s, 5xx, and connection errors — usually
//! one-off blips that succeed on the next try. This module classifies an error
//! as retryable, computes an exponential-backoff delay with jitter (honoring a
//! `Retry-After` header when the server provides one), and drives the
//! attempt/sleep loop. It deliberately works on a `TransportError` enum rather
//! than `anyhow::Error` so the policy is testable without networking.
//!
//! The retry wraps only the *creation* of a transport stream. Once events have
//! started flowing into the consumer channel, partial deltas have already been
//! emitted to the user and the session log; retrying mid-stream would
//! duplicate output, so mid-stream errors surface as a normal stream error.

use anyhow::anyhow;
use reqwest::StatusCode;
use std::future::Future;
use std::time::{Duration, Instant};
use tokio::time::sleep;

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Maximum number of attempts including the first try. `0` disables retry.
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    /// Fractional jitter. `0.1` ⇒ multiplier sampled in `[0.9, 1.1)`.
    pub jitter: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
            jitter: 0.1,
        }
    }
}

#[derive(Debug)]
pub enum TransportError {
    /// Server returned a non-success status with an optional `Retry-After`.
    Http {
        status: StatusCode,
        retry_after: Option<Duration>,
        body: String,
    },
    /// The transport gave up waiting (connect timeout, idle timeout, etc.).
    Timeout,
    /// TCP / TLS / DNS / socket-level failure surfaced by reqwest or tungstenite.
    Network(String),
    /// Anything else — kept around so callers can convert to `anyhow::Error`.
    Other(anyhow::Error),
}

impl TransportError {
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            TransportError::Http { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Http { status, body, .. } => {
                if body.is_empty() {
                    write!(f, "HTTP {status}")
                } else {
                    write!(f, "HTTP {status}: {body}")
                }
            }
            TransportError::Timeout => write!(f, "transport timed out"),
            TransportError::Network(msg) => write!(f, "network error: {msg}"),
            TransportError::Other(err) => write!(f, "{err:#}"),
        }
    }
}

impl std::error::Error for TransportError {}

// No explicit `From<TransportError> for anyhow::Error` — anyhow's blanket
// `From<E: std::error::Error + Send + Sync + 'static>` already covers it.

impl From<reqwest::Error> for TransportError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            return TransportError::Timeout;
        }
        if let Some(status) = err.status() {
            return TransportError::Http {
                status,
                retry_after: None,
                body: err.to_string(),
            };
        }
        TransportError::Network(err.to_string())
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for TransportError {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        use tokio_tungstenite::tungstenite::Error as WsErr;
        match err {
            WsErr::Http(response) => {
                let status = response.status();
                let body = match response.body() {
                    Some(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                    None => String::new(),
                };
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_retry_after_seconds);
                TransportError::Http {
                    status,
                    retry_after,
                    body,
                }
            }
            WsErr::Io(io_err) => TransportError::Network(io_err.to_string()),
            other => TransportError::Network(other.to_string()),
        }
    }
}

/// Returns true if the error class is worth retrying. Client errors (4xx
/// except 429) are *not* retried — they indicate a bug in the request, not a
/// transient failure.
pub fn should_retry(err: &TransportError) -> bool {
    match err {
        TransportError::Http { status, .. } => {
            status.as_u16() == 429 || status.is_server_error()
        }
        TransportError::Timeout | TransportError::Network(_) => true,
        TransportError::Other(_) => false,
    }
}

impl RetryPolicy {
    /// Compute the sleep duration before the next attempt. `attempt` is
    /// 1-based — `attempt=1` is the delay between the first (failed) try and
    /// the second try. `Retry-After`, when present, replaces the exponential
    /// term but is still capped by `max_delay` so a hostile server can't pin
    /// the client for minutes; jitter does not apply to a server-provided
    /// hint.
    pub fn delay_for(
        &self,
        attempt: u32,
        retry_after: Option<Duration>,
        jitter_fn: impl FnOnce() -> f64,
    ) -> Duration {
        if let Some(hint) = retry_after {
            return hint.min(self.max_delay);
        }
        let exp = attempt.saturating_sub(1).min(20);
        let multiplier = 1u128 << exp;
        let raw_ms = (self.base_delay.as_millis()).saturating_mul(multiplier);
        let raw_ms = raw_ms.min(u128::from(u64::MAX));
        let raw = Duration::from_millis(raw_ms as u64).min(self.max_delay);

        let r = jitter_fn().clamp(0.0, 1.0);
        let mult = 1.0 - self.jitter + 2.0 * self.jitter * r;
        let ms = (raw.as_millis() as f64 * mult) as u64;
        Duration::from_millis(ms)
    }
}

/// Parse a `Retry-After` header value. Only integer-seconds form is
/// supported; HTTP-date form is intentionally ignored (would pull in a date
/// crate for a path we hit on rate limits only).
pub fn parse_retry_after_seconds(value: &str) -> Option<Duration> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Drives the attempt/sleep loop. `on_retry` is invoked before each backoff
/// sleep so callers can surface a `ProviderRetry` event to users.
pub async fn retry<F, Fut, T>(
    policy: &RetryPolicy,
    mut on_retry: impl FnMut(u32, Duration, &TransportError),
    mut f: F,
) -> Result<T, TransportError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, TransportError>>,
{
    let max_attempts = policy.max_attempts.max(1);
    for attempt in 1..=max_attempts {
        match f().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                let last_attempt = attempt == max_attempts;
                if last_attempt || !should_retry(&err) {
                    return Err(err);
                }
                let delay = policy.delay_for(attempt, err.retry_after(), default_jitter);
                on_retry(attempt, delay, &err);
                sleep(delay).await;
            }
        }
    }
    Err(TransportError::Other(anyhow!(
        "retry loop exited without producing a result"
    )))
}

/// Non-cryptographic deterministic-per-call jitter source. Returns a value in
/// `[0, 1)` derived from the current nanosecond clock — good enough to keep
/// concurrent retriers from thundering, not good enough for anything else.
fn default_jitter() -> f64 {
    let nanos = Instant::now().elapsed().subsec_nanos() as u64;
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let mut s = nanos ^ now_ns ^ 0x9E37_79B9_7F4A_7C15u64;
    s ^= s >> 33;
    s = s.wrapping_mul(0xff51_afd7_ed55_8ccd);
    s ^= s >> 33;
    (s as f64) / (u64::MAX as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(status: u16, retry_after: Option<u64>) -> TransportError {
        TransportError::Http {
            status: StatusCode::from_u16(status).unwrap(),
            retry_after: retry_after.map(Duration::from_secs),
            body: String::new(),
        }
    }

    #[test]
    fn should_retry_classifies_errors() {
        assert!(should_retry(&http(429, None)));
        assert!(should_retry(&http(500, None)));
        assert!(should_retry(&http(503, None)));
        assert!(should_retry(&TransportError::Timeout));
        assert!(should_retry(&TransportError::Network("reset".into())));
        assert!(!should_retry(&http(400, None)));
        assert!(!should_retry(&http(401, None)));
        assert!(!should_retry(&http(403, None)));
        assert!(!should_retry(&http(404, None)));
        assert!(!should_retry(&TransportError::Other(anyhow!("nope"))));
    }

    #[test]
    fn delay_for_progresses_exponentially_without_retry_after() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(8),
            jitter: 0.0,
        };
        let d1 = policy.delay_for(1, None, || 0.5);
        let d2 = policy.delay_for(2, None, || 0.5);
        let d3 = policy.delay_for(3, None, || 0.5);
        let d4 = policy.delay_for(4, None, || 0.5);
        assert_eq!(d1, Duration::from_millis(100));
        assert_eq!(d2, Duration::from_millis(200));
        assert_eq!(d3, Duration::from_millis(400));
        assert_eq!(d4, Duration::from_millis(800));
    }

    #[test]
    fn delay_for_caps_at_max_delay() {
        let policy = RetryPolicy {
            max_attempts: 20,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(1),
            jitter: 0.0,
        };
        // Without cap, attempt 10 would be 100ms * 2^9 = 51.2s.
        let d = policy.delay_for(10, None, || 0.5);
        assert!(d <= Duration::from_secs(1));
    }

    #[test]
    fn delay_for_applies_jitter_bounds() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1000),
            max_delay: Duration::from_secs(10),
            jitter: 0.1,
        };
        let lower = policy.delay_for(1, None, || 0.0);
        let upper = policy.delay_for(1, None, || 1.0);
        // jitter=0.1 ⇒ [0.9, 1.1] of base
        assert_eq!(lower, Duration::from_millis(900));
        assert_eq!(upper, Duration::from_millis(1100));
    }

    #[test]
    fn delay_for_honors_retry_after_without_jitter() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(8),
            jitter: 0.5,
        };
        let d = policy.delay_for(1, Some(Duration::from_secs(3)), || 0.0);
        assert_eq!(d, Duration::from_secs(3));
    }

    #[test]
    fn delay_for_caps_retry_after_at_max_delay() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(8),
            jitter: 0.0,
        };
        let d = policy.delay_for(1, Some(Duration::from_secs(60)), || 0.0);
        assert_eq!(d, Duration::from_secs(8));
    }

    #[test]
    fn parse_retry_after_seconds_accepts_integers() {
        assert_eq!(parse_retry_after_seconds("5"), Some(Duration::from_secs(5)));
        assert_eq!(
            parse_retry_after_seconds("  30 "),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn parse_retry_after_seconds_rejects_http_date() {
        // We deliberately don't support HTTP-date — just falls back to backoff.
        assert!(parse_retry_after_seconds("Wed, 21 Oct 2015 07:28:00 GMT").is_none());
    }

    #[tokio::test]
    async fn retry_returns_immediately_on_success() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
            jitter: 0.0,
        };
        let count = std::sync::Mutex::new(0u32);
        let result: Result<u32, TransportError> =
            retry(&policy, |_, _, _| {}, || async {
                *count.lock().unwrap() += 1;
                Ok(42)
            })
            .await;
        assert_eq!(result.unwrap(), 42);
        assert_eq!(*count.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn retry_retries_on_transient_then_succeeds() {
        let policy = RetryPolicy {
            max_attempts: 4,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
            jitter: 0.0,
        };
        let attempts = std::sync::Mutex::new(0u32);
        let observed = std::sync::Mutex::new(Vec::<u32>::new());
        let result: Result<&'static str, TransportError> = retry(
            &policy,
            |attempt, _, _| observed.lock().unwrap().push(attempt),
            || async {
                let mut a = attempts.lock().unwrap();
                *a += 1;
                if *a < 3 {
                    Err(http(503, None))
                } else {
                    Ok("done")
                }
            },
        )
        .await;
        assert_eq!(result.unwrap(), "done");
        assert_eq!(*attempts.lock().unwrap(), 3);
        // on_retry fires twice: after attempts 1 and 2, before sleeping.
        assert_eq!(*observed.lock().unwrap(), vec![1, 2]);
    }

    #[tokio::test]
    async fn retry_gives_up_after_max_attempts() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
            jitter: 0.0,
        };
        let attempts = std::sync::Mutex::new(0u32);
        let result: Result<u32, TransportError> = retry(
            &policy,
            |_, _, _| {},
            || async {
                *attempts.lock().unwrap() += 1;
                Err(http(503, None))
            },
        )
        .await;
        assert!(matches!(result, Err(TransportError::Http { .. })));
        assert_eq!(*attempts.lock().unwrap(), 3);
    }

    #[tokio::test]
    async fn retry_does_not_retry_non_retryable_errors() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
            jitter: 0.0,
        };
        let attempts = std::sync::Mutex::new(0u32);
        let result: Result<u32, TransportError> = retry(
            &policy,
            |_, _, _| {},
            || async {
                *attempts.lock().unwrap() += 1;
                Err(http(400, None))
            },
        )
        .await;
        assert!(matches!(result, Err(TransportError::Http { .. })));
        assert_eq!(*attempts.lock().unwrap(), 1);
    }
}
