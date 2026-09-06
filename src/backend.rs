#[cfg(feature = "in-memory")]
use std::sync::OnceLock;
#[cfg(feature = "in-memory")]
use std::time::{Duration, Instant};

use crate::metrics::RateLimitResult;
use crate::quota::Quota;

/// Backend trait for rate limit state storage.
#[async_trait::async_trait]
pub trait RateLimitBackend: Send + Sync + 'static {
    /// Check if a request for `key` is allowed under the given `quota`.
    async fn check(&self, key: &str, quota: &Quota) -> RateLimitResult;
}

/// Pure GCRA (Generic Cell Rate Algorithm) conformance decision.
///
/// This is the same math as the Redis GCRA Lua script in `src/redis.rs`
/// (`new_tat = max(tac, now) + emission`, `allow_at = new_tat - emission *
/// burst`), lifted out of the DashMap/Redis contexts so the decision core is
/// a total, side-effect-free function that can be exhaustively model-checked
/// with Kani (see `tests/kani.rs`).
///
/// Returns `(allowed, retry_after_ms, remaining)`:
///
/// - `allowed`: whether the request conforms.
/// - `retry_after_ms`: milliseconds until the next request can conform;
///   always `0` iff the request was allowed.
/// - `remaining`: reported remaining capacity, in `[0, burst]`; always `0`
///   when the request was denied.
///
/// # Argument contract
///
/// - `now_ms` / `tac_ms`: monotonic clock reading and the key's stored
///   *theoretical arrival time* (TAC). **Every** `u64` input is handled
///   without overflow or panic: all intermediate math is `u128`.
/// - `emission_interval_ms`: minimum spacing between conforming requests.
///   `0` (sub-millisecond quotas) is clamped to `1` ms so the divisions
///   below are well-defined.
/// - `burst_ms`: burst capacity in requests. `0` is strict GCRA: nothing
///   conforms before a full interval has elapsed.
pub fn gcra_decide(
    now_ms: u64,
    tac_ms: u64,
    emission_interval_ms: u64,
    burst_ms: u64,
) -> (bool, u64, u64) {
    // Sub-millisecond emission intervals are floored to 1 ms so the
    // divisions below are well-defined (no divide-by-zero panic).
    let emission = u128::from(emission_interval_ms.max(1));
    let burst = u128::from(burst_ms);
    let now = u128::from(now_ms);

    let new_tat = u128::from(tac_ms).max(now) + emission;
    // No overflow is possible: `emission * burst` is at most
    // (2^64-1)^2 < 2^128, and adding `now` (< 2^64) stays well below
    // u128::MAX. (The naive signed form `new_tat - emission * burst` could
    // underflow, so the comparison is rearranged into the all-unsigned
    // `new_tat <= now + emission * burst`.)
    let conforms = new_tat <= now + emission * burst;

    if conforms {
        // Tokens available right now (floor division), clamped to the burst
        // capacity so the reported value can never exceed `burst`. The
        // result is <= burst <= u64::MAX, so the cast cannot truncate.
        let remaining = ((new_tat - now) / emission).min(burst) as u64;
        (true, 0, remaining)
    } else {
        // `allow_at - now` in ms: strictly positive on this branch and
        // bounded by `new_tat - now`, which only exceeds u64::MAX for
        // absurd inputs (TAC near u64::MAX with now near 0) — clamp instead
        // of wrapping.
        let retry_after = u64::try_from(new_tat - now - emission * burst).unwrap_or(u64::MAX);
        (false, retry_after, 0)
    }
}

/// In-memory backend backed by a `DashMap`.
///
/// Suitable for single-node deployments. Each key tracks its GCRA
/// theoretical arrival time (TAC) in monotonic milliseconds since process
/// start; the allow/deny/remaining decision is delegated to the verified
/// pure core [`gcra_decide`], matching the Redis GCRA backend's semantics.
#[cfg(feature = "in-memory")]
#[derive(Clone)]
pub struct InMemoryBackend {
    entries: dashmap::DashMap<String, Entry>,
}

#[cfg(feature = "in-memory")]
#[derive(Clone)]
struct Entry {
    /// GCRA theoretical arrival time (TAC) in monotonic ms. A fresh key
    /// starts with `tac == now`, i.e. a full burst budget.
    tac_ms: u64,
}

/// Monotonic milliseconds since process start (the GCRA clock domain).
///
/// Anchored once so that clock adjustments never move the TAC timeline.
#[cfg(feature = "in-memory")]
fn monotonic_ms() -> u64 {
    static ANCHOR: OnceLock<Instant> = OnceLock::new();
    let anchor = ANCHOR.get_or_init(Instant::now);
    anchor.elapsed().as_millis() as u64
}

