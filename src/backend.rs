#[cfg(feature = "in-memory")]
use std::sync::OnceLock;
#[cfg(feature = "in-memory")]
use std::time::{Duration, Instant};

use std::future::Future;

use crate::metrics::RateLimitResult;
use crate::quota::Quota;

/// Backend trait for rate limit state storage.
///
/// Uses native `async fn` in trait (MSRV 1.85) instead of
/// `#[async_trait]`, so a check dispatches no future boxing and no
/// vtable call. The returned future is required to be `Send` so
/// backends work on multi-threaded runtimes (the Tower layer's boxed
/// future, spawned tasks).
///
/// The trait is intentionally **not dyn-compatible**: every consumer in
/// this crate is generic over `B: RateLimitBackend`, keeping the hot
/// path statically dispatched. If you need heterogeneous backend sets,
/// wrap them in your own enum rather than a `dyn` trait object.
pub trait RateLimitBackend: Send + Sync + 'static {
    /// Check if a request for `key` is allowed under the given `quota`.
    fn check(&self, key: &str, quota: &Quota) -> impl Future<Output = RateLimitResult> + Send;
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

/// One GCRA step for a stored entry: decide via the verified pure core
/// [`gcra_decide`], then — exactly as it computed — advance the TAC to
/// `new_tat = max(tac, now) + emission` when the request conforms.
///
/// Shared by the warm-key (`get_mut`) and cold-key (`entry`) paths so
/// the two cannot drift.
#[cfg(feature = "in-memory")]
fn gcra_advance(entry: &mut Entry, now_ms: u64, interval_ms: u64, burst: u64) -> (bool, u64, u64) {
    // Verified GCRA decision core (see tests/kani.rs).
    let (allowed, retry_after_ms, remaining) =
        gcra_decide(now_ms, entry.tac_ms, interval_ms, burst);
    if allowed {
        entry.tac_ms = entry.tac_ms.max(now_ms).saturating_add(interval_ms);
    }
    (allowed, retry_after_ms, remaining)
}

/// Monotonic milliseconds since process start (the GCRA clock domain).
///
/// Anchored once so that clock adjustments never move the TAC timeline.
#[cfg(feature = "in-memory")]
fn monotonic_anchor() -> Instant {
    static ANCHOR: OnceLock<Instant> = OnceLock::new();
    *ANCHOR.get_or_init(Instant::now)
}

/// Monotonic milliseconds since process start (the GCRA clock domain).
#[cfg(feature = "in-memory")]
fn monotonic_ms() -> u64 {
    monotonic_anchor().elapsed().as_millis() as u64
}

