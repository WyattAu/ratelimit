# Requirements: throttle-kit
| REQ-THROTTLE-102 | A malformed, missing, non-ASCII, or too-short forwarded header (chain ≤ `num_trusted_hops`) falls back to the peer IP and emits a `tracing::warn!`; it never panics and never walks further left | MUST |
  proptest + 2 doc); no client-IP coverage; `extract_key` (former