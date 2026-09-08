# throttle-kit vs governor — head-to-head comparison

governor (crates.io: `governor`) is the market-leading Rust rate-limiting
crate (75M+ downloads, GCRA, same algorithm as throttle-kit's in-memory and
Redis backends). This page compares them honestly: measured numbers first,
then features.

Reproduce:

```sh
cargo bench --bench head_to_head
```

## Benchmark: single-key allow/deny decision

Same operation on both sides: the GCRA decision for one key under the same
quota — 1000 requests/second, burst 100 — measured with criterion in a
sustained loop. After the initial burst both limiters settle into the
rate-exceeded decision branch; that branch is identical arithmetic on both
sides (compute TAT, compare, return), so the comparison is apples-to-apples.
governor is measured through `RateLimiter::direct(..).check()` — the
cheapest synchronous path it offers.

| benchmark                     | median time |
|-------------------------------|-------------|
| `governor_check_direct`       | ~9 ns       |
| `throttle_kit_check_async`    | ~148 ns     |
| `throttle_kit_check_sync`     | ~1.03 µs    |

Hardware: Intel Core i5-9400F @ 2.90GHz (6 cores), Linux x86_64,
rustc 1.94.1, criterion 0.5, governor 0.10.4 (jitter feature off — it does
not affect `check()`), throttle-kit 1.0.0 with default features.

Reading the numbers honestly:

- governor's in-memory decision path is ~17x faster than throttle-kit's
  async path. governor uses a TSC-based clock (quanta) and a lean single
  allocation-free decision; throttle-kit reads the wall clock twice, builds a
  full `RateLimitResult` (remaining/reset_at/limit/retry_after), and hashes
  into a `DashMap` keyed by an owned `String`.
- `throttle_kit_check_sync` builds a fresh tokio runtime **per call**
  (`RateLimiter::check_sync`, src/lib.rs). That is a convenience API for
  sync-only contexts, not a hot path — do not use it per-request. The async
  `check` number is the honest hot-path comparison.
- A ~148 ns limiter adds ~1.5 ms of overhead per 10k checks; for most
  services the network dominates by 3-5 orders of magnitude. If you need
  millions of decisions per second per core in-process, governor wins.

## Feature matrix

|                                    | throttle-kit 1.0          | governor 0.10.4                  |
|------------------------------------|---------------------------|----------------------------------|
| Algorithm                          | GCRA (+ optional fixed/cleanup sliding window) | GCRA |
| In-memory backend                  | Yes                       | Yes                              |
| Distributed backend (Redis GCRA)   | Yes                       | No                               |
| SQLite backend                     | Yes                       | No                               |
| Async API                          | Yes (tokio)               | Yes (`until_ready`, futures-timer) |
| Sync API                           | Yes (`check_sync`)        | Yes (`check`)                    |
| Per-key quotas                     | Yes (`with_quota_for_key`) | Single quota per limiter        |
| Tower layer                        | Built-in optional feature | Separate crate (`tower_governor`) |
| Built-in metrics hooks             | Yes (`metrics` feature)   | No                               |
| Testable/simulated clock           | No                        | Yes (`FakeRelativeClock`, `NoOpMiddleware`) |
| Clock override                     | No                        | Yes (`Clock` trait)              |
| Built-in jitter                    | No                        | Yes (`jitter` feature)           |
| `no_std`                           | No (tokio-based)          | Yes (`no_std` feature)           |
| Verified decision core             | Yes (Kani proofs, `tests/kani.rs`) | No                      |
| Extra crates.io features (0.10.4)  | —                         | dashmap, jitter, quanta, std, no_std (no shuttle feature in this release) |

## Positioning

governor is the better choice when you want the fastest possible in-process
GCRA decisions, `no_std` support, a simulated clock for deterministic tests,
or you value a huge user base and long maintenance history.

throttle-kit is the better choice when the limiter must be shared across
processes or persist state (Redis GCRA, SQLite), when you need per-key quota
overrides, a Tower layer with HTTP/proxy awareness, built-in metrics, or a
formally verified decision core.

The honest headline: for a single-node in-memory limiter, governor is
faster and more mature; throttle-kit's reason to exist is the backend,
integration, and verification surface around the same algorithm.
