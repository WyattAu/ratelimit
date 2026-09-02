#![forbid(unsafe_code)]
#![deny(missing_docs)]
//! Rate limiting for Rust.
//!
//! Implements the **GCRA** (Generic Cell Rate Algorithm) with pluggable
//! backends (in-memory via `DashMap`, optional Redis) and optional Tower
//! layer integration.
//!
//! # Quick Start
//!
//! ```no_run
//! use ratelimit::{RateLimiter, Quota, InMemoryBackend};
//!
//! #[tokio::main]
//! async fn main() {
//!     let backend = InMemoryBackend::new();
//!     let limiter = RateLimiter::new(
//!         Quota::per_second(10),
//!         backend,
//!     );
//!
//!     let result = limiter.check("user-123").await;
//!     assert!(result.allowed);
//! }
//! ```

mod backend;
mod error;
mod metrics;
mod quota;

#[cfg(feature = "redis")]
mod redis;

#[cfg(feature = "sqlite")]
mod sqlite;

#[cfg(feature = "sliding-window")]
mod sliding_window;

#[cfg(feature = "tower")]
mod tower_layer;

pub use backend::{InMemoryBackend, RateLimitBackend};

pub use error::RateLimitError;
pub use metrics::RateLimitResult;
pub use quota::Quota;
#[cfg(feature = "sliding-window")]
pub use sliding_window::SlidingWindowBackend;

#[cfg(feature = "redis")]
pub use redis::RedisBackend;

#[cfg(feature = "sqlite")]
pub use sqlite::SqliteBackend;

use std::sync::Arc;

/// Core rate limiter that delegates to a [`RateLimitBackend`].
#[derive(Clone)]
pub struct RateLimiter<B: RateLimitBackend> {
    quota: Quota,
    backend: Arc<B>,
}

impl<B: RateLimitBackend> RateLimiter<B> {
    /// Create a new rate limiter with the given quota and backend.
    pub fn new(quota: Quota, backend: B) -> Self {
        Self {
            quota,
            backend: Arc::new(backend),
        }
    }

    /// Check whether the caller identified by `key` is allowed to proceed.
    pub async fn check(&self, key: &str) -> RateLimitResult {
        self.backend.check(key, &self.quota).await
    }

    /// Synchronous version of [`check`](RateLimiter::check).
    ///
    /// Blocks the current thread until the check completes.
    /// Useful for synchronous frameworks (e.g. Actix, Rocket).
    pub fn check_sync(&self, key: &str) -> RateLimitResult {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("failed to create tokio runtime for sync check");
        rt.block_on(self.check(key))
    }
}

#[cfg(feature = "in-memory")]
pub use keyed::KeyedRateLimiter;

#[cfg(feature = "in-memory")]
mod keyed {
    use dashmap::DashMap;

    use crate::RateLimiter;
    use crate::backend::RateLimitBackend;
    use crate::metrics::RateLimitResult;
    use crate::quota::Quota;

    /// Per-key rate limiter that tracks separate rate limits for each key.
    ///
    /// Each key gets its own [`RateLimiter`] instance with either a
    /// custom quota (set via [`with_quota_for_key`](KeyedRateLimiter::with_quota_for_key))
    /// or the default quota.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use ratelimit::{KeyedRateLimiter, Quota, InMemoryBackend};
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let backend = InMemoryBackend::new();
    ///     let limiter = KeyedRateLimiter::new(Quota::per_second(100), backend);
    ///
    ///     // Tighter limit for login endpoint
    ///     limiter.with_quota_for_key("login", Quota::per_second(10));
    ///
    ///     let result = limiter.check("192.168.1.1").await;
    ///     assert!(result.allowed);
    /// }
    /// ```
    pub struct KeyedRateLimiter<B: RateLimitBackend + Clone> {
        limiters: DashMap<String, RateLimiter<B>>,
        default_quota: Quota,
        backend: B,
    }

