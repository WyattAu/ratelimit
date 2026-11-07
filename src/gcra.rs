//! Integer-microsecond GCRA cell — the Lean-proven core.
//!
//! This module is the authoritative GCRA implementation for the estate: its
//! semantics are machine-checked in `proofs/RatelimitProofs/Gcra.lean`
//! (namespace `Ratelimit.Gcra`, Lean 4.35.0 + Mathlib v4.35.0, zero `sorry`).
//! The theorems cited below are the runtime mirrors of those proofs:
//!
//! * THM-RATE-001 `run_monotone`       — TAT never decreases
//! * THM-RATE-002 `bounded_debt`       — after an admission at `u`, `TAT ≤ u + τ + T`
//! * THM-RATE-003 `spacing_k`          — k-th admission `t_k ≥ TAT₀ + (k-1)·T − τ`
//! * THM-RATE-004 `pair_spacing`       — `t_j − t_i ≥ (j−i)·T − τ`
//! * THM-RATE-005 `burst_same_instant` — at most `b` admissions at one instant (tight)
//! * THM-RATE-006 `window_bound` / `int_window_bound` — Cruz (σ,ρ) window bound
//! * THM-RATE-007 `conforms_mono`      — a later arrival never breaks conformance
//!
//! Semantics (integer µs, matching the proofs exactly):
//!
//! * conformance: `now ≥ TAT − τ`
//! * transition:  `TAT ← max(TAT, now) + T` (admissions only)
//! * burst capacity: `b = 1 + ⌊τ/T⌋`, so `(b−1)·T ≤ τ < b·T`
//!
//! Time is `u64` microseconds on a monotonic clock; all intermediate math is
//! `u128`, and every `u64` input is handled without overflow or panic.

/// GCRA parameters in integer microseconds.
///
/// `T > 0` is the emission interval (µs per request); `τ ≥ 0` is the burst
/// tolerance (µs). The derived burst capacity is `b = 1 + ⌊τ/T⌋`, which
/// satisfies the proof invariant `(b−1)·T ≤ τ < b·T`. A `τ = (b−1)·T`
/// configuration is the common integer case (`int_window_bound`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcraParams {
    /// Emission interval `T` in µs; always `≥ 1` (clamped).
    emission_us: u64,
    /// Burst tolerance `τ` in µs.
    tau_us: u64,
}

impl GcraParams {
    /// Builds parameters from raw integer microseconds.
    ///
    /// A zero `emission_us` (sub-microsecond rate) is clamped to `1` so the
    /// divisions below are well-defined.
    pub const fn new(emission_us: u64, tau_us: u64) -> Self {
        Self {
            emission_us: if emission_us == 0 { 1 } else { emission_us },
            tau_us,
        }
    }

    /// Builds parameters from a request rate and a burst capacity.
    ///
    /// `T = round(1e6 / rps)` µs and `τ = (burst−1)·T`, the integer
    /// configuration of the proofs. Non-finite or non-positive `rps` clamps
    /// to `T = 1` µs; `burst` saturating multiplies are documented clamps for
    /// absurd inputs.
    pub fn from_rate(rps: f64, burst: u64) -> Self {
        let emission_us = if rps.is_finite() && rps > 0.0 {
            let t = 1_000_000.0 / rps;
            let t = if t.is_finite() { t } else { f64::MAX };
            // `as`-cast from f64 saturates; clamp keeps `τ = (burst−1)·T`
            // inside `u64` range for any representable `burst`.
            (t as u64).clamp(1, u64::MAX)
        } else {
            1
        };
        let tau_us = burst.saturating_sub(1).saturating_mul(emission_us);
        Self::new(emission_us, tau_us)
    }

    /// Emission interval `T` in µs (`> 0`).
    pub const fn emission_us(&self) -> u64 {
        self.emission_us
    }

    /// Burst tolerance `τ` in µs (`≥ 0`).
    pub const fn tau_us(&self) -> u64 {
        self.tau_us
    }

