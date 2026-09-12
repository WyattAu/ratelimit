// Live-gated wire tests: unwrap/expect, slicing, and panicking asserts are
// the test signal here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
#![cfg(feature = "redis")]

//! Live Redis GCRA integration tests — the Lua-script decision loop
//! cannot run against the in-memory mock, so these are fixture-gated
//! like the rest of the estate:
//!
//! ```sh
//! docker run -d --name throttle-kit-redis -p 6379:6379 redis:7
//! cargo test --features redis --test redis_gcra_live -- --ignored --nocapture
//! ```
//!
//! What these prove beyond the unit tests:
//! - the GCRA Lua script admits exactly `burst` requests, in sequence
//!   and under concurrency (atomicity of the Redis decision loop);
//! - denials carry a `retry_after` and a zero `remaining`;
//! - distinct keys are fully independent quotas.

use std::sync::Arc;
use std::time::Duration;

use throttle_kit::{Quota, RateLimitBackend, RedisBackend};

/// Default matches the documented fixture; override with
/// `THROTTLE_KIT_REDIS_URL` when 6379 is taken (CI uses the service port).
fn redis_url() -> String {
    std::env::var("THROTTLE_KIT_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".into())
}

async fn backend() -> RedisBackend {
    RedisBackend::connect(&redis_url()).await.unwrap()
}

async fn reset_keys(keys: &[&str]) {
    let client = redis::Client::open(redis_url()).unwrap();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    for key in keys {
        let _: () = redis::cmd("DEL")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
    }
}

// ---------------------------------------------------------------------------
// Sequential GCRA allowance sequence
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live Redis server (docker run -p 6379:6379 redis:7)"]
async fn sequential_checks_follow_the_expected_allowance_sequence() {
    let backend = backend().await;
    reset_keys(&["gcra-live:seq"]).await;

    // burst 3 per second: emission interval = 1000/3 ≈ 333 ms, so a fast
    // sequential burst must see exactly [allow, allow, allow, deny, deny].
    let quota = Quota::per_second(3);
    let mut allowed_seq = Vec::new();
    let mut denials = 0;

    for i in 0..6 {
        let result = backend.check("gcra-live:seq", &quota).await;
        allowed_seq.push(result.allowed);
        if result.allowed {
            // `remaining` is the GCRA headroom in whole emission intervals,
            // timing-dependent — but it must never exceed the burst.
            assert!(
                result.remaining < quota.burst as u64,
                "check {i}: remaining {remaining} must be < burst",
                remaining = result.remaining
            );
            assert!(
                result.retry_after.is_none(),
                "allowed checks carry no retry_after"
            );
        } else {
            denials += 1;
            let retry_after = result.retry_after.expect("denials must carry retry_after");
            assert!(retry_after > Duration::ZERO, "retry_after must be positive");
            assert_eq!(result.remaining, 0, "denied checks have nothing left");
        }
    }

    assert_eq!(
        allowed_seq,
        vec![true, true, true, false, false, false],
        "GCRA must admit exactly `burst` requests then deny"
    );
    assert_eq!(denials, 3);
}

#[tokio::test]
#[ignore = "requires a live Redis server (docker run -p 6379:6379 redis:7)"]
async fn denial_clears_after_the_retry_window() {
    let backend = backend().await;
    reset_keys(&["gcra-live:window"]).await;

    let quota = Quota::per_second(2);
    // Drain the burst.
    for _ in 0..2 {
        assert!(backend.check("gcra-live:window", &quota).await.allowed);
    }
    let denied = backend.check("gcra-live:window", &quota).await;
    assert!(!denied.allowed);
    let retry_after = denied.retry_after.unwrap();
    assert!(
        retry_after <= Duration::from_secs(2),
        "retry_after must be within one interval-ish"
    );

    // Waiting out the window admits the next request again.
    tokio::time::sleep(retry_after + Duration::from_millis(50)).await;
    assert!(
        backend.check("gcra-live:window", &quota).await.allowed,
        "after the retry window the quota refills"
    );
}

// ---------------------------------------------------------------------------
// Concurrency: the Lua decision loop is atomic
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live Redis server (docker run -p 6379:6379 redis:7)"]
async fn concurrent_checkers_never_exceed_the_quota() {
    let backend = Arc::new(backend().await);
    reset_keys(&["gcra-live:conc"]).await;

    // per_minute(20): emission = 3 s, so the refill during this sub-second
    // test is negligible and the admitted total must be exactly the burst.
    let quota = Arc::new(Quota::per_minute(20));

    let allowed_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let backend = backend.clone();
        let quota = quota.clone();
        let allowed_count = allowed_count.clone();
        handles.push(tokio::spawn(async move {
            for _ in 0..5 {
                let result = backend.check("gcra-live:conc", &quota).await;
                if result.allowed {
                    allowed_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(
        allowed_count.load(std::sync::atomic::Ordering::SeqCst),
        20,
        "40 concurrent checks against burst 20 must admit exactly 20"
    );
}

// ---------------------------------------------------------------------------
// Key isolation
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live Redis server (docker run -p 6379:6379 redis:7)"]
async fn distinct_keys_have_independent_quotas() {
    let backend = backend().await;
    reset_keys(&["gcra-live:key-a", "gcra-live:key-b"]).await;

    let quota = Quota::per_second(2);
    // Exhaust key A fully.
    for _ in 0..2 {
        assert!(backend.check("gcra-live:key-a", &quota).await.allowed);
    }
    assert!(!backend.check("gcra-live:key-a", &quota).await.allowed);

    // Key B is untouched by A's exhaustion.
    for _ in 0..2 {
        assert!(backend.check("gcra-live:key-b", &quota).await.allowed);
    }

    // And A stays denied — its own window governs it.
    assert!(!backend.check("gcra-live:key-a", &quota).await.allowed);
}

// Docker-gated: uses a throwaway container that is stopped mid-test so
// the command-failure path is exercised against a genuinely dead server.
#[tokio::test]
#[ignore = "requires docker (spins up its own throwaway Redis container)"]
async fn backend_error_fails_open() {
    use testcontainers::runners::AsyncRunner as _;

    let container = testcontainers_modules::redis::Redis::default()
        .start()
        .await
        .unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(6379).await.unwrap();
    let backend = RedisBackend::connect(&format!("redis://{host}:{port}/"))
        .await
        .unwrap();

    // Sanity: against a live server the check is a real decision.
    let alive = backend
        .check("gcra-live:failopen-pre", &Quota::per_second(5))
        .await;
    assert!(alive.allowed);

    // Kill the server, then check: the documented contract is fail-open —
    // Redis unavailability must never take the caller down.
    container.stop().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let result = backend
        .check("gcra-live:failopen", &Quota::per_second(5))
        .await;
    assert!(
        result.allowed,
        "Redis command failures must fail OPEN: {result:?}"
    );
    assert_eq!(result.remaining, 0, "fail-open results carry no allowance");
    assert!(result.retry_after.is_none());
}
