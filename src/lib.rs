#![forbid(unsafe_code)]
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

pub use backend::{InMemoryBackend, RateLimitBackend};
pub use error::RateLimitError;
pub use metrics::RateLimitResult;
pub use quota::Quota;

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
}
