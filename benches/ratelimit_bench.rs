// Benchmarks run on fixed, known-good inputs; unwrap failures abort the
// bench run visibly, which is the desired behavior here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

#[cfg(any(feature = "in-memory", feature = "tower"))]
use criterion::{Criterion, criterion_group, criterion_main};
#[cfg(feature = "in-memory")]
use std::sync::Arc;
#[cfg(feature = "in-memory")]
use throttle_kit::{InMemoryBackend, Quota, RateLimiter};

#[cfg(feature = "in-memory")]
fn bench_check_single_key(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    c.bench_function("rate_limit_check_single_key", |b| {
        b.iter_custom(|iters| {
            let backend = InMemoryBackend::new();
            let limiter = RateLimiter::new(Quota::per_second(1_000_000), backend);
            let start = std::time::Instant::now();
            rt.block_on(async {
                for _ in 0..iters {
                    limiter.check("single-key").await;
                }
            });
            start.elapsed()
        });
    });
}

#[cfg(feature = "in-memory")]
fn bench_check_1000_keys(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    c.bench_function("rate_limit_check_1000_keys", |b| {
        b.iter_custom(|iters| {
            let backend = InMemoryBackend::new();
            let limiter = RateLimiter::new(Quota::per_second(1_000_000), backend);
            let start = std::time::Instant::now();
            rt.block_on(async {
                for _ in 0..iters {
                    for i in 0..1000 {
                        let key = format!("key-{i}");
                        limiter.check(&key).await;
                    }
                }
            });
            start.elapsed()
        });
    });
}

#[cfg(feature = "in-memory")]
fn bench_check_contention(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    c.bench_function("rate_limit_check_4_threads", |b| {
        b.iter_custom(|iters| {
            let backend = InMemoryBackend::new();
            let limiter = Arc::new(RateLimiter::new(Quota::per_second(1_000_000), backend));
            let start = std::time::Instant::now();
            rt.block_on(async {
                let mut handles = Vec::with_capacity(4);
                for t in 0..4 {
                    let limiter = limiter.clone();
                    handles.push(tokio::spawn(async move {
                        for i in 0..iters {
                            let key = format!("thread-{t}-key-{i}");
                            limiter.check(&key).await;
                        }
                    }));
                }
                for h in handles {
                    h.await.unwrap();
                }
            });
            start.elapsed()
        });
    });
}

// Client-IP resolution cost (REQ-THROTTLE-100..104). Measures the
// added per-request identity work in front of the limiter check.
#[cfg(feature = "tower")]
fn bench_client_ip_resolution(c: &mut Criterion) {
    use std::net::{IpAddr, Ipv4Addr};
    use throttle_kit::client_ip::{ClientIpConfig, IpNet, resolve_client_identity};

    let trusted_config = ClientIpConfig {
        trusted_proxies: vec![IpNet::parse("10.0.0.0/8").unwrap()],
        num_trusted_hops: 1,
        trusted_header: None,
    };
    let untrusted_config = ClientIpConfig::default();

    // Typical shape: spoofable left entry + client + our proxy's
    // appendage, peer is the trusted proxy.
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        http::HeaderValue::from_static("203.0.113.7, 10.0.0.254"),
    );
    let trusted_peer = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9)));

    c.bench_function("client_ip_resolve_trusted_proxy", |b| {
        b.iter(|| resolve_client_identity(&headers, trusted_peer, &trusted_config))
    });
    c.bench_function("client_ip_resolve_untrusted_peer", |b| {
        b.iter(|| resolve_client_identity(&headers, trusted_peer, &untrusted_config))
    });
}

#[cfg(feature = "in-memory")]
criterion_group!(
    benches,
    bench_check_single_key,
    bench_check_1000_keys,
    bench_check_contention,
);

#[cfg(feature = "tower")]
criterion_group!(client_ip_benches, bench_client_ip_resolution);

#[cfg(all(feature = "tower", feature = "in-memory"))]
criterion_main!(benches, client_ip_benches);
#[cfg(all(feature = "tower", not(feature = "in-memory")))]
criterion_main!(client_ip_benches);
#[cfg(all(not(feature = "tower"), feature = "in-memory"))]
criterion_main!(benches);

// Bench targets are `harness = false`, so a `main` must exist even when
// every benchmark is compiled out.
#[cfg(not(any(feature = "in-memory", feature = "tower")))]
fn main() {}