    /// Burst capacity `b = 1 + ⌊τ/T⌋` with `(b−1)·T ≤ τ < b·T` (THM-RATE-005).
    pub fn burst(&self) -> u64 {
        1 + self.tau_us / self.emission_us
    }
}

/// Outcome of one GCRA step ([`gcra_step_us`] / [`GcraCell::check`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcraOutcome {
    /// Whether the request conforms (`now ≥ TAT − τ`).
    pub allowed: bool,
    /// The state after the step: `max(TAT, now) + T` on an admission,
    /// `TAT` unchanged on a denial (THM-RATE-001).
    pub next_tat_us: u64,
    /// Microseconds until the earliest conforming arrival; always `0` iff
    /// the request was allowed.
    pub retry_after_us: u64,
    /// Reported remaining capacity in `[0, b]`; always `0` on a denial.
    /// Same convention as [`crate::gcra_decide`].
    pub remaining: u64,
}

/// Conformance predicate: `now ≥ TAT − τ`.
///
/// Rearranged as `now + τ ≥ TAT` so unsigned arithmetic never underflows.
pub fn gcra_conforms_us(now_us: u64, tat_us: u64, params: GcraParams) -> bool {
    u128::from(now_us) + u128::from(params.tau_us) >= u128::from(tat_us)
}

/// One pure GCRA step: decide, and compute the transition
/// `TAT ← max(TAT, now) + T` (applied only on conformance).
///
/// Total and side-effect-free; every `u64` input is handled without
/// overflow or panic.
pub fn gcra_step_us(now_us: u64, tat_us: u64, params: GcraParams) -> GcraOutcome {
    if gcra_conforms_us(now_us, tat_us, params) {
        let next_tat = u128::from(tat_us).max(u128::from(now_us)) + u128::from(params.emission_us);
        let next_tat_us = u64::try_from(next_tat).unwrap_or(u64::MAX);
        // Same remaining convention as `gcra_decide`: whole emission
        // intervals visible right now, clamped to the burst capacity.
        // The result is `≤ burst ≤ u64::MAX`, so the cast cannot truncate.
        let visible = (next_tat - u128::from(now_us)) / u128::from(params.emission_us);
        let remaining = u64::try_from(visible.min(u128::from(params.burst()))).unwrap_or(u64::MAX);
        GcraOutcome {
            allowed: true,
            next_tat_us,
            retry_after_us: 0,
            remaining,
        }
    } else {
        // `tat − τ − now` is strictly positive on this branch; the `u128`
        // subtraction cannot underflow because `tat > now + τ` holds here.
        let wait = u128::from(tat_us) - (u128::from(now_us) + u128::from(params.tau_us));
        GcraOutcome {
            allowed: false,
            next_tat_us: tat_us,
            retry_after_us: u64::try_from(wait).unwrap_or(u64::MAX),
            remaining: 0,
        }
    }
}

/// A single GCRA cell: the mutable `TAT` state for one key (one domain, one
/// bucket, one API client — the caller owns the keying).
///
/// The initial state `TAT = now` hands out a full burst budget, exactly like
/// a fresh key in the in-memory/Redis backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcraCell {
    tat_us: u64,
}

impl GcraCell {
    /// Creates a fresh cell at `now_us` with a full burst budget.
    pub const fn new(now_us: u64) -> Self {
        Self { tat_us: now_us }
    }

    /// The stored theoretical arrival time (µs).
    pub const fn tat_us(&self) -> u64 {
        self.tat_us
    }

    /// Runs one GCRA step at `now_us` and persists the new TAT on
    /// conformance. Denials never move the TAT backwards (THM-RATE-001).
    pub fn check(&mut self, now_us: u64, params: GcraParams) -> GcraOutcome {
        let outcome = gcra_step_us(now_us, self.tat_us, params);
        self.tat_us = outcome.next_tat_us;
        outcome
    }

    /// Microseconds from `now_us` until the earliest conforming arrival
    /// (`max(TAT − τ − now, 0)`); always `0` when the cell conforms now.
    pub fn retry_after_us(&self, now_us: u64, params: GcraParams) -> u64 {
        u128::from(self.tat_us)
            .saturating_sub(u128::from(now_us) + u128::from(params.tau_us))
            .min(u128::from(u64::MAX)) as u64
    }

