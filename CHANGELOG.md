Changelog](https://keepachangelog.com/); versions follow [semver](https://semver.org).
  `impl Future<Output = RateLimitResult> + Send`; no future boxing and
  `copy_impl_added`; non-move closures now capture a `Quota` by
- ci: perf-gate now installs `valgrind` explicitly; recent
- `tests/config_matrix.rs`; per-knob behavior matrix for all 4 knob
  instead of ~333 ms; clients retrying on `retry_after` hammered Redis.
  burst 20 admit exactly 20; the Lua decision loop holds), key
  GCRA backend previously allocated on *every* check; the key was
  per call; despite PERF-SLO.md claiming a zero-alloc steady state. The
  service future is still boxed; unboxing it is future work.
  or future work; including the still-boxed Tower service future and
- First stable release; API stabilization of the GCRA rate limiter:
- Pinned proptest max_global_rejects in code; assume-throttle abort no longer masquerades as a test failure in CI/release gates
- **Directly exposed?** Nothing to do; the default (socket address,
- GCRA algorithm; smooth, memory-efficient rate limiting.
- ClientIpConfig trusted-proxy identity (0.4.0); production-proven with adversarial property + fuzz suites