use std::time::Duration;

/// A rate limit quota expressed in the GCRA style.
#[derive(Debug, Clone)]
pub struct Quota {
    /// Minimum interval between allowed requests.
    interval: Duration,
    /// Maximum burst size.
    pub burst: u32,
}

impl Quota {
    /// Allow `n` requests per second.
    pub fn per_second(n: u32) -> Self {
        Self {
            interval: Duration::from_secs_f64(1.0 / n as f64),
            burst: n,
        }
    }

    /// Allow `n` requests per minute.
    pub fn per_minute(n: u32) -> Self {
        Self {
            interval: Duration::from_secs_f64(60.0 / n as f64),
            burst: n,
        }
    }

    /// Allow `n` requests per hour.
    pub fn per_hour(n: u32) -> Self {
        Self {
            interval: Duration::from_secs_f64(3600.0 / n as f64),
            burst: n,
        }
    }

    /// Override the burst (token bucket) capacity.
    pub fn allow_burst(mut self, burst: u32) -> Self {
        self.burst = burst;
        self
    }

    /// Create a quota with a custom interval and burst size.
    ///
    /// `interval` is the minimum time between allowed requests.
    /// `burst` is the maximum number of requests that can be made in rapid succession.
    pub fn from_parts(interval: Duration, burst: u32) -> Self {
        Self { interval, burst }
    }

    /// The minimum interval between requests.
    pub fn interval(&self) -> Duration {
        self.interval
    }
}
