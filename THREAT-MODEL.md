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
| T4 | Malformed key strings / hostile headers crash the limiter | DoS | `check`, `resolve_client_identity` | Keys are opaque `&str` map keys; forwarded-header values parsed with `to_str().ok()` fallbacks; resolver never panics (proptest + fuzz); `#![forbid(unsafe_code)]` | `tests/client_ip.rs::req102_*`, `prop_resolution_never_panics`; `fuzz/fuzz_targets/fuzz_client_ip.rs` |
| T5 | Over-limit request forwarded (layer failure) | Elevation | `RateLimitService::call` | Disallowed → immediate `429` + `retry-after: 1`, inner service never called | `tests/integration.rs::result_denied_with_retry_after`, `result_headers_contain_correct_keys` |
| T6 | Unbounded token replenishment overflow | Tampering | GCRA replenish math | Replenished tokens capped at `burst` via `.min(burst)` | `tests/proptest.rs::quota_burst_matches_rate`, `backend_exhausts_quota` |

## CLOSED RISKS

- **CLOSED-1 (was OPEN-1) — `extract_key` trusted `X-Forwarded-For`
  blindly.** 0.3.0 keyed on the first XFF value, which any client could
  set, minting a fresh GCRA budget per request — a complete bypass when
  directly internet-facing — and herded all XFF-less clients into one
  `"unknown"` bucket.
  **Mitigation (0.4.0):** client identity goes through
  `client_ip::resolve_client_identity` (REQ-THROTTLE-100..104):
  forwarded headers are believed only for peers in
  `ClientIpConfig::trusted_proxies` (**empty default = headers ignored
  entirely**); trusted peers resolve via a right-to-left walk minus
  `num_trusted_hops`; malformed/too-short headers fall back to the peer
  IP; no `ConnectInfo` → fail-closed `503` unless `FallbackKey` is
  opted into. The `"unknown"` herd bucket is gone.
  **Verifying tests:** `tests/client_ip.rs::req100_untrusted_peer_ignores_spoofed_xff`,
  `req100_default_config_ignores_headers`,
  `req101_right_to_left_walk_skips_hops`,
  `req102_malformed_chosen_entry_falls_back_no_leftward_walk` (and the
  other REQ-THROTTLE-10x tests), service-level
  `layer_default_keys_by_peer_and_ignores_spoofed_xff`,
  `prop_adversarial_chain_matches_oracle` (independent oracle),
  `prop_untrusted_peer_always_peer_socket`, and fuzz target
  `fuzz/fuzz_targets/fuzz_client_ip.rs`.

## OPEN RISKS (missing mitigations — not fabricated)

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
- Abuse of un-limited routes (the layer must be installed to protect).

## Residual Risks

- **Over-broad `trusted_proxies` misconfiguration (post-CLOSED-1).** The
  resolver is only as trustworthy as its configuration: an operator who
  lists, say, `0.0.0.0/0` as a trusted proxy (or sets
  `num_trusted_hops` higher than their real proxy chain) re-opens the
  spoofing hole through the configuration, not the code. The mechanism
  cannot distinguish a deliberate CDN deployment from a mistake; the
  module docs and README give worked examples for the common topologies
  and warn to never trust more hops than your infrastructure appends.
  `MissingClientPolicy::FallbackKey` herds unresolvable clients into one
  bucket by design (opt-in) — operators should prefer `Reject`.
- `check_sync` builds a fresh current-thread Tokio runtime per call —
  correctness is fine, but sync callers are slow, tempting them to
  hand-roll checks (bypassing the limiter).
- GCRA "cost" is fixed at 1 in `RedisBackend::check`; weighted requests
  need caller-side key multiplexing.
