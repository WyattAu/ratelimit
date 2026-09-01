//! Integration tests for the ratelimit (throttle-kit) crate.
//!
//! Tests quota creation, RateLimitResult fields, InMemoryBackend check/allow/deny,
//! and RateLimitError display.

use std::time::Duration;

use throttle_kit::{InMemoryBackend, RateLimitError, RateLimiter, Quota};

// ---------------------------------------------------------------------------
// Quota creation
// ---------------------------------------------------------------------------

#[test]
fn quota_per_second_interval_calculation() {
    let q = Quota::per_second(10);
    assert_eq!(q.burst, 10);
    assert_eq!(q.interval(), Duration::from_millis(100));
}

#[test]
fn quota_per_minute_interval_calculation() {
    let q = Quota::per_minute(120);
    assert_eq!(q.burst, 120);
    assert_eq!(q.interval(), Duration::from_millis(500));
}

#[test]
fn quota_per_hour_interval_calculation() {
    let q = Quota::per_hour(360);
    assert_eq!(q.burst, 360);
    assert_eq!(q.interval(), Duration::from_secs(10));
}

#[test]
fn quota_from_parts() {
    let q = Quota::from_parts(Duration::from_millis(250), 50);
    assert_eq!(q.burst, 50);
    assert_eq!(q.interval(), Duration::from_millis(250));
}

#[test]
fn quota_allow_burst_override() {
    let q = Quota::per_second(10).allow_burst(25);
    assert_eq!(q.burst, 25);
    assert_eq!(q.interval(), Duration::from_millis(100));
}

// ---------------------------------------------------------------------------
// RateLimitResult fields
// ---------------------------------------------------------------------------

#[test]
fn result_allowed_with_remaining() {
    let r = throttle_kit::RateLimitResult {
        allowed: true,
        remaining: 9,
        reset_at: std::time::Instant::now(),
        limit: 10,
        retry_after: None,
    };
    assert!(r.allowed);
    assert_eq!(r.remaining, 9);
    assert_eq!(r.limit, 10);
    assert!(r.retry_after.is_none());
}

#[test]
fn result_denied_with_retry_after() {
    let r = throttle_kit::RateLimitResult {
        allowed: false,
        remaining: 0,
        reset_at: std::time::Instant::now(),
        limit: 10,
        retry_after: Some(Duration::from_secs(1)),
    };
    assert!(!r.allowed);
    assert_eq!(r.remaining, 0);
    assert!(r.retry_after.is_some());
}

#[test]
fn result_headers_contain_correct_keys() {
    let r = throttle_kit::RateLimitResult {
        allowed: true,
        remaining: 5,
        reset_at: std::time::Instant::now() + Duration::from_secs(30),
        limit: 10,
        retry_after: None,
    };
    let headers = r.headers();
    assert_eq!(headers.len(), 3);
    assert_eq!(headers[0].0, "X-RateLimit-Limit");
    assert_eq!(headers[0].1, "10");
    assert_eq!(headers[1].0, "X-RateLimit-Remaining");
    assert_eq!(headers[1].1, "5");
    assert_eq!(headers[2].0, "X-RateLimit-Reset");
}

// ---------------------------------------------------------------------------
// InMemoryBackend check / allow / deny
// ---------------------------------------------------------------------------

#[tokio::test]
async fn backend_allows_first_request() {
    let backend = InMemoryBackend::new();
    let limiter = RateLimiter::new(Quota::per_second(5), backend);

    let result = limiter.check("key-1").await;
    assert!(result.allowed);
    assert_eq!(result.remaining, 4);
    assert_eq!(result.limit, 5);
}

#[tokio::test]
async fn backend_exhausts_quota() {
    let backend = InMemoryBackend::new();
    let limiter = RateLimiter::new(Quota::per_second(3), backend);

    let r1 = limiter.check("user").await;
    assert!(r1.allowed);

    let r2 = limiter.check("user").await;
    assert!(r2.allowed);

    let r3 = limiter.check("user").await;
    assert!(r3.allowed);

    let r4 = limiter.check("user").await;
    assert!(!r4.allowed);
    assert!(r4.retry_after.is_some());
    assert_eq!(r4.remaining, 0);
}

#[tokio::test]
async fn backend_keys_are_isolated() {
    let backend = InMemoryBackend::new();
    let limiter = RateLimiter::new(Quota::per_second(1), backend);

    let r_a = limiter.check("a").await;
    assert!(r_a.allowed);

    // "a" is now exhausted
    let r_a2 = limiter.check("a").await;
    assert!(!r_a2.allowed);

    // "b" should still be allowed
    let r_b = limiter.check("b").await;
    assert!(r_b.allowed);
}

#[test]
fn backend_new_and_default() {
    let _ = InMemoryBackend::new();
    let _ = InMemoryBackend::default();
}

#[test]
fn sync_check_works() {
    let backend = InMemoryBackend::new();
    let limiter = RateLimiter::new(Quota::per_second(10), backend);

    let r = limiter.check_sync("sync-key");
    assert!(r.allowed);
    assert_eq!(r.remaining, 9);
}

#[test]
fn sync_check_exhaustion() {
    let backend = InMemoryBackend::new();
    let limiter = RateLimiter::new(Quota::per_second(2), backend);

    let _ = limiter.check_sync("k");
    let _ = limiter.check_sync("k");
    let r = limiter.check_sync("k");
    assert!(!r.allowed);
}

#[tokio::test]
async fn rate_limiter_is_cloneable() {
    let backend = InMemoryBackend::new();
    let limiter = RateLimiter::new(Quota::per_second(5), backend);

    let limiter2 = limiter.clone();
    let r = limiter2.check("clone-test").await;
    assert!(r.allowed);
}

#[tokio::test]
async fn per_minute_quota_works() {
    let backend = InMemoryBackend::new();
    let limiter = RateLimiter::new(Quota::per_minute(100), backend);

    let r = limiter.check("user").await;
    assert!(r.allowed);
    assert_eq!(r.limit, 100);
}

// ---------------------------------------------------------------------------
// RateLimitError display
// ---------------------------------------------------------------------------

#[test]
fn error_rate_limited_display() {
    let e = RateLimitError::RateLimited;
    assert_eq!(e.to_string(), "rate limit exceeded");
}

#[test]
fn error_backend_error_display() {
    let e = RateLimitError::BackendError("timeout connecting to Redis".into());
    assert_eq!(
        e.to_string(),
        "rate limit backend error: timeout connecting to Redis"
    );
}

#[test]
fn error_debug_format() {
    let e = RateLimitError::RateLimited;
    let debug = format!("{:?}", e);
    assert!(debug.contains("RateLimited"));
}
