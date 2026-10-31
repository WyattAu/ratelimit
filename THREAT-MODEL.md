# Threat Model: throttle-kit
- **CLOSED-1 (was OPEN-1); `extract_key` trusted `X-Forwarded-For`
  set, minting a fresh GCRA budget per request; a complete bypass when
  directly internet-facing; and herded all XFF-less clients into one
## OPEN RISKS (missing mitigations: not fabricated)
- **OPEN-2; in-memory per-key state is never evicted.** Keys are
  key inserts a `DashMap` entry that lives forever; memory-exhaustion DoS.
- **OPEN-3; Redis backend fails OPEN on any backend error**
  (`src/redis.rs`: "On Redis errors, fail open; allow the request"). A
- **OPEN-4; clock source is per-call `SystemTime::now()`.** Redis and
- **OPEN-5; two algorithms with different semantics.** The `sliding-window`
  bucket by design (opt-in); operators should prefer `Reject`.
- `check_sync` builds a fresh current-thread Tokio runtime per call -