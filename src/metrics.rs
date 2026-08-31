use std::time::{Duration, Instant};

/// Result of a rate limit check.
#[derive(Debug, Clone)]
pub struct RateLimitResult {
    /// Whether the request was allowed.
    pub allowed: bool,
    /// Remaining requests in the current window.
    pub remaining: u64,
    /// When the next token becomes available.
    pub reset_at: Instant,
    /// The total rate limit for this window.
    pub limit: u64,
    /// When to retry if the request was rejected.
    pub retry_after: Option<Duration>,
}

impl RateLimitResult {
    /// Build standard rate limit HTTP headers.
    ///
    /// Returns `(header_name, header_value)` pairs for:
    /// - `X-RateLimit-Limit`
    /// - `X-RateLimit-Remaining`
    /// - `X-RateLimit-Reset`
    pub fn headers(&self) -> Vec<(&str, String)> {
        vec![
            ("X-RateLimit-Limit", self.limit.to_string()),
            ("X-RateLimit-Remaining", self.remaining.to_string()),
            (
                "X-RateLimit-Reset",
                self.reset_at
                    .duration_since(Instant::now())
                    .as_secs()
                    .to_string(),
            ),
        ]
    }
}
