# PATTERN_ADOPTION: throttle-kit

Relationship to the WyattAu pattern canon. Statuses follow the closed
vocabulary: adopted, partial, planned, refused, n/a. Evidence paths are
relative to this repo.

| Pattern | Status | Evidence |
|---|---|---|
| P22 | n/a | GCRA is per-key static-rate conformance (src/gcra.rs:32, src/gcra.rs:57). The canon P22 pattern is weighted DRR pool scheduling against noisy-tenant starvation; no tenant-weighted queue exists in this crate, so the pattern never applies |
| T35 | n/a | Limits are fixed parameters derived from a configured rate and burst (src/gcra.rs:51-57). Nothing adapts limits to measured load or latency, so the adaptive-concurrency pattern does not apply |
