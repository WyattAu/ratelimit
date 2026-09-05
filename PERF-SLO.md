# Performance SLOs — throttle-kit

Measured with criterion (`cargo bench --bench ratelimit_bench`), 2026-09.
Hardware: Intel(R) Core(TM) i5-9400F CPU @ 2.90GHz, 6 cores, Linux x86_64.
Criterion reports mean/median/stddev, not percentiles; **P50 column = criterion
mean** (P99 is not directly measured; the CI bench job compares means against
the saved `ci` baseline).

## Measured (mean per operation)

| Benchmark | P50 (mean) | Notes |
|---|---|---|
| `RateLimiter::check`, single reused key | **137.5 ns** (re-measured 144.0 ns, 2026-09-05) | in-memory GCRA backend, allowed; check path unchanged by 0.4.0 |
| `check`, 1000 distinct keys | 200.4 ns/key (re-measured 223 ns/key) | includes `format!` key construction |
| `check`, 4 threads contention | 2.03 µs/iter (re-measured 1.12 µs) | distinct keys per thread |
| `client_ip::resolve_client_identity`, trusted peer + 2-entry XFF | **134.6 ns** | NEW in 0.4.0: header lookup, right-to-left hop walk, parse, no allocation (added in front of `check` for proxied deployments) |
| `client_ip::resolve_client_identity`, untrusted peer | **8.3 ns** | NEW in 0.4.0: default (secure-by-default) path — empty/unmatched `trusted_proxies`, header never consulted |

## SLO statements

- `throttle_kit::RateLimiter::check` on the in-memory backend completes in
  **< 200 ns P50 for a warm key** (measured 137.5 ns, 2026-09, 6-core
  x86_64).
- Fresh-key checks (new DashMap entry) stay **< 250 ns P50** amortized.
- Identity resolution adds **< 150 ns P50** on the proxied path (measured
  134.6 ns) and **< 20 ns** on the default direct-exposure path (measured
  8.3 ns). End-to-end layer cost for the common proxied deployment is
  therefore ≈ 137.5 + 134.6 ≈ **272 ns** per request (plus one `String`
  allocation for the key, as before).

## Allocation profile (from code reading)

- **1 allocation per check on a fresh key**: the key is copied into the
  DashMap entry (`key.to_string()`). Steady-state checks on an existing key
  reuse the stored key; remaining per-op cost is a quota-clone and `Arc`
  refcount bumps (no heap allocation). Not yet verified with a counting
  allocator — the breaker probe (see its PERF-SLO.md) demonstrates the
  method; same treatment planned.
- **Resolution allocates nothing**: the right-to-left walk uses
  `split(',').rev().nth(hops)` over the header value in place; the
  resolved `IpAddr` is only stringified once, at the layer boundary.

## Regression policy

- Baselines are saved on main in CI by the shared bench job
  ([rust-kit.yml](https://github.com/WyattAu/engineering-standards/blob/main/.github/workflows/rust-kit.yml),
  `cargo bench -- --save-baseline ci`), non-gating (regression visibility).
- Local: `cargo bench --bench ratelimit_bench -- --save-baseline main`, compare
  with `-- --baseline main`.
- Alert threshold: >2× mean regression on `rate_limit_check_single_key`
  and on `client_ip_resolve_trusted_proxy` (the 0.4.0 identity path).
