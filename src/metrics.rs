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
    /// Requests left in the burst budget, token-bucket style.
    ///
    /// GCRA's `remaining` counts *conforming requests visible right now*
    /// (floor of available time-budget in emission intervals), which is `1`
    /// on a fresh key after its first request. Consumers porting from
    /// fixed-window or token-bucket limiters — and clients reading
    /// `X-RateLimit-Remaining` — expect the burst-consumed view instead:
    /// `limit - consumed`, i.e. `burst - 1` after the first request of a
    /// full burst.
    ///
    /// `limit - remaining` is exactly that value: `remaining` is the unused
    /// time-budget fraction, so the difference is the consumed burst share.
    pub fn remaining_burst(&self) -> u64 {
        self.limit.saturating_sub(self.remaining)
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_burst_matches_burst_consumed_view() {
        let r = RateLimitResult {
            allowed: true,
            remaining: 1,
            reset_at: Instant::now(),
            limit: 60,
            retry_after: None,
        };
        // Fresh key, first request of a 60-burst: 59 requests left.
        assert_eq!(r.remaining_burst(), 59);

        // Denied request reports zero visible budget → full burst consumed.
        let denied = RateLimitResult {
            allowed: false,
            remaining: 0,
            reset_at: Instant::now(),
            limit: 60,
            retry_after: Some(Duration::from_secs(30)),
        };
        assert_eq!(denied.remaining_burst(), 60);
        assert_eq!(denied.remaining, 0);
    }
}
