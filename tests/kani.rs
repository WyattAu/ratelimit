#![cfg(kani)]
//! Kani bounded model-checking harnesses for the GCRA decision core
//! (`backend::gcra_decide`).
//!
//! `gcra_decide` was extracted out of the DashMap entry method (and mirrors
//! the Redis GCRA Lua script in `src/redis.rs`) so that the allow/deny,
//! retry_after, and remaining math is a *total pure function*:
//! no clocks, no I/O, no interior state. That makes it exhaustively
//! checkable — the harnesses below feed it arbitrary `u64`s.
//!
//! # Properties
//!
//! Harness 1 (`..._invariants_for_arbitrary_inputs`) uses **fully arbitrary
//! u64 inputs** (no bounds needed) and checks:
//!
//! 1. `allowed ⟺ retry_after == 0` — a conforming request never owes a
//!    wait; a denied one always reports a strictly positive one.
//! 2. `remaining ≤ burst` — the reported remaining capacity is clamped to
//!    the burst size (a stale far-future TAC must not mint phantom budget).
//! 3. `allowed ⟹ remaining ≥ 1` — an allowed request really had budget.
//! 4. `denied ⟹ remaining == 0 && retry_after ≥ 1`.
//! 5. Panic-freedom and arithmetic-overflow-freedom for *all* u64 inputs
//!    (including `emission_interval_ms == 0` and TAC/now near `u64::MAX`):
//!    Kani's injected overflow and division checks prove these implicitly.
//!    The implementation relies on `u128` intermediates and an unsigned
//!    rearrangement of the comparison (`new_tat <= now + emission * burst`
//!    instead of the underflow-prone `new_tat - emission * burst >= now`);
//!    the only division is by `max(emission, 1)`.
//!
//! Harness 2 (`..._matches_textbook_gcra`) bounds the inputs to a tiny
//! domain and checks agreement with the textbook GCRA equations (the same
//! math as the Redis Lua script, evaluated in `i64` with no clamping).
//!
//! # Notes
//!
//! - The `client_ip.rs` trust-walk contains no GCRA arithmetic (identity
//!   resolution only); the decision math lives in `backend.rs` + the Redis
//!   Lua script, which this now covers.
//! - Per-request cost is 1 in the in-memory backend, matching the Lua
//!   script's `cost` parameter used by `RedisBackend`.
//!
//! Run with:
//! ```text
//! timeout 600 cargo kani --tests
//! ```

use throttle_kit::gcra_decide;

#[kani::proof]
fn kani_gcra_decide_invariants_for_arbitrary_inputs() {
    let now_ms: u64 = kani::any();
    let tac_ms: u64 = kani::any();
    let emission_interval_ms: u64 = kani::any();
    let burst: u64 = kani::any();

    let (allowed, retry_after_ms, remaining) =
        gcra_decide(now_ms, tac_ms, emission_interval_ms, burst);

    // P1: allowed ⟺ retry_after == 0.
    kani::assert(
        allowed == (retry_after_ms == 0),
        "allowed ⟺ retry_after == 0",
    );
    // P2: remaining never exceeds the burst capacity.
    kani::assert(remaining <= burst, "remaining ≤ burst");

    if allowed {
        // P3: an allowed request had real budget available.
        kani::assert(remaining >= 1, "allowed ⟹ remaining ≥ 1");
    } else {
        // P4: a denied request reports no budget and a real wait.
        kani::assert(remaining == 0, "denied ⟹ remaining == 0");
        kani::assert(retry_after_ms >= 1, "denied ⟹ retry_after ≥ 1");
    }
    // P5 (panic-/overflow-freedom) is discharged by Kani's automatic
    // overflow, division-by-zero, and unreachable checks on this harness.
}

#[kani::proof]
fn kani_gcra_decide_matches_textbook_gcra() {
    let now: u64 = kani::any();
    let tac: u64 = kani::any();
    let emission: u64 = kani::any();
    let burst: u64 = kani::any();
    kani::assume(now <= 16);
    kani::assume(tac <= 16);
    kani::assume(emission >= 1 && emission <= 4);
    kani::assume(burst <= 3);

    // Textbook GCRA (the Redis Lua math in i64 — no clamping needed at
    // these magnitudes, so any mismatch is a semantic bug, not a guard).
    let now_i = now as i64;
    let new_tat = (tac as i64).max(now_i) + emission as i64;
    let allow_at = new_tat - emission as i64 * burst as i64;
    let ref_allowed = now_i >= allow_at;

    let (allowed, retry_after_ms, remaining) = gcra_decide(now, tac, emission, burst);

    kani::assert(allowed == ref_allowed, "agrees with textbook GCRA decision");
    if allowed {
        let ref_remaining = ((new_tat - now_i) / emission as i64) as u64;
        kani::assert(remaining == ref_remaining.min(burst), "remaining matches");
        kani::assert(retry_after_ms == 0, "no retry on allow");
    } else {
        kani::assert(
            retry_after_ms == (allow_at - now_i) as u64,
            "retry_after matches",
        );
        kani::assert(remaining == 0, "no remaining on deny");
    }
}
