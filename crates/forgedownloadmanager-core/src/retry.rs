use std::time::Duration;

use crate::error::Error;

/// Attempt 0 is the first try. `max_retries` is how many extra attempts follow it.
pub fn is_retryable(err: &Error) -> bool {
    match err {
        // Disk and socket failures share `Error::Io`. A full disk retries a few times and then stops.
        Error::Incomplete | Error::HttpClient(_) | Error::Io(_) | Error::RateLimited { .. } => true,
        Error::Http { status, .. } => matches!(*status, 429 | 500 | 502 | 503 | 504),
        // 401, 403, 410, remote change, checksum, cancel, and "server ignored range" are final.
        _ => false,
    }
}

pub fn retry_after_ms(err: &Error) -> Option<u64> {
    match err {
        Error::RateLimited { retry_after_ms } => *retry_after_ms,
        _ => None,
    }
}

/// Delta-seconds become milliseconds. An HTTP-date is not an integer, so the caller uses jitter.
pub fn parse_retry_after(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok().map(|seconds| seconds.saturating_mul(1000))
}

/// Full jitter. Cap is `min(200ms * 2^attempt, 30s)`. A `Retry-After` value replaces the jitter.
pub fn backoff_delay(attempt: u32, retry_after_ms: Option<u64>) -> Duration {
    if let Some(ms) = retry_after_ms {
        return Duration::from_millis(ms);
    }
    let shift = attempt.min(8);
    let cap = 200u64.saturating_mul(1u64 << shift).min(30_000);
    Duration::from_millis(jitter_unit() % (cap + 1))
}

fn jitter_unit() -> u64 {
    let mut buf = [0u8; 8];
    let _ = getrandom::fill(&mut buf);
    u64::from_le_bytes(buf)
}

/// Run `once` until it succeeds, the error is final, or `max_retries` extra attempts are spent.
pub async fn retry_loop<T, F, Fut>(
    max_retries: u32,
    cancel: &tokio_util::sync::CancellationToken,
    mut on_retry: impl FnMut(u32),
    mut once: F,
) -> crate::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = crate::Result<T>>,
{
    let mut attempt = 0u32;
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match once().await {
            Ok(value) => return Ok(value),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(err) if !is_retryable(&err) || attempt >= max_retries => return Err(err),
            Err(err) => {
                let wait = backoff_delay(attempt, retry_after_ms(&err));
                attempt += 1;
                on_retry(attempt);
                if !wait.is_zero() {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Err(Error::Cancelled),
                        _ = tokio::time::sleep(wait) => {}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_integer_is_milliseconds() {
        assert_eq!(parse_retry_after("0"), Some(0));
        assert_eq!(parse_retry_after(" 2 "), Some(2_000));
        assert!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT").is_none());
    }

    #[test]
    fn link_expired_is_not_retried() {
        assert!(!is_retryable(&Error::LinkExpired));
        assert!(!is_retryable(&Error::AuthRequired));
        assert!(!is_retryable(&Error::RemoteChanged));
        assert!(!is_retryable(&Error::Failed("server ignored the range request".into())));
        assert!(is_retryable(&Error::Incomplete));
        assert!(is_retryable(&Error::Http { status: 503, message: "x".into() }));
        assert!(is_retryable(&Error::RateLimited { retry_after_ms: Some(0) }));
        assert!(!is_retryable(&Error::Http { status: 404, message: "x".into() }));
    }

    #[test]
    fn retry_after_replaces_jitter() {
        assert_eq!(backoff_delay(3, Some(0)), Duration::ZERO);
        assert_eq!(backoff_delay(0, Some(1500)), Duration::from_millis(1500));
    }

    #[test]
    fn jitter_stays_inside_the_cap() {
        let delay = backoff_delay(0, None);
        assert!(delay <= Duration::from_millis(200));
        let later = backoff_delay(8, None);
        assert!(later <= Duration::from_secs(30));
    }
}
