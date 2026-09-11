# Performance SLOs — throttle-kit

Measured with criterion (`cargo bench --bench ratelimit_bench`) and
instruction counts (`perf stat`, min of 5 runs, identical A/B driver
binaries), 2026-09-11. Hardware: Intel(R) Core(TM) i5-9400F CPU @
2.90GHz, 6 cores, Linux x86_64. Criterion reports mean/median/stddev,
not percentiles; **P50 column = criterion mean** (P99 is not directly
measured; the CI bench job compares means against the saved `ci`
baseline).

**Measurement caveat (2026-09-11):** the shared dev box was heavily
loaded during this re-measurement (load average ~40 on 6 cores, other
release builds and benchmarks running concurrently), so wall-clock
numbers below are inflated and noisy; they are kept for the trend, and
the instruction counts — which are contention-immune and reproduced
within ±0.1% across runs — are the authoritative before/after evidence.
The saved criterion baseline `before-1.0.0` (1.0.0 code, same box) is
checked in via `target/criterion` for CI-side comparison.

## Measured (per warm-key check, in-memory GCRA backend)

| Metric | 1.0.0 (before) | 1.1.0 (after) | Delta |
|---|---|---|---|
| **Heap allocations per warm check** | **2** (key `String` + `async-trait` future box) | **0** (verified by counting allocator, `tests/zero_alloc_hot_path.rs`) | **−2** |
| **Instructions per warm check** (`perf stat`, A/B driver) | ~1010 | ~662 | **−34.5%** |
| `RateLimiter::check` criterion mean | 192–273 ns (noisy runs) | 318–422 ns (noisy runs; box saturated) | wall-clock unreliable this session; instruction counts authoritative |
| Clock reads per check | 2 (`monotonic_ms` + fresh `Instant::now` for `reset_at`) | 1 | −1 syscall |

The 137.5 ns single-key figure in the previous revision of this file was
measured on an idle machine; the current box could not reproduce idle
conditions. Scaling the verified −34.5% instruction reduction from the
137.5 ns baseline projects ≈ **90 ns** on idle hardware; the CI
criterion/iai jobs (idle runners) are the source of truth going forward.

## SLO statements

- `throttle_kit::RateLimiter::check` on the in-memory backend completes
  in **< 200 ns P50 for a warm key** on idle hardware (projected ≈ 90 ns
  from the verified instruction delta; re-baseline on CI runners).
- **Zero heap allocations on the warm-key check path** (counting
  allocator, `tests/zero_alloc_hot_path.rs`, runs on every `cargo test`).
  This was *claimed* in earlier revisions of this file and the
  `iai_hot_path` bench comments, but **was false in 1.0.0** (every call
  allocated the key string and an `async-trait` future box). Fixed and
  now enforced by test.
- Fresh-key checks (new DashMap entry) stay **< 250 ns P50** amortized.
- Identity resolution adds **< 150 ns P50** on the proxied path and
  **< 20 ns** on the default direct-exposure path (1.0.0 idle-box
  measurements: 134.6 ns / 8.3 ns; code unchanged in 1.1.0).
- `KeyedRateLimiter::check` performs **zero allocations on every
  repeated check** (warm path reads the override map by borrowed key and
  delegates straight to the shared backend; 1.0.0 allocated the key
  string plus a limiter clone on every call).

## Allocation profile (verified, not aspirational)

- **Warm key (steady state): 0 allocations.** The in-memory backend
  updates the existing DashMap entry via `get_mut(key)` on the borrowed
  key; the check future is a native async-fn-in-trait future (no box).
  Enforced by `tests/zero_alloc_hot_path.rs`.
- **Fresh key: ≥ 1 allocation** — the key is copied into the DashMap
  entry on its first-ever check (`key.to_owned()` on the cold path
  only). This is the key's only lifetime allocation.
- **Tower layer (allowed request): 1 heap allocation per client-IP
  request** — the three `X-RateLimit-*` header values must own their
  bytes (`HeaderValue` storage). The key itself is formatted into a
  45-byte stack buffer (no `String`), the per-request key-source clone is
  an `Arc` bump, and the future is still boxed (`Pin<Box<dyn Future>>` in
  the Service impl — unboxing is future work; 1.0.0 additionally paid
  1 key `String` + 3 `to_string().parse()` round-trip allocations, all
  removed). Custom key extractors return `String` by API contract.
- **`metrics` feature: label recording allocates** (dynamic `"key"`
  labels are stringified per call). The zero-allocation guarantee applies
  to the default feature set.
- **Single clock read per check**: `reset_at` is derived from the same
  monotonic anchor read as the GCRA decision (1.0.0 read the clock
  twice).
- **Resolution allocates nothing**: the right-to-left walk uses
  `split(',').rev().nth(hops)` over the header value in place; the
  resolved `IpAddr` is stringified once into stack space at the layer
  boundary.

## Regression policy

- Baselines are saved on main in CI by the shared bench job
  ([rust-kit.yml](https://github.com/WyattAu/engineering-standards/blob/main/.github/workflows/rust-kit.yml),
  `cargo bench -- --save-baseline ci`), non-gating (regression visibility).
- Local: `cargo bench --bench ratelimit_bench -- --save-baseline main`, compare
  with `-- --baseline main`.
- The iai-callgrind instruction gate (`benches/iai_hot_path.rs`) runs in
  CI only (`valgrind` not installed locally as of 2026-09-11); the
  zero-allocation property it implies is additionally pinned locally by
  `tests/zero_alloc_hot_path.rs`, which needs no valgrind.
- Alert threshold: >2× mean regression on `rate_limit_check_single_key`
  and on `client_ip_resolve_trusted_proxy` (the 0.4.0 identity path), and
  any allocation on the warm-key path (`tests/zero_alloc_hot_path.rs`).
