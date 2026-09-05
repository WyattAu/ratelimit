# ratelimit

Rate limiting for Rust — **GCRA** (Generic Cell Rate Algorithm) with pluggable
backends (in-memory, Redis) and optional
[Tower](https://docs.rs/tower) layer integration.

[![CI](https://github.com/WyattAu/ratelimit/actions/workflows/ci.yml/badge.svg)](https://github.com/WyattAu/ratelimit/actions)
[![crates.io](https://img.shields.io/crates/v/ratelimit)](https://crates.io/crates/ratelimit)
[![license](https://img.shields.io/crates/l/ratelimit)](LICENSE-MIT)

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

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE)
at your option.

## Security

Threat model: [THREAT-MODEL.md](THREAT-MODEL.md).

## Performance

Measured hot-path SLOs and allocation profile: [PERF-SLO.md](PERF-SLO.md). Benchmarks run in CI (non-gating regression visibility against the saved `ci` baseline).
