# ratelimit

Rate limiting for Rust — **GCRA** (Generic Cell Rate Algorithm) with pluggable
backends (in-memory, Redis) and optional
[Tower](https://docs.rs/tower) layer integration.

[![docs.rs](https://docs.rs/throttle-kit/badge.svg)](https://docs.rs/throttle-kit)
[![CI](https://github.com/WyattAu/ratelimit/actions/workflows/ci.yml/badge.svg)](https://github.com/WyattAu/ratelimit/actions)
[![crates.io](https://img.shields.io/crates/v/throttle-kit)](https://crates.io/crates/throttle-kit)
[![license](https://img.shields.io/crates/l/throttle-kit)](LICENSE-MIT)

## Feature Flags

| Feature | Default | Description |
|---|---|---|
| `in-memory` | ✅ | `InMemoryBackend` — GCRA state in a `DashMap`, one `u64` TAC per key. |
| `sliding-window` | — | `SlidingWindowBackend` — alternative sliding-window counting backend. |
| `redis` | — | `RedisBackend` for distributed, multi-instance rate limiting. |
| `sqlite` | — | `SqliteBackend` for durable local limits. |
| `tower` | — | `RateLimitLayer` with `X-RateLimit-*` headers and `client_ip` identity resolution. |
| `metrics` | — | Record per-check results via the [`metrics`](https://docs.rs/metrics) facade. |
| `test-util` | — | `NoopBackend` — always-allow backend for tests and kill switches. |

## Features

- **GCRA algorithm** — smooth, memory-efficient rate limiting
- In-memory backend via `DashMap` (default)
- Optional Redis backend for distributed deployments
- Tower `Layer` with `X-RateLimit-*` response headers
- Configurable burst / token-bucket capacity

## What is GCRA?

The Generic Cell Rate Algorithm treats each key as a leaky bucket. A request
is allowed if the bucket has capacity; otherwise it is rejected. Tokens are
replenished at a constant rate derived from the desired RPS/RPM.

```
allowed if:  ema ≤ limit
where ema(t) = max(0, ema(t-1) - (t - t_last)) + 1
```

This yields the same behaviour as a token bucket but only stores the last
emission time — one `Instant` per key.

## Quick Start

```rust
use ratelimit::{RateLimiter, Quota, InMemoryBackend};

#[tokio::main]
async fn main() {
    let backend = InMemoryBackend::new();
    let limiter = RateLimiter::new(Quota::per_second(100), backend);

    let result = limiter.check("api-key-abc").await;
    assert!(result.allowed);
    println!("remaining: {}", result.remaining);
}
```

## Presets

| Constructor         | Rate          | Burst |
|---------------------|---------------|-------|
| `Quota::per_second` | n / s         | n     |
| `Quota::per_minute` | n / 60 s      | n     |
| `Quota::per_hour`   | n / 3600 s    | n     |

Override burst with `.allow_burst(n)`.

## Tower Integration

By default the layer keys requests by the **client's socket address** and
ignores `X-Forwarded-For` entirely (secure by default — a client-set XFF
header cannot mint fresh budgets).

```rust,ignore
use ratelimit::{RateLimiter, Quota, InMemoryBackend};
use ratelimit::RateLimitLayer;
use axum::{Router, extract::ConnectInfo, routing::get};
use std::net::SocketAddr;

let backend = InMemoryBackend::new();
let layer = RateLimitLayer::new(Quota::per_second(50), backend);

let app = Router::new()
    .route("/", get(handler))
    .layer(layer);

// Serve with connect info so the layer can see the peer address.
// Without it, requests are rejected with 503 (fail closed) unless a
// MissingClientPolicy::FallbackKey is configured.
let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
axum::serve(
    listener,
    app.into_make_service_with_connect_info::<SocketAddr>(),
).await?;
```

### Behind a proxy

If the service sits behind proxies you control, tell the layer which
peers it may believe, and how many header entries *your* infrastructure
appends in front of the direct peer (`num_trusted_hops`):

```rust,ignore
use ratelimit::client_ip::{ClientIpConfig, IpNet};

// internet → ALB (trusted) → nginx (trusted, direct peer) → app
// nginx appends the ALB's address, so XFF is "client, alb-ip";
// the default num_trusted_hops = 1 skips "alb-ip" and yields "client".
let layer = RateLimitLayer::new(Quota::per_second(50), backend)
    .with_client_ip(ClientIpConfig {
        trusted_proxies: vec![IpNet::parse("10.0.0.0/8").unwrap()],
        num_trusted_hops: 1,                       // default
        trusted_header: None,                      // default: X-Forwarded-For
    });
```

Resolution walks the header RIGHT-TO-LEFT and never trusts entries left
of the resolved one, so client-spoofed XFF values are ignored. For a
single `internet → nginx → app` proxy, set `num_trusted_hops: 0`.
Platforms with a dedicated header (`CF-Connecting-IP`) can set
`trusted_header`; same trust rules apply.

Migrating from 0.3.0: if you were behind a proxy, you **must** configure
`trusted_proxies` (the old trust-XFF-unconditionally behavior is gone);
if your service is directly exposed, you need nothing — the default is
stricter and safe. Callers keying by API keys instead of IP can use
`.with_key_extractor(...)`.

## Comparison with governor

|                  | ratelimit               | governor                        |
|------------------|-------------------------|---------------------------------|
| Algorithm        | GCRA (leaky bucket)     | GCRA (leaky bucket)             |
| Backends         | In-memory, Redis        | In-memory only                  |
| Tower layer      | Optional                | Built-in                        |
| Dependencies     | Minimal                 | Jitter + parking_lot            |
| Burst support    | `.allow_burst()`        | Fixed per-quota                 |

## Live Redis tests

The Redis GCRA decision loop (`tests/redis_gcra_live.rs`) needs a real
server, so it is fixture-gated like the rest of the estate:

```sh
docker run -d --name throttle-kit-redis -p 6379:6379 redis:7
cargo test --features redis --test redis_gcra_live -- --ignored --nocapture
```

CI runs the same suite against a `redis:7` service automatically.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE)
at your option.

## Security

Threat model: [THREAT-MODEL.md](THREAT-MODEL.md).

## Performance

Measured hot-path SLOs and allocation profile: [PERF-SLO.md](PERF-SLO.md). Benchmarks run in CI (non-gating regression visibility against the saved `ci` baseline); the zero-allocation warm path is enforced by a counting-allocator test on every `cargo test`.

| Hot path (in-memory GCRA, warm key) | 1.0.0 | 1.1.0 |
|---|---|---|
| Heap allocations per check | 2 (key `String` + async-trait future box) | **0** (counting-allocator verified) |
| Instructions per check (`perf stat`, min of 5) | ~1010 | **~662 (−34.5%)** |
| Clock reads per check | 2 | **1** |

SLOs (idle hardware): `check` warm key < 200 ns P50, fresh key < 250 ns
amortized, `resolve_client_identity` < 150 ns proxied / < 20 ns direct.
The 2026-09-11 re-measurement ran on a saturated shared box (see the
caveat in PERF-SLO.md); instruction counts and allocation behavior are
the authoritative before/after evidence.

Head-to-head numbers against governor (the leading GCRA crate), with an honest feature comparison: [COMPARISON.md](COMPARISON.md).
