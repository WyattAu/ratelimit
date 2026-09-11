// Zero-allocation hot-path gate: a counting global allocator proves the
// steady-state check paths are allocation-free.
//
// Compiled to nothing unless the in-memory backend is available and the
// `metrics` feature is OFF (dynamic metric labels allocate by design —
// see PERF-SLO.md).
#![cfg(all(feature = "in-memory", not(feature = "metrics")))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Allocation counter tests for the `RateLimiter::check` hot path.
//!
//! PERF-SLO.md claims a zero-allocation steady state; this binary is the
//! verification (a counting allocator cannot lie about code reading).
//! The iai-callgrind instruction-count gate (CI-only; requires valgrind)
//! pins the cycle cost; this file pins the heap behavior on every
//! `cargo test` run.
//!
//! The check future is polled directly with a no-op waker: the
//! in-memory backend completes on its first poll, and this keeps tokio's
//! runtime plumbing (which allocates incidentally around `block_on`) out
//! of the measurement. The claim under test is about the crate's path.

use std::alloc::{GlobalAlloc, Layout, System};
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

use throttle_kit::{InMemoryBackend, KeyedRateLimiter, Quota, RateLimiter};

/// Drive `fut` to completion on the current thread without a runtime.
fn block_once<F: Future>(fut: F) -> F::Output {
    let mut fut = pin!(fut);
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(output) => output,
        // The in-memory backends never yield: check is straight-line
        // code around a map update.
        Poll::Pending => panic!("check future must complete on its first poll"),
    }
}

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations() -> usize {
    ALLOCATIONS.load(Ordering::Relaxed)
}

const WARM_KEY: &str = "zero-alloc-warm-key";
const ITERATIONS: usize = 100;

/// One sequential test: the allocation counter is process-global, so
/// parallel test threads would pollute each other's counts.
#[test]
fn hot_path_allocation_claims() {
    // --- RateLimiter: warm-key check is allocation-free. ---
    let limiter = RateLimiter::new(Quota::per_second(1_000_000), InMemoryBackend::new());
    // Warm the key: the first call inserts GCRA state (documented as the
    // one-and-only allocation for a key's lifetime).
    assert!(block_once(limiter.check(WARM_KEY)).allowed);

    let before = allocations();
    for _ in 0..ITERATIONS {
        assert!(block_once(limiter.check(WARM_KEY)).allowed);
    }
    assert_eq!(
        allocations(),
        before,
        "warm-key check must not allocate (before: {before}, after: {})",
        allocations()
    );

    // --- KeyedRateLimiter: warm-key checks are allocation-free. ---
    let keyed = KeyedRateLimiter::new(Quota::per_second(1_000_000), InMemoryBackend::new());
    // Warm path with the default quota (borrowed-key override lookup).
    assert!(block_once(keyed.check(WARM_KEY)).allowed);

    let before = allocations();
    for _ in 0..ITERATIONS {
        assert!(block_once(keyed.check(WARM_KEY)).allowed);
    }
    assert_eq!(
        allocations(),
        before,
        "keyed warm-key check must not allocate (before: {before}, after: {})",
        allocations()
    );

    // Override path: the override insert is a configuration action (it
    // may allocate the map key); once it exists, repeated checks are
    // allocation-free again.
    keyed.with_quota_for_key(WARM_KEY, Quota::per_second(1_000_000));
    assert!(block_once(keyed.check(WARM_KEY)).allowed);

    let before = allocations();
    for _ in 0..ITERATIONS {
        assert!(block_once(keyed.check(WARM_KEY)).allowed);
    }
    assert_eq!(
        allocations(),
        before,
        "keyed warm-key check with an override must not allocate"
    );

    // --- Counter sanity guard. ---
    // If this ever fails, the zero-alloc assertions above prove nothing
    // (the counter would be broken, not the fast path miraculously free).
    let limiter = RateLimiter::new(Quota::per_second(1_000_000), InMemoryBackend::new());
    let before = allocations();
    let key = format!("fresh-{before}");
    assert!(block_once(limiter.check(&key)).allowed);
    assert!(
        allocations() > before,
        "first-ever call for a key inserts its state and must allocate \
         (counter sanity check)"
    );
}
