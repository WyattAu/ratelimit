//! Config-knob behavior matrix for throttle-kit (ratelimit).
//!
//! Every public knob must OBSERVABLY change behavior: each test pairs a
//! default with an alternate value and asserts the observable output
//! differs.
//!
//! Knobs covered here (4 groups):
//!   1. quota interval (rate) — governs recovery timing + `retry_after`
//!   2. quota burst — governs the rapid-request budget
//!   3. client-IP trust (`trusted_proxies`, `num_trusted_hops`,
//!      `trusted_header`) — switches the rate-limit key between socket
//!      and forwarded header
//!   4. missing-client policy (+ custom key extractor) — decides the fate
//!      of requests with no resolvable identity
//!
//! Deep edge coverage (CIDR math, hop-walk cases, malformed headers,
//! proptest oracle) already lives in `tests/client_ip.rs` and
//! `tests/integration.rs` — cited, not duplicated. This file proves each
//! knob *moves the needle* end to end through the public API.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

#[cfg(feature = "in-memory")]
use std::time::Duration;

#[cfg(feature = "in-memory")]
use throttle_kit::{InMemoryBackend, Quota, RateLimiter};

#[cfg(feature = "in-memory")]
fn backend() -> InMemoryBackend {
    InMemoryBackend::new()
}

// ---------------------------------------------------------------------------
// 1. quota interval governs recovery timing
// ---------------------------------------------------------------------------

#[cfg(feature = "in-memory")]
#[tokio::test]
async fn knob_quota_interval_governs_recovery() {
    // 1/second, burst 1: the second immediate check is denied with a
    // retry hint...
    let strict = RateLimiter::new(Quota::per_second(1).allow_burst(1), backend());
    assert!(strict.check("k").await.allowed);
    let denied = strict.check("k").await;
    assert!(!denied.allowed, "burst exhausted: second check must deny");
    assert!(
        denied.retry_after.is_some(),
        "denial must carry a retry_after hint"
    );

    // ...while a 1 ms interval recovers after a short sleep.
    let fast = RateLimiter::new(Quota::from_parts(Duration::from_millis(1), 1), backend());
    assert!(fast.check("k").await.allowed);
    assert!(!fast.check("k").await.allowed);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        fast.check("k").await.allowed,
        "interval expiry must restore the budget"
    );
}

// ---------------------------------------------------------------------------
// 2. quota burst sets the rapid-request budget
// ---------------------------------------------------------------------------

#[cfg(feature = "in-memory")]
#[tokio::test]
async fn knob_burst_sets_rapid_budget() {
    let one = RateLimiter::new(Quota::per_second(1).allow_burst(1), backend());
    assert!(one.check("k").await.allowed);
    assert!(
        !one.check("k").await.allowed,
        "burst=1 allows one rapid check"
    );

    let three = RateLimiter::new(Quota::per_second(1).allow_burst(3), backend());
    for _ in 0..3 {
        assert!(three.check("k").await.allowed);
    }
    assert!(!three.check("k").await.allowed, "burst=3 allows three");
}

// ---------------------------------------------------------------------------
// 3. client-IP trust switches the rate-limit key
// ---------------------------------------------------------------------------

#[cfg(feature = "tower")]
use std::net::{IpAddr, Ipv4Addr};

#[cfg(feature = "tower")]
use throttle_kit::client_ip::{ClientIpConfig, ClientIpSource, IpNet, resolve_client_identity};

#[cfg(feature = "tower")]
fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

#[cfg(feature = "tower")]
fn xff(value: &str) -> http::HeaderMap {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        http::HeaderValue::from_str(value).unwrap(),
    );
    headers
}

#[cfg(feature = "tower")]
#[test]
fn knob_trusted_proxies_switch_keying() {
    let peer = v4(10, 0, 0, 1);
    // Chain shape (as appended by proxies): client first, last hop last.
    let headers = xff("203.0.113.7, 10.0.0.254");

    // Default: no trusted proxies → spoofed header ignored, keyed by peer.
    let resolved = resolve_client_identity(&headers, Some(peer), &ClientIpConfig::default())
        .expect("peer present");
    assert_eq!(resolved.ip, peer);
    assert_eq!(resolved.source, ClientIpSource::PeerSocket);

    // Trusting the peer's network → the header entry becomes the key.
    let trusting = ClientIpConfig {
        trusted_proxies: vec![IpNet::parse("10.0.0.0/8").unwrap()],
        num_trusted_hops: 1,
        trusted_header: None,
    };
    let resolved = resolve_client_identity(&headers, Some(peer), &trusting).expect("peer present");
    assert_eq!(resolved.ip, v4(203, 0, 113, 7));
    assert_eq!(resolved.source, ClientIpSource::ForwardedHeader);
}