#[cfg(feature = "in-memory")]
impl InMemoryBackend {
    /// Create a new in-memory backend.
    pub fn new() -> Self {
        Self {
            entries: dashmap::DashMap::new(),
        }
    }
}

#[cfg(feature = "in-memory")]
impl Default for InMemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "in-memory")]
#[async_trait::async_trait]
impl RateLimitBackend for InMemoryBackend {
    async fn check(&self, key: &str, quota: &Quota) -> RateLimitResult {
        let interval_ms = quota.interval().as_millis().max(1) as u64;
        let burst = u64::from(quota.burst);
        let now_ms = monotonic_ms();

        let mut entry = self
            .entries
            .entry(key.to_string())
            .or_insert_with(|| Entry { tac_ms: now_ms });

        // Verified GCRA decision core (see tests/kani.rs).
        let (allowed, retry_after_ms, remaining) =
            gcra_decide(now_ms, entry.tac_ms, interval_ms, burst);

        if allowed {
            // Advance the TAC exactly as `gcra_decide` computed it:
            // new_tat = max(tac, now) + emission.
            entry.tac_ms = entry.tac_ms.max(now_ms).saturating_add(interval_ms);
        }

        let reset_at = Instant::now() + quota.interval();

        #[cfg(feature = "metrics")]
        {
            // `remaining <= burst` is a Kani-verified property of
            // `gcra_decide`, so utilization is in [0, 1].
            let utilization = 1.0 - remaining as f64 / burst as f64;
            metrics::histogram!("ratelimit_utilization").record(utilization);
            if allowed {
                metrics::counter!("ratelimit_allowed_total", "key" => key.to_string()).increment(1);
            } else {
                metrics::counter!("ratelimit_rejected_total", "key" => key.to_string())
                    .increment(1);
            }
        }

        RateLimitResult {
            allowed,
            remaining,
            reset_at,
            limit: burst,
            retry_after: if allowed {
                None
            } else {
                Some(Duration::from_millis(retry_after_ms))
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcra_decide_fresh_key_allows_with_one_interval_budget() {
        // Fresh key (tac == now): the request itself is conforming, and
        // exactly one further interval of budget is visible.
        let (allowed, retry_after, remaining) = gcra_decide(1_000, 1_000, 100, 10);
        assert!(allowed);
        assert_eq!(retry_after, 0);
        assert_eq!(remaining, 1);
    }

    #[test]
    fn gcra_decide_burst_rapid_requests_then_deny() {
        // burst = 3: two more rapid requests conform after the first, the
        // fourth is denied with a positive retry_after and zero remaining.
        let (a1, r1, rem1) = gcra_decide(0, 0, 100, 3);
        assert!(a1 && r1 == 0 && rem1 == 1);
        let (a2, r2, rem2) = gcra_decide(0, 100, 100, 3);
        assert!(a2 && r2 == 0 && rem2 == 2);
        let (a3, r3, rem3) = gcra_decide(0, 200, 100, 3);
        assert!(a3 && r3 == 0 && rem3 == 3);
        let (a4, r4, rem4) = gcra_decide(0, 300, 100, 3);
        assert!(!a4 && r4 > 0 && rem4 == 0);
    }

    #[test]
    fn gcra_decide_zero_burst_never_allows() {
        let (allowed, retry_after, remaining) = gcra_decide(1_000, 1_000, 100, 0);
        assert!(!allowed);
        assert!(retry_after >= 1);
        assert_eq!(remaining, 0);
    }

    #[test]
    fn gcra_decide_zero_emission_is_clamped() {
        // Sub-millisecond quota: emission is clamped to 1 ms, no panic.
        let (allowed, retry_after, _remaining) = gcra_decide(5, 5, 0, 1);
        assert!(allowed);
        assert_eq!(retry_after, 0);
    }

    #[test]
    fn gcra_decide_extreme_inputs_no_overflow() {
        // All-u64 extremes: must not overflow or panic (u128 internals).
        // new_tat = 2*u64::MAX (as u128); emission*burst = u64::MAX^2, so
        // the request conforms and exactly 2 emission-intervals of budget
        // are visible (both burst and emission are u64::MAX here).
        let (allowed, retry_after, remaining) = gcra_decide(0, u64::MAX, u64::MAX, u64::MAX);
        assert!(allowed);
        assert_eq!(retry_after, 0);
        assert_eq!(remaining, 2);

        // now == tac == u64::MAX, emission 1 ms, burst 2: conforms with
        // exactly one 1-ms interval of budget visible.
        let (allowed2, retry_after2, remaining2) = gcra_decide(u64::MAX, u64::MAX, 1, 2);
        assert!(allowed2);
        assert_eq!(retry_after2, 0);
        assert_eq!(remaining2, 1);
    }
}
