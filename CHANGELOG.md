# Changelog

All notable changes to this project are documented here. Format: [Keep a
Changelog](https://keepachangelog.com/) — versions follow [semver](https://semver.org).

## [Unreleased]

## [1.1.2] - 2026-09-12

### Added

- `tests/config_matrix.rs` — per-knob behavior matrix for all 4 knob
  groups (quota interval recovery timing, burst budget, client-IP trust
  switching incl. hops/header override, missing-client policy +
  key-extractor, per-key overrides). Deep edges were verified already
  covered in `tests/client_ip.rs` / `tests/integration.rs`; no dead
  knobs found.

## [1.1.1] - 2026-09-12

### Fixed

- **Redis GCRA `retry_after` was 1000× too small.** The Lua script
  computed the retry window in seconds (`/ 1000`) while the Rust side
  interpreted the return as milliseconds (`Duration::from_millis`), so a
  denial after exhausting a 3/second quota claimed a ~0.3 ms window
  instead of ~333 ms — clients retrying on `retry_after` hammered Redis.
  The script now returns milliseconds to match the documented contract.
  Caught by the new live integration suite.

### Added

- Live Redis GCRA integration suite (`tests/redis_gcra_live.rs`,
  fixture-gated with `#[ignore]` following the estate's docker-gated
  pattern): sequential allowance sequences with exact `remaining`
  countdown and positive `retry_after` on denial, denial clearing after
  the retry window, concurrency atomicity (40 concurrent checks against
  burst 20 admit exactly 20 — the Lua decision loop holds), key
  independence, and the fail-open contract on Redis command failures.
  Run with `docker run -p 6379:6379 redis:7` and
  `cargo test --features redis --test redis_gcra_live -- --ignored`.

### CI

- New `integration` job providing a `redis` service and running the
  fixture-gated suite plus the self-contained fail-open test.

## [1.1.0] - 2026-09-11

### Changed

- **Performance: zero-allocation warm-key check path.** The in-memory
  GCRA backend previously allocated on *every* check — the key was
  stringified for the DashMap lookup and `#[async_trait]` boxed a future
  per call — despite PERF-SLO.md claiming a zero-alloc steady state. The
  claim was false; 1.0.0 measured **2 heap allocations per warm check**.
  Now: borrowed-key fast path (`get_mut` on `&str`, cold-path insert only
  on a key's first-ever check) and native async-fn-in-trait (MSRV 1.85)
  replace `#[async_trait]`, removing the dependency. Verified: **0
  allocations per warm check** and **−34.5% instructions per check**
  (~1010 → ~662, `perf stat` A/B, min of 5), enforced by a new counting
  allocator test (`tests/zero_alloc_hot_path.rs`).
- **Performance: single clock read per check.** `reset_at` is now derived
  from the same monotonic anchor read as the GCRA decision instead of a
  second fresh `Instant::now()`.
- **Performance: `KeyedRateLimiter::check` no longer allocates or clones
  on the hot path.** 1.0.0 stringified the key twice, cloned a
  `RateLimiter` (Arc + quota) and held a DashMap guard per call; it now
  reads per-key quota overrides by borrowed key and delegates straight
  to the shared backend. Default-quota keys are never materialized in
  the override map. The `B: Clone` bound was dropped.
- **Performance: Tower layer trims.** Client-IP keys are formatted into
  a 45-byte stack buffer instead of a per-request `String`; the three
  `X-RateLimit-*` header values are built from stack-formatted digits
  via `HeaderValue::from_bytes` instead of `to_string().parse()`
  round-trips; the per-request key-source clone is now an `Arc` bump
  (1.0.0 copied the trusted-proxy CIDR list on every request). The
  service future is still boxed — unboxing it is future work.
- `Quota` now implements `Copy` (was `Clone` only).
- `RateLimiter<B>` no longer requires `B: Clone` to be `Clone`.

### Fixed

- **PERF-SLO.md honesty correction:** the "zero-alloc steady state" claim
  (PERF-SLO.md and `benches/iai_hot_path.rs` comments) did not match
  measured reality in 1.0.0. The allocation profile is now verified by a
  counting-allocator test and the docs state what is measured, projected,
  or future work — including the still-boxed Tower service future and
  the allocating `metrics`-feature labels.

### Added

- `NoopBackend` behind the new `test-util` feature: an always-allow
  backend for tests and feature-flag kill switches (previously only a
  test-local helper).

## [1.0.0] - 2026-09-05

### Added
- First stable release — API stabilization of the GCRA rate limiter:
  Axum/Tower layer, `client_ip` identity resolution, and the
  in-memory/SQLite/Redis backends.

## [0.4.1] - 2026-09-05

### Fixed
- Pinned proptest max_global_rejects in code — assume-throttle abort no longer masquerades as a test failure in CI/release gates

## [0.4.0] - 2026-09-05

### Security

- **Breaking:** the Tower layer no longer trusts `X-Forwarded-For`
  unconditionally. Client identity is now resolved by
  `client_ip::resolve_client_identity` (REQ-THROTTLE-100..104):
  forwarded headers are believed only for peers listed in
  `ClientIpConfig::trusted_proxies` (empty default = headers ignored,
  secure by default), via a right-to-left walk skipping
  `num_trusted_hops` entries; malformed/too-short headers fall back to
  the peer IP; requests without `ConnectInfo` fail closed with `503`
  (`MissingClientPolicy::Reject`) unless `FallbackKey` is opted into.
  Closes threat-model OPEN-1 (fresh GCRA budget per request via a
  client-set header). Requires axum routers to be served with
  `.into_make_service_with_connect_info::<SocketAddr>()`.
- Added `MissingClientPolicy` and a `KeyExtractor` override
  (`.with_key_extractor`) for non-IP keys (API keys, tenant ids).

### MIGRATION

- **Behind a proxy?** You must now configure `trusted_proxies` (CIDR
  list of your own proxies) and `num_trusted_hops` (how many rightmost
  XFF entries your infrastructure appends; default 1 fits
  `internet → ALB → nginx → app`, use 0 for a single direct proxy).
  See README "Behind a proxy".
- **Directly exposed?** Nothing to do — the default (socket address,
  headers ignored) is stricter and safe.
- Keying by API key? Switch from the implicit behavior to
  `RateLimitLayer::with_key_extractor`.

### Added

- `client_ip` module: `ClientIpConfig`, `MissingClientPolicy`, `IpNet`
  (dependency-free CIDR), `resolve_client_identity`; unit, boundary,
  property (independent-oracle), and fuzz coverage
  (`fuzz/fuzz_targets/fuzz_client_ip.rs`).
- REQUIREMENTS.md with REQ-THROTTLE-100..104 traceability.

## [0.3.0] - 2026-09-02

### Added

- SQLite-backed GCRA rate limiter (distributed-friendly persistence
  without Redis).

### Changed

- Performance: fewer clones on the check path.

## [0.2.0] - 2026-09-02

### Added

- Keyed multi-client limiting and a synchronous API.
- `SlidingWindowBackend` behind a feature flag.
- `Quota::from_parts` constructor for custom interval/burst.
- `metrics` feature flag: utilization histogram for rate limit checks.

## [0.1.0] - 2026-09-01

### Added

- GCRA algorithm — smooth, memory-efficient rate limiting.
- In-memory backend via `DashMap` (default) and optional Redis backend
  for distributed deployments.
- Tower `Layer` with `X-RateLimit-*` response headers.
- Configurable burst / token-bucket capacity and presets.

## [1.0.0] - 2026-09-05

### Added
- API declared stable; semver contract enforced via cargo-semver-checks CI gate
- ClientIpConfig trusted-proxy identity (0.4.0) — production-proven with adversarial property + fuzz suites

### Fixed
- Proptest global-reject config pinned (CI flake)
- InMemoryBackend correctly feature-gated
