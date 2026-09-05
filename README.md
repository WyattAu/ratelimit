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

```rust,ignore
use ratelimit::{RateLimiter, Quota, InMemoryBackend};
use ratelimit::tower_layer::RateLimitLayer;
use axum::{Router, routing::get};

let backend = InMemoryBackend::new();
let layer = RateLimitLayer::new(Quota::per_second(50), backend);

let app = Router::new()
    .route("/", get(handler))
    .layer(layer);
```

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
