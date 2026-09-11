// iai-callgrind benchmarks run once under Valgrind on fixed inputs; the
// harness measures instruction counts, so there is no "expected failure"
// recovery path — a panic aborts the run visibly, which is what we want.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Deterministic regression gate for the `RateLimiter::check` hot path.
//!
//! Unlike criterion (wall-clock, noisy, human-readable trend —
//! `benches/ratelimit_bench.rs`), iai-callgrind counts CPU instructions
//! under Valgrind and is reproducible for a given binary — fit for a CI
//! gate. Criterion stays the source of the wall-clock trend; this file is
//! the pass/fail gate.
//!
//! Workflow:
//!
//! - main: `cargo bench --bench iai_hot_path -- --save-baseline=main`
//!   (done by the `perf-gate` CI job; baselines update intentionally on
//!   every main push).
//! - PRs: `cargo bench --bench iai_hot_path -- --baseline=main --fail-fast`
//!   — any instruction-count regression fails the job.
//! - Locally this needs `valgrind` installed (`apt install valgrind`);
//!   without it, compile-check only: `cargo bench --no-run --bench iai_hot_path`.

#[cfg(feature = "in-memory")]
use std::hint::black_box;

#[cfg(feature = "in-memory")]
use iai_callgrind::{library_benchmark, library_benchmark_group, main};
#[cfg(feature = "in-memory")]
use throttle_kit::{InMemoryBackend, Quota, RateLimiter};

#[cfg(feature = "in-memory")]
type Rt = tokio::runtime::Runtime;

#[cfg(feature = "in-memory")]
fn setup_check() -> (Rt, RateLimiter<InMemoryBackend>) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let limiter = RateLimiter::new(Quota::per_second(1_000_000), InMemoryBackend::new());
    // Warm one check so the measured call is the steady-state path: the
    // first call for a key inserts its GCRA state into the DashMap
    // (allocation + hash insert); every subsequent call is a lookup +
    // GCRA update, which is the path real traffic hits.
    rt.block_on(limiter.check("iai-hot-key"));
    (rt, limiter)
}

// Steady-state GCRA check on a warm key: DashMap lookup + arithmetic +
// state update. Allocation-free as of 1.1.0 (borrowed-key fast path,
// native AFIT) — verified on every test run by the counting allocator in
// tests/zero_alloc_hot_path.rs; this gate pins the instruction count in
// CI (valgrind required).
#[cfg(feature = "in-memory")]
#[library_benchmark]
#[bench::steady_state(setup = setup_check)]
fn check_steady_state(env: (Rt, RateLimiter<InMemoryBackend>)) -> bool {
    let (rt, limiter) = env;
    rt.block_on(async { black_box(limiter.check("iai-hot-key").await).allowed })
}

#[cfg(feature = "in-memory")]
library_benchmark_group!(name = iai_hot_path; benchmarks = check_steady_state);

#[cfg(feature = "in-memory")]
main!(library_benchmark_groups = iai_hot_path);

// Bench targets are `harness = false`, so a `main` must exist even when
// the in-memory feature (and with it every benchmark) is compiled out.
#[cfg(not(feature = "in-memory"))]
fn main() {}
