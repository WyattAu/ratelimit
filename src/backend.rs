use std::time::{Duration, Instant};

use crate::metrics::RateLimitResult;
use crate::quota::Quota;

/// Backend trait for rate limit state storage.
#[async_trait::async_trait]
pub trait RateLimitBackend: Send + Sync + 'static {
    /// Check if a request for `key` is allowed under the given `quota`.
    async fn check(&self, key: &str, quota: &Quota) -> RateLimitResult;
}

/// In-memory backend backed by a `DashMap`.
///
/// Suitable for single-node deployments. Each key tracks its last-allowed
/// timestamp and a token-bucket counter.
pub struct InMemoryBackend {
    entries: dashmap::DashMap<String, Entry>,
}

struct Entry {
    last_check: Instant,
    remaining: u32,
}

impl InMemoryBackend {
    pub fn new() -> Self {
        Self {
            entries: dashmap::DashMap::new(),
        }
    }
}

impl Default for InMemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl RateLimitBackend for InMemoryBackend {
    async fn check(&self, key: &str, quota: &Quota) -> RateLimitResult {
        let interval = quota.interval();
        let burst = quota.burst;

        let mut entry = self.entries.entry(key.to_string()).or_insert_with(|| Entry {
            last_check: Instant::now(),
            remaining: burst,
        });

        let now = Instant::now();
        let elapsed = now.duration_since(entry.last_check);

        // GCRA: compute how many tokens have been replenished.
        let replenished = (elapsed.as_nanos() as u64 / interval.as_nanos() as u64) as u32;
        if replenished > 0 {
            entry.remaining = (entry.remaining + replenished).min(burst);
            let consumption =
                Duration::from_nanos(replenished as u64 * interval.as_nanos() as u64);
            entry.last_check += consumption;
        }

        let allowed = entry.remaining > 0;
        if allowed {
            entry.remaining -= 1;
        }

        let remaining = entry.remaining;
        let reset_at = entry.last_check + interval;

        RateLimitResult {
            allowed,
            remaining,
            reset_at,
        }
    }
}
