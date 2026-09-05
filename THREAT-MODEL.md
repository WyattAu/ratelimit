# Threat Model — throttle-kit

Status: **v1.0** · Method: STRIDE over the public API surface
(`RateLimiter`/`KeyedRateLimiter`, `Quota`, `InMemoryBackend`,
`RedisBackend` (GCRA Lua), `SqliteBackend`, `SlidingWindowBackend`,
`RateLimitLayer`/`RateLimitService`).

Trust boundaries: (1) rate-limit keys derived from attacker-controlled
requests (headers, IPs, API keys), (2) the shared backend state (process
map / Redis / SQLite), (3) the Tower layer's request pipeline.

## Assets

| ID | Asset | Example |
|----|-------|---------|
| A1 | Limit enforcement | Attacker evades the quota entirely |
| A2 | Correctness across callers | One caller's burst consumes another's budget |
| A3 | Process/Redis availability | Unbounded per-key state or Redis outage takes down auth paths |
| A4 | Header accuracy | `Retry-After`/`X-RateLimit-*` mislead clients or proxies |

## STRIDE Analysis

| # | Threat | Category | Surface | Mitigation | Verifying test |
|---|--------|----------|---------|------------|----------------|
| T1 | Cross-key budget interference | Elevation | `InMemoryBackend::check` | Per-key `Entry { last_check, remaining }` in `DashMap`; GCRA replenishment computed per entry | `tests/integration.rs::backend_keys_are_isolated` |
| T2 | Quota bypass via concurrent requests (TOCTOU) | Elevation | `InMemoryBackend` | DashMap entry lock serializes check-and-decrement per key | `tests/integration.rs::backend_exhausts_quota`, `sync_check_exhaustion` |
| T3 | Redis multi-instance race (two nodes, one budget) | Elevation | `RedisBackend::gcra_check` | GCRA implemented as an atomic Lua script (single Redis transaction) | `src/redis.rs::GCRA_SCRIPT` (script contract); backend tests cover the in-memory path |
| T4 | Malformed key strings / hostile headers crash the limiter | DoS | `check`, `extract_key` | Keys are opaque `&str` map keys; header values parsed with `to_str().ok()` fallbacks; `#![forbid(unsafe_code)]` | `tests/integration.rs::backend_new_and_default`, `error_backend_error_display`; proptest quota/result invariants (`tests/proptest.rs`: `quota_interval_always_positive`, `rate_limit_result_headers_always_3`) |
| T5 | Over-limit request forwarded (layer failure) | Elevation | `RateLimitService::call` | Disallowed → immediate `429` + `retry-after: 1`, inner service never called | `tests/integration.rs::result_denied_with_retry_after`, `result_headers_contain_correct_keys` |
| T6 | Unbounded token replenishment overflow | Tampering | GCRA replenish math | Replenished tokens capped at `burst` via `.min(burst)` | `tests/proptest.rs::quota_burst_matches_rate`, `backend_exhausts_quota` |

## OPEN RISKS (missing mitigations — not fabricated)

- **OPEN-1 — `extract_key` trusts `X-Forwarded-For` blindly.**
  `RateLimitService` keys on the first XFF value, which any client can set,
  yielding a fresh budget per request — a complete GCRA bypass when the
  service is directly internet-facing. Fallback key is the constant
  `"unknown"`, which herds *all* XFF-less clients into one shared bucket
  (both bypass and self-DoS in one function). No test covers
  `extract_key` behavior.
- **OPEN-2 — in-memory per-key state is never evicted.** Keys are
  attacker-supplied in the common deployment (IP/tenant id); every unique
  key inserts a `DashMap` entry that lives forever — memory-exhaustion DoS.
  No GC, TTL, or capacity bound, and no test.
- **OPEN-3 — Redis backend fails OPEN on any backend error**
  (`src/redis.rs`: "On Redis errors, fail open — allow the request"). A
  Redis outage disables rate limiting silently (`eprintln!` only). This is a
  documented availability-over-enforcement trade-off, but it is not
  configurable and not surfaced to metrics.
- **OPEN-4 — clock source is per-call `SystemTime::now()`.** Redis and
  in-memory backends sample wall time at check time; a caller cannot inject
  a clock, so wall-clock jumps (NTP steps) perturb windows. Determinism
  tests (e.g. webauthn-kit's `with_clock` pattern) are absent.
- **OPEN-5 — two algorithms with different semantics.** The `sliding-window`
  feature adds `SlidingWindowBackend` alongside GCRA; a caller swapping
  backends changes burst semantics silently. No conformance test asserts
  both backends honor the same `Quota` contract.

## Out of Scope

- Distributed deployment of the in-memory backend (single-process only).
- Key derivation policy (which header/IP identifies a caller) — integrator
  choice, though OPEN-1 shows the default is dangerous.
- Abuse of un-limited routes (the layer must be installed to protect).

## Residual Risks

- `check_sync` builds a fresh current-thread Tokio runtime per call —
  correctness is fine, but sync callers are slow, tempting them to
  hand-roll checks (bypassing the limiter).
- GCRA "cost" is fixed at 1 in `RedisBackend::check`; weighted requests
  need caller-side key multiplexing.
