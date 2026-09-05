// Benchmarks run on fixed, known-good inputs; unwrap failures abort the
// bench run visibly, which is the desired behavior here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::sync::Arc;
use throttle_kit::{InMemoryBackend, Quota, RateLimiter};

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

criterion_group!(
    benches,
    bench_check_single_key,
    bench_check_1000_keys,
    bench_check_contention,
);
criterion_main!(benches);