    impl<B: RateLimitBackend + Clone> KeyedRateLimiter<B> {
        /// Create a new keyed rate limiter with the given default quota and backend.
        pub fn new(default_quota: Quota, backend: B) -> Self {
            Self {
                limiters: DashMap::new(),
                default_quota,
                backend,
            }
        }

        /// Override the rate limit quota for a specific key.
        ///
        /// This replaces any existing limiter for the key. If the key
        /// has not been seen yet, the next call to [`check`](KeyedRateLimiter::check)
        /// will use this quota instead of the default.
        pub fn with_quota_for_key(&self, key: &str, quota: Quota) {
            self.limiters.insert(
                key.to_string(),
                RateLimiter::new(quota, self.backend.clone()),
            );
        }

        /// Check whether the caller identified by `key` is allowed to proceed.
        pub async fn check(&self, key: &str) -> RateLimitResult {
            let limiter = self
                .limiters
                .entry(key.to_string())
                .or_insert_with(|| {
                    RateLimiter::new(self.default_quota.clone(), self.backend.clone())
                })
                .value()
                .clone();
            limiter.check(key).await
        }

        /// Synchronous version of [`check`](KeyedRateLimiter::check).
        ///
        /// Blocks the current thread until the check completes.
        /// Useful for synchronous frameworks (e.g. Actix, Rocket).
        pub fn check_sync(&self, key: &str) -> RateLimitResult {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("failed to create tokio runtime for sync check");
            rt.block_on(self.check(key))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn quota_per_second() {
        let q = Quota::per_second(10);
        assert_eq!(q.burst, 10);
        assert_eq!(q.interval(), Duration::from_millis(100));
    }

    #[test]
    fn quota_per_minute() {
        let q = Quota::per_minute(100);
        assert_eq!(q.burst, 100);
        assert_eq!(q.interval(), Duration::from_millis(600));
    }

    #[test]
    fn quota_per_hour() {
        let q = Quota::per_hour(3600);
        assert_eq!(q.burst, 3600);
        assert_eq!(q.interval(), Duration::from_secs(1));
    }

    #[test]
    fn quota_allow_burst() {
        let q = Quota::per_second(10).allow_burst(20);
        assert_eq!(q.burst, 20);
    }

    #[test]
    fn rate_limit_result_fields() {
        let r = RateLimitResult {
            allowed: true,
            remaining: 5,
            reset_at: std::time::Instant::now(),
            limit: 10,
            retry_after: None,
        };
        assert!(r.allowed);
        assert_eq!(r.remaining, 5);
        assert_eq!(r.limit, 10);
        assert!(r.retry_after.is_none());
    }

    #[test]
    fn rate_limit_result_headers() {
        let r = RateLimitResult {
            allowed: true,
            remaining: 7,
            reset_at: std::time::Instant::now() + Duration::from_secs(30),
            limit: 10,
            retry_after: None,
        };
        let headers = r.headers();
        assert_eq!(headers.len(), 3);
        assert_eq!(headers[0].0, "X-RateLimit-Limit");
        assert_eq!(headers[0].1, "10");
        assert_eq!(headers[1].0, "X-RateLimit-Remaining");
        assert_eq!(headers[1].1, "7");
        assert_eq!(headers[2].0, "X-RateLimit-Reset");
    }

    #[test]
    fn rate_limit_result_retry_after() {
        let r = RateLimitResult {
            allowed: false,
            remaining: 0,
            reset_at: std::time::Instant::now(),
            limit: 10,
            retry_after: Some(Duration::from_millis(100)),
        };
        assert!(!r.allowed);
        assert_eq!(r.retry_after, Some(Duration::from_millis(100)));
    }

    #[test]
    fn in_memory_backend_new() {
        let _ = InMemoryBackend::new();
    }

    #[test]
    fn in_memory_backend_default() {
        let _ = InMemoryBackend::default();
    }

    #[test]
    fn rate_limit_error_display() {
        let e = RateLimitError::RateLimited;
        assert_eq!(e.to_string(), "rate limit exceeded");

        let e = RateLimitError::BackendError("connection refused".into());
        assert_eq!(
            e.to_string(),
            "rate limit backend error: connection refused"
        );
    }

    #[tokio::test]
    async fn first_request_is_allowed() {
        let backend = InMemoryBackend::new();
        let limiter = RateLimiter::new(Quota::per_second(10), backend);
        let result = limiter.check("user-1").await;
        assert!(result.allowed);
        assert_eq!(result.remaining, 9);
        assert_eq!(result.limit, 10);
        assert!(result.retry_after.is_none());
    }

    #[tokio::test]
    async fn rate_limit_exhaustion() {
        let backend = InMemoryBackend::new();
        let limiter = RateLimiter::new(Quota::per_second(2), backend);
        let r1 = limiter.check("user-1").await;
        assert!(r1.allowed);
        let r2 = limiter.check("user-1").await;
        assert!(r2.allowed);
        let r3 = limiter.check("user-1").await;
        assert!(!r3.allowed);
        assert!(r3.retry_after.is_some());
    }

    #[tokio::test]
    async fn keyed_limiter_default_quota() {
        use crate::KeyedRateLimiter;
        let backend = InMemoryBackend::new();
        let limiter = KeyedRateLimiter::new(Quota::per_second(5), backend);

        let r = limiter.check("client-1").await;
        assert!(r.allowed);
        assert_eq!(r.limit, 5);
    }

    #[tokio::test]
    async fn keyed_limiter_custom_quota_per_key() {
        use crate::KeyedRateLimiter;
        let backend = InMemoryBackend::new();
        let limiter = KeyedRateLimiter::new(Quota::per_second(100), backend);

        // Tighter limit for login
        limiter.with_quota_for_key("login", Quota::per_second(2));

        let r_login = limiter.check("login").await;
        assert!(r_login.allowed);
        assert_eq!(r_login.limit, 2);

        let r_api = limiter.check("api").await;
        assert!(r_api.allowed);
        assert_eq!(r_api.limit, 100);
    }

    #[tokio::test]
    async fn keyed_limiter_isolation() {
        use crate::KeyedRateLimiter;
        let backend = InMemoryBackend::new();
        let limiter = KeyedRateLimiter::new(Quota::per_second(1), backend);

        let r1 = limiter.check("a").await;
        assert!(r1.allowed);

        // "a" is exhausted but "b" should be fine
        let r1_again = limiter.check("a").await;
        assert!(!r1_again.allowed);

        let r2 = limiter.check("b").await;
        assert!(r2.allowed);
    }

    #[test]
    fn keyed_limiter_sync_check() {
        use crate::KeyedRateLimiter;
        let backend = InMemoryBackend::new();
        let limiter = KeyedRateLimiter::new(Quota::per_second(10), backend);

        let r = limiter.check_sync("client-1");
        assert!(r.allowed);
        assert_eq!(r.limit, 10);
    }

    #[tokio::test]
    async fn keyed_limiter_headers() {
        use crate::KeyedRateLimiter;
        let backend = InMemoryBackend::new();
        let limiter = KeyedRateLimiter::new(Quota::per_second(10), backend);

        let r = limiter.check("client-1").await;
        let headers = r.headers();
        assert!(headers.iter().any(|(k, _)| *k == "X-RateLimit-Limit"));
        assert!(
            headers
                .iter()
                .any(|(k, v)| *k == "X-RateLimit-Remaining" && v == "9")
        );
    }

    #[test]
    fn sync_check_basic() {
        let backend = InMemoryBackend::new();
        let limiter = RateLimiter::new(Quota::per_second(10), backend);
        let r = limiter.check_sync("user-1");
        assert!(r.allowed);
    }
}