    /// Same-instant burst credits visible at `now_us`, in `[0, b]`:
    /// `b − ⌈max(TAT − now, 0)/T⌉`.
    ///
    /// By THM-RATE-005 at most `b` admissions can occur at one instant; a
    /// fresh or fully recovered cell reports the full `b`, and each admission
    /// consumes exactly one credit. This is a reporting view — the admission
    /// decision itself is always [`GcraCell::check`].
    pub fn credits(&self, now_us: u64, params: GcraParams) -> u64 {
        let behind = u128::from(self.tat_us).saturating_sub(u128::from(now_us));
        let t = u128::from(params.emission_us);
        // Ceiling division: a partial elapsed interval still consumes a
        // whole credit, matching the token-bucket view consumers expect.
        let consumed = behind.div_ceil(t);
        let burst = u128::from(params.burst());
        (burst - consumed.min(burst)) as u64
    }
}

// Tests exercise proof-mirroring invariants directly; unwrap/expect,
// slicing, and panicking asserts are acceptable here — violations surface
// as test failures, not production panics.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const T: u64 = 100_000; // 100 ms emission interval

    fn params(tau_us: u64) -> GcraParams {
        GcraParams::new(T, tau_us)
    }

    #[test]
    fn burst_is_one_plus_floor_tau_over_t() {
        // b = 1 + ⌊τ/T⌋ with (b−1)·T ≤ τ < b·T.
        assert_eq!(params(0).burst(), 1);
        assert_eq!(params(T - 1).burst(), 1);
        assert_eq!(params(T).burst(), 2);
        assert_eq!(params(4 * T + 5).burst(), 5);
    }

    #[test]
    fn thm_rate_005_tight_same_instant_burst_bound() {
        // Exactly b admissions at one instant; the (b+1)-th is denied.
        let p = params(4 * T); // b = 5
        let mut cell = GcraCell::new(1_000);
        for _ in 0..5 {
            assert!(cell.check(1_000, p).allowed);
        }
        let denied = cell.check(1_000, p);
        assert!(!denied.allowed);
        assert_eq!(denied.remaining, 0);
        assert_eq!(denied.retry_after_us, T);
    }

    #[test]
    fn thm_rate_001_tat_never_decreases_under_clock_skew() {
        // A late (non-conforming) arrival leaves the TAT unchanged; an
        // admission with now < tat advances it monotonically.
        let p = params(0);
        let mut cell = GcraCell::new(1_000);
        let out = cell.check(1_100, p);
        assert!(out.allowed);
        assert_eq!(out.next_tat_us, 1_100 + T);
        // Skewed-back clock: denied, TAT must not decrease.
        let out = cell.check(500, p);
        assert!(!out.allowed);
        assert_eq!(out.next_tat_us, 1_100 + T);
        assert_eq!(cell.tat_us(), 1_100 + T);
    }

    #[test]
    fn thm_rate_007_later_arrival_conforms_if_earlier_did() {
        let p = params(2 * T);
        let tat = 500_000;
        // 300_000 + 2T ≥ 500_000 conforms; any later arrival does too.
        assert!(gcra_conforms_us(300_000, tat, p));
        assert!(gcra_conforms_us(310_000, tat, p));
        // 100_000 + 2T < 500_000 does not conform.
        assert!(!gcra_conforms_us(100_000, tat, p));
    }

    #[test]
    fn thm_rate_004_zero_tau_pair_spacing_is_exact() {
        // τ = 0: consecutive conforming admissions are exactly T apart.
        let p = params(0);
        let mut cell = GcraCell::new(0);
        let first = cell.check(1_000, p);
        assert!(first.allowed);
        // j − i = 1 ⇒ spacing ≥ 1·T − 0; 1_000 + T − 1 µs must be denied.
        assert!(!cell.check(1_000 + T - 1, p).allowed);
        assert!(cell.check(1_000 + T, p).allowed);
        assert!(!cell.check(1_000 + 2 * T - 1, p).allowed);
        assert!(cell.check(1_000 + 2 * T, p).allowed);
    }

    #[test]
    fn thm_rate_002_bounded_debt_after_admission() {
        // After an admission at u: TAT ≤ u + τ + T.
        let p = params(3 * T);
        let u = 50_000;
        let mut cell = GcraCell::new(u);
        let out = cell.check(u, p);
        assert!(out.allowed);
        assert!(out.next_tat_us <= u + 3 * T + T);
    }

    #[test]
    fn thm_rate_006_int_window_bound() {
        // τ = (b−1)·T, b = 3: three admissions in a zero-length window; the
        // fourth needs d > (4−3)·T = T (the bound is strict).
        let p = params(2 * T);
        let mut cell = GcraCell::new(0);
        for _ in 0..3 {
            assert!(cell.check(10_000, p).allowed);
        }
        assert!(!cell.check(10_000 + T - 1, p).allowed);
        assert!(cell.check(10_000 + T, p).allowed);
    }

    #[test]
    fn credits_fresh_full_then_one_per_admission() {
        let p = params(4 * T); // b = 5
        let mut cell = GcraCell::new(1_000);
        assert_eq!(cell.credits(1_000, p), 5);
        assert!(cell.check(1_000, p).allowed);
        assert_eq!(cell.credits(1_000, p), 4);
        assert!(cell.check(1_000, p).allowed);
        assert_eq!(cell.credits(1_000, p), 3);
        // Partial interval still consumes a whole credit.
        let mut cell = GcraCell::new(1_000);
        assert!(cell.check(1_000, p).allowed);
        assert_eq!(cell.credits(1_000 + T / 2, p), params(4 * T).burst() - 1);
    }

    #[test]
    fn retry_after_is_zero_iff_conforming() {
        // τ = 2T ⇒ b = 3: three same-instant admissions, the fourth denied
        // with wait = tat − τ − now = (10_000 + 3T) − 2T − 10_000 = T.
        let p = params(2 * T);
        let mut cell = GcraCell::new(10_000);
        for _ in 0..3 {
            let out = cell.check(10_000, p);
            assert!(out.allowed && out.retry_after_us == 0);
        }
        let out = cell.check(10_000, p);
        assert!(!out.allowed);
        assert_eq!(out.retry_after_us, T);
        assert_eq!(cell.retry_after_us(10_000, p), T);
        // One full interval later the cell conforms again → wait 0.
        assert_eq!(cell.retry_after_us(10_000 + T, p), 0);
        assert!(cell.check(10_000 + T, p).allowed);
    }

    #[test]
    fn extreme_inputs_no_overflow_or_panic() {
        // All-u64 extremes must stay panic-free (u128 internals).
        let p = GcraParams::new(u64::MAX, u64::MAX);
        let mut cell = GcraCell::new(u64::MAX);
        let out = cell.check(u64::MAX, p);
        assert!(out.allowed);
        assert_eq!(out.next_tat_us, u64::MAX);

        let p = GcraParams::from_rate(f64::NAN, u64::MAX);
        assert_eq!(p.emission_us(), 1);
        let p = GcraParams::from_rate(-1.0, 3);
        assert_eq!(p.emission_us(), 1);
        let p = GcraParams::from_rate(1.0, 3);
        assert_eq!(p.emission_us(), 1_000_000);
        assert_eq!(p.tau_us(), 2_000_000);

        // Sub-microsecond rates clamp to T = 1 µs, no panic.
        let p = GcraParams::new(0, 0);
        let mut cell = GcraCell::new(5);
        assert!(cell.check(5, p).allowed);
    }

    #[test]
    fn from_rate_matches_duration_config() {
        let p = GcraParams::new(500_000, 500_000);
        assert_eq!(p.burst(), 2);
        assert_eq!(p.emission_us(), Duration::from_millis(500).as_micros() as u64);
    }
}
