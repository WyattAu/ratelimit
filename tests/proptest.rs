//! Property-based tests for throttle-kit crate.

use proptest::prelude::*;
use std::time::Duration;

use throttle_kit::{Quota, RateLimitResult};

#[test]
fn quota_per_second_interval_correct() {
    proptest!(|(n in 1u32..10_000u32)| {
        let q = Quota::per_second(n);
        let expected = Duration::from_secs_f64(1.0 / n as f64);
        prop_assert!((q.interval().as_secs_f64() - expected.as_secs_f64()).abs() < 0.001);
    });
}

#[test]
fn quota_per_minute_interval_correct() {
    proptest!(|(n in 1u32..10_000u32)| {
        let q = Quota::per_minute(n);
        let expected = Duration::from_secs_f64(60.0 / n as f64);
        prop_assert!((q.interval().as_secs_f64() - expected.as_secs_f64()).abs() < 0.001);
    });
}

#[test]
fn quota_per_hour_interval_correct() {
    proptest!(|(n in 1u32..10_000u32)| {
        let q = Quota::per_hour(n);
        let expected = Duration::from_secs_f64(3600.0 / n as f64);
        prop_assert!((q.interval().as_secs_f64() - expected.as_secs_f64()).abs() < 0.001);
    });
}

#[test]
fn quota_interval_always_positive() {
    proptest!(|(n in 1u32..100_000u32)| {
        let q = Quota::per_second(n);
        prop_assert!(q.interval() > Duration::ZERO);
    });
}

#[test]
fn quota_burst_matches_rate() {
    proptest!(|(n in 1u32..10_000u32)| {
        let q = Quota::per_second(n);
        prop_assert_eq!(q.burst, n);
    });
}

#[test]
fn quota_allow_burst_overrides() {
    proptest!(|(n in 1u32..1_000u32, burst in 1u32..10_000u32)| {
        let q = Quota::per_second(n).allow_burst(burst);
        prop_assert_eq!(q.burst, burst);
    });
}

#[test]
fn quota_clone_preserves_values() {
    proptest!(|(n in 1u32..10_000u32)| {
        let q = Quota::per_second(n);
        let cloned = q.clone();
        prop_assert_eq!(q.burst, cloned.burst);
        prop_assert_eq!(q.interval(), cloned.interval());
    });
}

#[test]
fn rate_limit_result_headers_always_3() {
    proptest!(|(remaining in 0u64..1_000u64, limit in 1u64..1_000u64)| {
        let r = RateLimitResult {
            allowed: true,
            remaining,
            reset_at: std::time::Instant::now(),
            limit,
            retry_after: None,
        };
        let headers = r.headers();
        prop_assert_eq!(headers.len(), 3);
        prop_assert_eq!(headers[0].0, "X-RateLimit-Limit");
        prop_assert_eq!(headers[1].0, "X-RateLimit-Remaining");
        prop_assert_eq!(headers[2].0, "X-RateLimit-Reset");
    });
}
