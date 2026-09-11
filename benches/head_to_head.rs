// Benchmarks run on fixed, known-good inputs; unwrap failures abort the
// bench run visibly, which is the desired behavior here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Head-to-head comparison: throttle-kit vs governor 0.10, the market-leading
//! GCRA rate limiter.
//!
//! Both crates implement GCRA; the measured operation is the allow/deny
//! decision for one key under the SAME quota on both sides:
//! 1000 requests/second, burst 100.
//!
//! Fairness notes:
//! - governor is measured through `RateLimiter::direct(..).check()` — its
//!   synchronous, clock-driven (quanta) single-state limiter, i.e. the
//!   cheapest possible path it offers.
//! - throttle-kit is measured twice: `check_sync(..)` (direct analogue) and
//!   the async `check(..)` wrapper, so readers can see the cost of the async
//!   layer separately.
//! - governor's `jitter` feature is disabled: it only affects retry-sleep
//!   timing, not the `check()` decision, and dropping it keeps the dep tree
//!   smaller.
//!
//! "Steady state" caveat: a criterion loop issues checks far faster than
//! 1000/s, so after the first ~100 iterations both limiters settle into the
//! rate-exceeded branch of GCRA. That branch performs the same arithmetic as
//! the allow branch (compute next TAT, compare against the clock, return the
//! decision), and it does so identically on both sides, so the comparison
//! stays apples-to-apples. Rebuilding the limiter per iteration would measure
//! allocator noise instead of the decision path.

#[cfg(feature = "in-memory")]
use criterion::{Criterion, criterion_group, criterion_main};
#[cfg(feature = "in-memory")]
use throttle_kit::{InMemoryBackend, Quota, RateLimiter};

#[cfg(feature = "in-memory")]
const KEY: &str = "bench-key";

#[cfg(feature = "in-memory")]
fn bench_throttle_kit_check_sync(c: &mut Criterion) {
    let mut group = c.benchmark_group("comparison");
    group.bench_function("throttle_kit_check_sync", |b| {
        let backend = InMemoryBackend::new();
        let limiter = RateLimiter::new(Quota::per_second(1_000).allow_burst(100), backend);
        b.iter(|| {
            let _ = std::hint::black_box(limiter.check_sync(KEY));
        });
    });
    group.finish();
}

#[cfg(feature = "in-memory")]
fn bench_throttle_kit_check_async(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("comparison");
    group.bench_function("throttle_kit_check_async", |b| {
        let backend = InMemoryBackend::new();
        let limiter = RateLimiter::new(Quota::per_second(1_000).allow_burst(100), backend);
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            rt.block_on(async {
                for _ in 0..iters {
                    let _ = std::hint::black_box(limiter.check(KEY).await);
                }
            });
            start.elapsed()
        });
    });
    group.finish();
}

#[cfg(feature = "in-memory")]
fn bench_governor_check(c: &mut Criterion) {
    use std::num::NonZeroU32;

    use governor::{Quota as GovernorQuota, RateLimiter as GovernorRateLimiter};

    let mut group = c.benchmark_group("comparison");
    group.bench_function("governor_check_direct", |b| {
        // 1000 req/s (1ms period), burst 100 — identical quota to ours above.
        let quota = GovernorQuota::per_second(NonZeroU32::new(1_000).unwrap())
            .allow_burst(NonZeroU32::new(100).unwrap());
        let limiter = GovernorRateLimiter::direct(quota);
        b.iter(|| {
            let _ = std::hint::black_box(limiter.check());
        });
    });
    group.finish();
}

#[cfg(feature = "in-memory")]
criterion_group!(
    comparison,
    bench_throttle_kit_check_sync,
    bench_throttle_kit_check_async,
    bench_governor_check,
);
#[cfg(feature = "in-memory")]
criterion_main!(comparison);

// Bench targets are `harness = false`, so a `main` must exist even when
// the in-memory feature (and with it every benchmark) is compiled out.
#[cfg(not(feature = "in-memory"))]
fn main() {}
