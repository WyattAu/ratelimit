# throttle-kit vs governor: head-to-head comparison
quota; 1000 requests/second, burst 100; measured with criterion in a
governor is measured through `RateLimiter::direct(..).check()`; the
rustc 1.94.1, criterion 0.5, governor 0.10.4 (jitter feature off; it does
  sync-only contexts, not a hot path; do not use it per-request. The async
| Extra crates.io features (0.10.4)  |;                         | dashmap, jitter, quanta, std, no_std (no shuttle feature in this release) |