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

#[async_trait::async_trait]
impl RateLimitBackend for SlidingWindowBackend {
    async fn check(&self, key: &str, quota: &Quota) -> RateLimitResult {
        let window = quota.interval() * quota.burst;
        let limit = quota.burst as u64;

        let mut entry = self
            .entries
            .entry(key.to_string())
            .or_insert_with(VecDeque::new);

        let now = Instant::now();
        let window_start = now.checked_sub(window).unwrap_or(now);

        while entry.front().map_or(false, |&ts| ts < window_start) {
            entry.pop_front();
        }

        let count = entry.len() as u64;
        let allowed = count < limit;

        if allowed {
            entry.push_back(now);
        }

        let remaining = if allowed { limit - count - 1 } else { 0 };

        let reset_at = entry
            .front()
            .map(|&ts| ts + window)
            .unwrap_or(now + window);

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
}