#[cfg(feature = "tower")]
#[test]
fn knob_num_trusted_hops_moves_the_walk() {
    // Chain: client, first hop; peer appended nothing (single entry + peer).
    let peer = v4(10, 0, 0, 1);
    let headers = xff("203.0.113.7, 10.0.0.254");
    let base = || ClientIpConfig {
        trusted_proxies: vec![IpNet::parse("10.0.0.0/8").unwrap()],
        num_trusted_hops: 1,
        trusted_header: None,
    };
    // 1 hop skipped → the client entry.
    let r1 = resolve_client_identity(&headers, Some(peer), &base()).unwrap();
    assert_eq!(r1.ip, v4(203, 0, 113, 7));
    // 0 hops → the rightmost entry (the last proxy).
    let zero = ClientIpConfig {
        num_trusted_hops: 0,
        ..base()
    };
    let r0 = resolve_client_identity(&headers, Some(peer), &zero).unwrap();
    assert_eq!(r0.ip, v4(10, 0, 0, 254));
}

#[cfg(feature = "tower")]
#[test]
fn knob_trusted_header_overrides_xff() {
    let peer = v4(10, 0, 0, 1);
    let mut headers = xff("6.6.6.6");
    headers.insert(
        "cf-connecting-ip",
        http::HeaderValue::from_static("203.0.113.9"),
    );
    let cfg = ClientIpConfig {
        trusted_proxies: vec![IpNet::parse("10.0.0.0/8").unwrap()],
        num_trusted_hops: 0,
        trusted_header: Some(http::HeaderName::from_static("cf-connecting-ip")),
    };
    let resolved = resolve_client_identity(&headers, Some(peer), &cfg).unwrap();
    assert_eq!(
        resolved.ip,
        v4(203, 0, 113, 9),
        "trusted_header must win over X-Forwarded-For"
    );
}

// ---------------------------------------------------------------------------
// 4. missing-client policy + custom key extractor (tower layer)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "tower", feature = "in-memory"))]
mod tower_knobs {
    use super::*;
    use std::task::{Context, Poll};
    use tower::{Layer, Service};

    #[derive(Clone)]
    struct OkService;

    impl Service<http::Request<()>> for OkService {
        type Response = http::Response<()>;
        type Error = std::convert::Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: http::Request<()>) -> Self::Future {
            std::future::ready(Ok(http::Response::new(())))
        }
    }

    use throttle_kit::RateLimitLayer;
    use throttle_kit::client_ip::MissingClientPolicy;

    #[tokio::test]
    async fn knob_missing_policy_decides_identity_less_requests() {
        // Default: fail closed with 503...
        let reject = RateLimitLayer::new(Quota::per_second(10), backend());
        let mut service = reject.layer(OkService);
        let req = http::Request::builder().body(()).unwrap();
        assert_eq!(
            service.call(req).await.unwrap().status(),
            http::StatusCode::SERVICE_UNAVAILABLE
        );

        // ...opt-in shared bucket serves then rate-limits as one identity.
        let fallback = RateLimitLayer::new(Quota::per_second(1), backend())
            .with_missing_client_policy(MissingClientPolicy::FallbackKey("shared".into()));
        let mut service = fallback.layer(OkService);
        let req = http::Request::builder().body(()).unwrap();
        assert_eq!(
            service.call(req).await.unwrap().status(),
            http::StatusCode::OK
        );
        let req = http::Request::builder().body(()).unwrap();
        assert_eq!(
            service.call(req).await.unwrap().status(),
            http::StatusCode::TOO_MANY_REQUESTS,
            "fallback requests must share one budget"
        );
    }

    #[tokio::test]
    async fn knob_key_extractor_replaces_ip_keying() {
        use throttle_kit::KeyExtractor;
        let extractor: KeyExtractor = std::sync::Arc::new(|headers, _| {
            headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("anon")
                .to_string()
        });
        let layer =
            RateLimitLayer::new(Quota::per_second(1), backend()).with_key_extractor(extractor);
        let mut service = layer.layer(OkService);
        let req = http::Request::builder()
            .header("x-api-key", "tenant-a")
            .body(())
            .unwrap();
        assert_eq!(
            service.call(req).await.unwrap().status(),
            http::StatusCode::OK
        );
        // Same API key, no identity headers at all → still the same bucket.
        let req = http::Request::builder()
            .header("x-api-key", "tenant-a")
            .body(())
            .unwrap();
        assert_eq!(
            service.call(req).await.unwrap().status(),
            http::StatusCode::TOO_MANY_REQUESTS
        );
    }
}

// ---------------------------------------------------------------------------
// Bonus: per-key quota overrides
// ---------------------------------------------------------------------------

#[cfg(feature = "in-memory")]
#[tokio::test]
async fn knob_per_key_override_beats_default_quota() {
    use throttle_kit::KeyedRateLimiter;
    let limiter = KeyedRateLimiter::new(Quota::per_second(1).allow_burst(1), backend());
    limiter.with_quota_for_key("vip", Quota::per_second(100).allow_burst(100));
    assert!(limiter.check("ordinary").await.allowed);
    assert!(!limiter.check("ordinary").await.allowed);
    for _ in 0..10 {
        assert!(
            limiter.check("vip").await.allowed,
            "per-key override must lift the VIP budget"
        );
    }
}
