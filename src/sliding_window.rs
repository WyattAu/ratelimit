use std::collections::VecDeque;
use std::time::Instant;

use crate::backend::RateLimitBackend;
use crate::metrics::RateLimitResult;
use crate::quota::Quota;

/// In-memory backend using a sliding window algorithm.
///
/// Tracks request timestamps in a deque and counts requests within
/// the current time window. Rejects requests when the count exceeds
/// the quota's burst limit.
#[derive(Clone)]
pub struct SlidingWindowBackend {
    entries: dashmap::DashMap<String, VecDeque<Instant>>,
}

impl SlidingWindowBackend {
    /// Create a new sliding window backend.
    pub fn new() -> Self {
        Self {
            entries: dashmap::DashMap::new(),
        }
    }
}

impl Default for SlidingWindowBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimitBackend for SlidingWindowBackend {
    async fn check(&self, key: &str, quota: &Quota) -> RateLimitResult {
        let window = quota.interval() * quota.burst;
        let limit = quota.burst as u64;

        let mut entry = self.entries.entry(key.to_string()).or_default();

        let now = Instant::now();
        let window_start = now.checked_sub(window).unwrap_or(now);

        while entry.front().is_some_and(|&ts| ts < window_start) {
            entry.pop_front();
        }

        let count = entry.len() as u64;
        let allowed = count < limit;

        if allowed {
            entry.push_back(now);
        }

        let remaining = if allowed { limit - count - 1 } else { 0 };

        let reset_at = entry.front().map(|&ts| ts + window).unwrap_or(now + window);

        let retry_after = if allowed {
            None
        } else {
            entry.front().map(|&ts| ts + window - now)
        };

        RateLimitResult {
            allowed,
            remaining,
            reset_at,
            limit,
            retry_after,
        }
    }
}

// Tests exercise failure paths and invariants directly; unwrap/expect,
// slicing, and panicking asserts are acceptable here — violations
// surface as test failures, not production panics.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn sliding_window_allows_requests_within_limit() {
        let backend = SlidingWindowBackend::new();
        let quota = Quota::per_second(3);

        let r1 = backend.check("key", &quota).await;
        assert!(r1.allowed);
        assert_eq!(r1.remaining, 2);

        let r2 = backend.check("key", &quota).await;
        assert!(r2.allowed);
        assert_eq!(r2.remaining, 1);

        let r3 = backend.check("key", &quota).await;
        assert!(r3.allowed);
        assert_eq!(r3.remaining, 0);
    }

    #[tokio::test]
    async fn sliding_window_rejects_when_exceeded() {
        let backend = SlidingWindowBackend::new();
        let quota = Quota::per_second(2);

        let r1 = backend.check("key", &quota).await;
        assert!(r1.allowed);

        let r2 = backend.check("key", &quota).await;
        assert!(r2.allowed);

        let r3 = backend.check("key", &quota).await;
        assert!(!r3.allowed);
        assert!(r3.retry_after.is_some());
        assert_eq!(r3.remaining, 0);
    }

    #[tokio::test]
    async fn sliding_window_keys_are_isolated() {
        let backend = SlidingWindowBackend::new();
        let quota = Quota::per_second(1);

        let r1 = backend.check("a", &quota).await;
        assert!(r1.allowed);

        let r2 = backend.check("a", &quota).await;
        assert!(!r2.allowed);

        let r3 = backend.check("b", &quota).await;
        assert!(r3.allowed);
    }

    #[tokio::test]
    async fn sliding_window_requests_expire() {
        let backend = SlidingWindowBackend::new();
        let quota = Quota::per_second(1);

        let r1 = backend.check("key", &quota).await;
        assert!(r1.allowed);

        let r2 = backend.check("key", &quota).await;
        assert!(!r2.allowed);

        tokio::time::sleep(Duration::from_millis(1100)).await;

        let r3 = backend.check("key", &quota).await;
        assert!(r3.allowed);
    }

    #[test]
    fn sliding_window_new_and_default() {
        let _ = SlidingWindowBackend::new();
        let _ = SlidingWindowBackend::default();
    }

    #[tokio::test]
    async fn sliding_window_resets_only_after_the_full_window() {
        // window = interval * burst = 500 ms * 2 = 1 s for per_second(2).
        // At 600 ms the recorded requests are still inside the true window,
        // so the request must stay denied; a window computed by division
        // (250 ms) would have expired them and re-allowed the request.
        let backend = SlidingWindowBackend::new();
        let quota = Quota::per_second(2);

        assert!(backend.check("w", &quota).await.allowed);
        assert!(backend.check("w", &quota).await.allowed);
        assert!(!backend.check("w", &quota).await.allowed);

        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(
            !backend.check("w", &quota).await.allowed,
            "requests inside the full window must still count"
        );
    }

    #[tokio::test]
    async fn sliding_window_reset_at_is_in_the_future_when_allowed() {
        let backend = SlidingWindowBackend::new();
        let r = backend.check("reset", &Quota::per_second(2)).await;
        assert!(r.allowed);
        assert!(
            r.reset_at > std::time::Instant::now(),
            "reset_at must be in the future for an allowed request"
        );
    }

    #[tokio::test]
    async fn sliding_window_retry_after_is_within_the_window() {
        let backend = SlidingWindowBackend::new();
        let quota = Quota::per_second(1); // window = 1 s

        assert!(backend.check("retry", &quota).await.allowed);
        tokio::time::sleep(Duration::from_millis(5)).await;
        let r2 = backend.check("retry", &quota).await;
        assert!(!r2.allowed);
        let retry = r2.retry_after.expect("denied request carries retry_after");
        assert!(retry > Duration::ZERO, "retry_after must be positive");
        assert!(
            retry <= quota.interval(),
            "retry_after must not exceed the window"
        );
    }
}
