Rate limiting for Rust; **GCRA** (Generic Cell Rate Algorithm) with pluggable
| `in-memory` | ✅ | `InMemoryBackend`; GCRA state in a `DashMap`, one `u64` TAC per key. |
| `sliding-window` | - | `SlidingWindowBackend`; alternative sliding-window counting backend. |
| `redis` | - | `RedisBackend` for distributed, multi-instance rate limiting. |
| `sqlite` | - | `SqliteBackend` for durable local limits. |
| `tower` | - | `RateLimitLayer` with `X-RateLimit-*` headers and `client_ip` identity resolution. |
| `metrics` | - | Record per-check results via the [`metrics`](https://docs.rs/metrics) facade. |
| `test-util` | - | `NoopBackend`; always-allow backend for tests and kill switches. |
- **GCRA algorithm**; smooth, memory-efficient rate limiting
emission time; one `Instant` per key.
ignores `X-Forwarded-For` entirely (secure by default; a client-set XFF
if your service is directly exposed, you need nothing; the default is