/// A deadline `from_now_ms` after the shared monotonic anchor.
///
/// Same clock domain as [`monotonic_ms`], so `reset_at` is derived from
/// the single clock read of a check instead of a fresh `Instant::now()`.
#[cfg(feature = "in-memory")]
fn monotonic_deadline(from_now_ms: u64) -> Instant {
    monotonic_anchor()
        .checked_add(Duration::from_millis(from_now_ms))
        .unwrap_or_else(Instant::now)
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
impl RateLimitBackend for InMemoryBackend {
    async fn check(&self, key: &str, quota: &Quota) -> RateLimitResult {
        let interval_ms = quota.interval().as_millis().max(1) as u64;
        let burst = u64::from(quota.burst);
        let now_ms = monotonic_ms();

        // Warm-key fast path (steady-state traffic): update the existing
        // entry in place — zero allocation. Only a key's first-ever call
        // pays for the `to_owned()` insert on the cold path below.
        let (allowed, retry_after_ms, remaining) =
            if let Some(mut entry) = self.entries.get_mut(key) {
                gcra_advance(&mut entry, now_ms, interval_ms, burst)
            } else {
                let mut entry = self
                    .entries
                    .entry(key.to_owned())
                    .or_insert_with(|| Entry { tac_ms: now_ms });
                gcra_advance(&mut entry, now_ms, interval_ms, burst)
            };

        // Derived from the same clock read as the GCRA decision above —
        // a check performs exactly one clock read.
        let reset_at = monotonic_deadline(now_ms.saturating_add(interval_ms));

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

/// No-op backend: allows every request with the quota's full budget.
///
/// Intended for tests, as a feature-flag "kill switch" that never
/// throttles, or as a placeholder while wiring a real backend. Not for
/// production limiting — it enforces nothing.
///
/// # Examples
///
/// ```
/// use throttle_kit::{NoopBackend, Quota, RateLimiter};
///
/// # async fn run() {
/// let limiter = RateLimiter::new(Quota::per_second(10), NoopBackend);
///
/// // Never throttles: every check is allowed with a full budget.
/// assert!(limiter.check("anything").await.allowed);
/// assert!(limiter.check("anything").await.allowed);
/// # }
/// # run();
/// ```
#[cfg(feature = "test-util")]
#[cfg_attr(docsrs, doc(cfg(feature = "test-util")))]
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopBackend;

#[cfg(feature = "test-util")]
impl RateLimitBackend for NoopBackend {
    fn check(&self, _key: &str, quota: &Quota) -> impl Future<Output = RateLimitResult> + Send {
        std::future::ready(RateLimitResult {
            allowed: true,
            remaining: u64::from(quota.burst).saturating_sub(1),
            reset_at: std::time::Instant::now() + quota.interval(),
            limit: u64::from(quota.burst),
            retry_after: None,
        })
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
    fn gcra_decide_deny_with_now_greater_than_zero() {
        // A denial with now > 0 pins the retry_after subtraction chain:
        // new_tat = max(1300, 1000) + 100 = 1400, and the request conforms
        // only while new_tat <= now + emission * burst (1400 <= 1300 fails),
        // so retry_after must be exactly new_tat - now - emission * burst.
        let (allowed, retry_after, remaining) = gcra_decide(1_000, 1_300, 100, 3);
        assert!(!allowed);
        assert_eq!(retry_after, 100);
        assert_eq!(remaining, 0);
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

    #[cfg(feature = "in-memory")]
    #[tokio::test]
    async fn in_memory_backend_reallows_after_real_elapsed_time() {
        // The backend clock must advance with real time. Exhausting the
        // burst and sleeping past one interval must re-enable the key; a
        // constant-clock mutant never observes the sleep and stays denied.
        let backend = InMemoryBackend::new();
        let quota = Quota::per_second(10); // 100 ms interval, burst 10

        // Ten rapid conforming requests exhaust the burst budget.
        for i in 1..=10u64 {
            let r = backend.check("clock", &quota).await;
            assert!(r.allowed);
            assert_eq!(r.remaining, i);
        }
        assert!(!backend.check("clock", &quota).await.allowed);

        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(
            backend.check("clock", &quota).await.allowed,
            "key must re-conform after one full interval of real time"
        );
    }

    #[cfg(feature = "in-memory")]
    #[tokio::test]
    async fn in_memory_reset_at_is_in_the_future_when_allowed() {
        let backend = InMemoryBackend::new();
        let r = backend.check("reset", &Quota::per_second(10)).await;
        assert!(r.allowed);
        assert!(
            r.reset_at > std::time::Instant::now(),
            "reset_at must be in the future for an allowed request"
        );
    }

    #[cfg(feature = "in-memory")]
    #[tokio::test]
    async fn warm_key_fast_path_matches_cold_path_semantics() {
        // The borrowed-key fast path (`get_mut`) must behave exactly like
        // the cold path it replaced: burst exhaustion and TAC advance are
        // identical across the boundary, and no duplicate map entries are
        // created for repeated checks.
        let backend = InMemoryBackend::new();
        let quota = Quota::per_second(10); // 100 ms interval, burst 10

        // Call 1 takes the cold path (entry insert); calls 2..=10 take
        // the warm path. Remaining increments across the boundary exactly
        // as it did when every call went through `entry()`.
        for i in 1..=10u64 {
            let r = backend.check("hot", &quota).await;
            assert!(r.allowed, "call {i} must conform");
            assert_eq!(r.remaining, i);
        }
        // Burst is exhausted through the warm path.
        let denied = backend.check("hot", &quota).await;
        assert!(!denied.allowed);
        assert!(denied.retry_after.is_some());

        // Exactly one entry for the key — the fast path must not insert.
        assert_eq!(backend.entries.len(), 1);
        assert!(backend.entries.contains_key("hot"));
    }

    #[cfg(feature = "in-memory")]
    #[tokio::test]
    async fn warm_key_reset_at_derived_from_single_clock_read() {
        // reset_at is anchored to the check's own monotonic clock read:
        // read_time + interval, with read_time inside the call's wall
        // window. One clock read per check — no fresh Instant for the
        // deadline.
        let backend = InMemoryBackend::new();
        let quota = Quota::per_second(10); // 100 ms interval
        let before = std::time::Instant::now();
        let r = backend.check("clock-domain", &quota).await;
        let after = std::time::Instant::now();
        assert!(r.allowed);
        // The monotonic read is truncated to whole milliseconds, so the
        // deadline can sit up to 1 ms earlier than the untruncated read.
        assert!(
            r.reset_at + Duration::from_millis(1) >= before + quota.interval(),
            "reset_at = clock_read + interval, read no earlier than `before`"
        );
        assert!(
            r.reset_at <= after + quota.interval(),
            "reset_at = clock_read + interval, read no later than `after`"
        );
    }
}
