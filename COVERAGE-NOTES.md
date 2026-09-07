# Coverage Notes — ratelimit (throttle-kit)

## Measurement

```
cargo llvm-cov --summary-only --all-features
```

The 1.0-time audit recorded this crate as **FAIL — measurement blocked** by a failing
proptest (`prop_adversarial_chain_matches_oracle`, tests/client_ip.rs). That blocker is
resolved in-code:

- The X-Forwarded-For trust-walk hardening (commit `65845e7`, REQ-THROTTLE-100..104)
  fixed the disagreement the property found.
- The property's config now sets `max_global_rejects: 20000` in-code
  (tests/client_ip.rs), so the adversarial generator is no longer starved by the
  proptest default reject budget.
- The suite is green on a clean checkout with **no special environment variables**;
  the earlier `PROPTEST_MAX_GLOBAL_REJECTS=20000` workaround is no longer required.

## Known exception: `src/redis.rs` GCRA execution paths

The Redis backend's script-execution paths (`gcra_check`,
`RateLimitBackend::check`) require a **live Redis server**; no Redis service
container exists in CI, so those lines are structurally unexecutable under
host-only coverage measurement. The construction/connection **error** paths are
covered by `tests/redis_error_paths.rs` (malformed URL, unreachable server).
To lift the exception, add a Redis service to CI and an integration test
gated on `REDIS_URL` (skipped when unset), following the standard
integration-test pattern.

## `#[cfg]` note

`--all-features` unions every feature, so all cfg-gated modules (redis, sqlite,
tower, sliding-window) are compiled and counted in the denominator. Feature-specific
backends are each covered by their own test targets under `tests/`.
