use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::error::Error;
use crate::Result;

/// Byte budget. Rate `0` is unlimited. The bucket starts empty so a short file still waits.
pub struct TokenBucket {
    rate: u64,
    capacity: f64,
    tokens: f64,
    /// When the bucket next has the tokens recorded here. May be in the future.
    updated: Instant,
}

impl TokenBucket {
    pub fn new(rate_bps: u64) -> Self {
        let capacity = if rate_bps == 0 { 0.0 } else { rate_bps as f64 };
        Self { rate: rate_bps, capacity, tokens: 0.0, updated: Instant::now() }
    }

    /// How long the caller must wait. The wait pays only the missing tokens.
    pub fn consume(&mut self, nbytes: u64) -> Duration {
        if self.rate == 0 || nbytes == 0 {
            return Duration::ZERO;
        }
        let now = Instant::now();
        if self.updated <= now {
            let elapsed = now.saturating_duration_since(self.updated).as_secs_f64();
            self.tokens = (self.tokens + elapsed * self.rate as f64).min(self.capacity);
            self.updated = now;
        }
        let need = nbytes as f64;
        if self.tokens >= need {
            self.tokens -= need;
            return Duration::ZERO;
        }
        let deficit = need - self.tokens;
        let wait = Duration::from_secs_f64(deficit / self.rate as f64);
        self.tokens = 0.0;
        // Stack on top of debt that has not elapsed yet.
        let start = if self.updated > now { self.updated } else { now };
        self.updated = start + wait;
        self.updated.saturating_duration_since(now)
    }
}

/// Process-wide bucket plus the per-download bucket. The caller sleeps the longer wait.
#[derive(Clone)]
pub struct Pace {
    pub global: Arc<Mutex<TokenBucket>>,
    pub local: Arc<Mutex<TokenBucket>>,
}

impl Pace {
    pub fn new(global: Arc<Mutex<TokenBucket>>, local_rate: u64) -> Self {
        Self { global, local: Arc::new(Mutex::new(TokenBucket::new(local_rate))) }
    }

    pub async fn acquire(&self, nbytes: u64, cancel: &CancellationToken) -> Result<()> {
        if nbytes == 0 {
            return Ok(());
        }
        let global_wait = lock(&self.global).consume(nbytes);
        let local_wait = lock(&self.local).consume(nbytes);
        let wait = global_wait.max(local_wait);
        if wait.is_zero() {
            return Ok(());
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::Cancelled),
            _ = tokio::time::sleep(wait) => Ok(()),
        }
    }
}

fn lock(bucket: &Mutex<TokenBucket>) -> std::sync::MutexGuard<'_, TokenBucket> {
    bucket.lock().unwrap_or_else(|err| err.into_inner())
}

/// `0` and `None` do not participate. Two limits become the smaller one.
pub fn tighter(left: Option<u64>, right: Option<u64>) -> u64 {
    match (positive(left), positive(right)) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => 0,
    }
}

fn positive(value: Option<u64>) -> Option<u64> {
    value.filter(|rate| *rate > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_does_not_wait() {
        let mut bucket = TokenBucket::new(0);
        assert_eq!(bucket.consume(1_000_000), Duration::ZERO);
    }

    #[test]
    fn deficit_stacks_instead_of_being_erased() {
        let mut bucket = TokenBucket::new(1_000);
        let first = bucket.consume(1_000);
        let second = bucket.consume(1_000);
        assert!(first >= Duration::from_millis(900), "{first:?}");
        assert!(second >= Duration::from_millis(1_900), "{second:?}");
        assert!(second < Duration::from_millis(2_200), "{second:?}");
    }

    #[test]
    fn tighter_ignores_zero() {
        assert_eq!(tighter(Some(0), Some(50)), 50);
        assert_eq!(tighter(None, None), 0);
        assert_eq!(tighter(Some(80), Some(50)), 50);
    }
}
