// Tests exercise hostile inputs directly; unwrap/expect, slicing, and
// panicking asserts are the test signal here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Client identity resolution tests: CIDR bit-math edges, the
//! right-to-left trust walk (REQ-THROTTLE-100..104), layer wiring, and
//! property tests against an independent oracle.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::task::{Context, Poll};

use http::{HeaderMap, HeaderValue, StatusCode};
use proptest::prelude::*;
use tower::{Layer, Service};

use throttle_kit::client_ip::{
    ClientIpConfig, ClientIpError, ClientIpSource, IpNet, MissingClientIdentity,
    MissingClientPolicy, resolve_client_identity,
};
use throttle_kit::{InMemoryBackend, Quota, RateLimitLayer};

const TRUSTED_PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
const CLIENT: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
const SPOOFED: IpAddr = IpAddr::V4(Ipv4Addr::new(6, 6, 6, 6));

fn config_trusting_peer(hops: usize) -> ClientIpConfig {
    ClientIpConfig {
        trusted_proxies: vec![IpNet::parse("10.0.0.0/8").unwrap()],
        num_trusted_hops: hops,
        trusted_header: None,
    }
}

fn xff_headers(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-for", HeaderValue::from_str(value).unwrap());
    headers
}

fn resolved_ip(
    headers: &HeaderMap,
    peer: Option<IpAddr>,
    config: &ClientIpConfig,
) -> Result<(IpAddr, ClientIpSource), MissingClientIdentity> {
    resolve_client_identity(headers, peer, config).map(|resolved| (resolved.ip, resolved.source))
}

// ---------------------------------------------------------------------------
// REQ-THROTTLE-104 — CIDR bit-math edges
// ---------------------------------------------------------------------------

#[test]
fn cidr_v4_prefix_zero_contains_all_v4() {
    let net = IpNet::V4 {
        addr: Ipv4Addr::new(0, 0, 0, 0),
        prefix: 0,
    };
    for ip in [
        Ipv4Addr::new(0, 0, 0, 0),
        Ipv4Addr::new(1, 2, 3, 4),
        Ipv4Addr::new(255, 255, 255, 255),
        Ipv4Addr::new(128, 0, 0, 1),
    ] {
        assert!(net.contains(IpAddr::V4(ip)), "{ip} in 0.0.0.0/0");
    }
    // Cross-family: a v4 /0 never contains a plain v6 address.
    assert!(!net.contains(IpAddr::V6("2001:db8::1".parse().unwrap())));
}

#[test]
fn cidr_v4_slash32_exact_match_only() {
    let net = IpNet::parse("203.0.113.7/32").unwrap();
    assert!(net.contains(CLIENT));
    assert!(!net.contains(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 6))));
    assert!(!net.contains(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 8))));
    assert!(!net.contains(IpAddr::V4(Ipv4Addr::new(203, 0, 112, 7))));
}

#[test]
fn cidr_v4_slash31_contains_both_addresses() {
    let net = IpNet::parse("203.0.113.6/31").unwrap();
    assert!(net.contains(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 6))));
    assert!(net.contains(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))));
    assert!(!net.contains(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5))));
    assert!(!net.contains(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 8))));
}

#[test]
fn cidr_v4_prefix_boundaries() {
    let net = IpNet::parse("10.1.2.3/8").unwrap();
    assert!(net.contains(IpAddr::V4(Ipv4Addr::new(10, 255, 255, 255))));
    assert!(!net.contains(IpAddr::V4(Ipv4Addr::new(11, 0, 0, 0))));

    let net24 = IpNet::parse("192.168.1.0/24").unwrap();
    assert!(net24.contains(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 255))));
    assert!(!net24.contains(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 0))));

    let net1 = IpNet::V4 {
        addr: Ipv4Addr::new(0, 0, 0, 0),
        prefix: 1,
    };
    assert!(net1.contains(IpAddr::V4(Ipv4Addr::new(127, 255, 0, 1))));
    assert!(!net1.contains(IpAddr::V4(Ipv4Addr::new(128, 0, 0, 0))));
}

#[test]
fn cidr_v6_full_and_zero() {
    let host = IpNet::parse("2001:db8::1/128").unwrap();
    assert!(host.contains(IpAddr::V6("2001:db8::1".parse().unwrap())));
    assert!(!host.contains(IpAddr::V6("2001:db8::2".parse().unwrap())));

    let everything = IpNet::V6 {
        addr: Ipv6Addr::UNSPECIFIED,
        prefix: 0,
    };
    assert!(everything.contains(IpAddr::V6("2001:db8::1".parse().unwrap())));
    assert!(everything.contains(IpAddr::V6(Ipv6Addr::LOCALHOST)));
}

#[test]
fn cidr_v6_prefix_spot_checks() {
    let net = IpNet::parse("2001:db8::/32").unwrap();
    assert!(net.contains(IpAddr::V6("2001:db8::1234:5678".parse().unwrap())));
    assert!(net.contains(IpAddr::V6("2001:db8:ffff::".parse().unwrap())));
    assert!(!net.contains(IpAddr::V6("2001:db9::".parse().unwrap())));
    assert!(!net.contains(IpAddr::V6("2001:db7::".parse().unwrap())));

    let fc = IpNet::parse("fc00::/7").unwrap();
    assert!(fc.contains(IpAddr::V6("fd00::1".parse().unwrap())));
    assert!(fc.contains(IpAddr::V6("fc00::".parse().unwrap())));
    assert!(!fc.contains(IpAddr::V6("fe00::1".parse().unwrap())));
}

#[test]
fn cidr_v4_mapped_v6_treated_as_v4() {
    // Decision (documented in module docs): v4-mapped IPv6 is evaluated
    // as IPv4, so it matches IPv4 networks only.
    let net = IpNet::parse("203.0.113.0/24").unwrap();
    let mapped: IpAddr = "::ffff:203.0.113.42".parse().unwrap();
    assert!(net.contains(mapped));

    // And a v4-mapped peer matches a v4 trust entry — see
    // resolver_v4_mapped_peer_matches_v4_trust below for the resolver
    // level of the same decision.

    // Plain v6 is never in a v4 net, and v4 is never in a plain v6 net.
    assert!(!net.contains(IpAddr::V6("2001:db8::1".parse().unwrap())));
    let v6net = IpNet::parse("2001:db8::/32").unwrap();
    assert!(!v6net.contains(CLIENT));
}

#[test]
fn cidr_direct_literal_struct_construction_masks_host_bits() {
    // Pub fields mean callers can construct with host bits set;
    // contains() must still mask.
    let net = IpNet::V4 {
        addr: Ipv4Addr::new(10, 1, 2, 3),
        prefix: 8,
    };
    assert!(net.contains(IpAddr::V4(Ipv4Addr::new(10, 255, 0, 1))));
    assert!(!net.contains(IpAddr::V4(Ipv4Addr::new(11, 0, 0, 1))));

    let net = IpNet::V6 {
        addr: "2001:db8:1::1".parse().unwrap(),
        prefix: 32,
    };
    assert!(net.contains(IpAddr::V6("2001:db8:ffff::".parse().unwrap())));
    assert!(!net.contains(IpAddr::V6("2001:db9::".parse().unwrap())));
}

// ---------------------------------------------------------------------------
// IpNet::parse forms
// ---------------------------------------------------------------------------

#[test]
fn parse_bare_ip_implies_max_prefix() {
    let v4 = IpNet::parse("203.0.113.7").unwrap();
    assert_eq!(
        v4,
        IpNet::V4 {
            addr: Ipv4Addr::new(203, 0, 113, 7),
            prefix: 32
        }
    );
    let v6 = IpNet::parse("2001:db8::1").unwrap();
    assert_eq!(
        v6,
        IpNet::V6 {
            addr: "2001:db8::1".parse().unwrap(),
            prefix: 128
        }
    );
}

#[test]
fn parse_accepts_prefix_forms_and_trims() {
    assert_eq!(
        IpNet::parse("10.0.0.0/8").unwrap(),
        IpNet::V4 {
            addr: Ipv4Addr::new(10, 0, 0, 0),
            prefix: 8
        }
    );
    assert_eq!(
        IpNet::parse(" 10.0.0.0 / 8 ").unwrap(),
        IpNet::V4 {
            addr: Ipv4Addr::new(10, 0, 0, 0),
            prefix: 8
        }
    );
    assert_eq!(
        IpNet::parse("fc00::/7").unwrap(),
        IpNet::V6 {
            addr: "fc00::".parse().unwrap(),
            prefix: 7
        }
    );
}

#[test]
fn parse_normalizes_v4_mapped() {
    let net = IpNet::parse("::ffff:10.0.0.5").unwrap();
    assert_eq!(
        net,
        IpNet::V4 {
            addr: Ipv4Addr::new(10, 0, 0, 5),
            prefix: 32
        }
    );
    // Compat-form ::10.0.0.5 (not v4-mapped) stays IPv6.
    let compat = IpNet::parse("::10.0.0.5").unwrap();
    assert!(matches!(compat, IpNet::V6 { .. }));
}

#[test]
fn parse_rejects_invalid_input() {
    for bad in [
        "1.2.3.4/33",
        "1.2.3.4/-1",
        "1.2.3.4/",
        "1.2.3.4/x",
        "2001:db8::/129",
        "not-an-ip",
        "1.2.3",
        "",
        "10.0.0.0/8/8",
        "/8",
    ] {
        let err = IpNet::parse(bad).unwrap_err();
        assert!(
            matches!(
                err,
                ClientIpError::InvalidIp(_) | ClientIpError::InvalidPrefix(_)
            ),
            "{bad:?} should be rejected"
        );
    }
    assert_eq!(
        IpNet::parse("1.2.3.4/33").unwrap_err(),
        ClientIpError::InvalidPrefix("33".to_string())
    );
    assert_eq!(
        IpNet::parse("2001:db8::/129").unwrap_err(),
        ClientIpError::InvalidPrefix("129".to_string())
    );
    assert_eq!(
        IpNet::parse("nope").unwrap_err(),
        ClientIpError::InvalidIp("nope".to_string())
    );
}

// ---------------------------------------------------------------------------
// REQ-THROTTLE-100 — untrusted direct peer: headers are lies
// ---------------------------------------------------------------------------

#[test]
fn req100_untrusted_peer_ignores_spoofed_xff() {
    let config = ClientIpConfig {
        trusted_proxies: vec![IpNet::parse("192.168.0.0/16").unwrap()],
        num_trusted_hops: 1,
        trusted_header: None,
    };
    // Peer 10.0.0.1 is NOT in the trusted list, whatever the header says.
    let headers = xff_headers(&format!("{SPOOFED}, {CLIENT}"));
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, TRUSTED_PEER);
    assert_eq!(source, ClientIpSource::PeerSocket);
}

#[test]
fn req100_default_config_ignores_headers() {
    // Secure by default: empty trusted_proxies → header never consulted.
    let config = ClientIpConfig::default();
    let headers = xff_headers(&format!("{SPOOFED}"));
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, TRUSTED_PEER);
    assert_eq!(source, ClientIpSource::PeerSocket);
}

// ---------------------------------------------------------------------------
// REQ-THROTTLE-101 — trusted peer: right-to-left walk minus hops
// ---------------------------------------------------------------------------

#[test]
fn req101_right_to_left_walk_skips_hops() {
    // nginx (peer 10.0.0.1) appended the ALB's address; hops=1 skips it.
    let config = config_trusting_peer(1);
    let headers = xff_headers(&format!("{SPOOFED}, {CLIENT}, 10.0.0.254"));
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, CLIENT);
    assert_eq!(source, ClientIpSource::ForwardedHeader);
}

#[test]
fn req101_chain_longer_than_hops_resolves_entry() {
    let config = config_trusting_peer(2);
    let headers = xff_headers(&format!("{SPOOFED}, {CLIENT}, 10.0.0.253, 10.0.0.254"));
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, CLIENT);
    assert_eq!(source, ClientIpSource::ForwardedHeader);
}

#[test]
fn req101_chain_exactly_hops_falls_back_to_peer() {
    let config = config_trusting_peer(2);
    let headers = xff_headers(&format!("{CLIENT}, 10.0.0.254"));
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, TRUSTED_PEER);
    assert_eq!(source, ClientIpSource::PeerSocket);
}

#[test]
fn req101_hops_zero_takes_rightmost_entry() {
    // Single trusted proxy: XFF is just the client it appended.
    let config = config_trusting_peer(0);
    let headers = xff_headers(&format!("{CLIENT}"));
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, CLIENT);
    assert_eq!(source, ClientIpSource::ForwardedHeader);

    // With hops=0, rightmost garbage still falls back — no leftward walk.
    let headers = xff_headers("garbage");
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, TRUSTED_PEER);
    assert_eq!(source, ClientIpSource::PeerSocket);
}

#[test]
fn req101_malformed_chosen_entry_falls_back_no_leftward_walk() {
    // The chosen entry (second from right; the rightmost was appended by
    // our direct peer) is garbage: fall back to the peer. NEVER continue
    // leftward into attacker-controlled entries.
    let config = config_trusting_peer(1);
    let headers = xff_headers(&format!("{SPOOFED}, not-an-ip, 10.0.0.254"));
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, TRUSTED_PEER);
    assert_eq!(source, ClientIpSource::PeerSocket);
}

#[test]
fn req101_trusted_header_override() {
    // Dedicated headers (CF-Connecting-IP) carry only the client IP, so
    // the rightmost entry IS the client: num_trusted_hops = 0.
    let config = ClientIpConfig {
        trusted_proxies: vec![IpNet::parse("10.0.0.0/8").unwrap()],
        num_trusted_hops: 0,
        trusted_header: Some("cf-connecting-ip".parse().unwrap()),
    };
    let mut headers = xff_headers(&format!("{SPOOFED}"));
    headers.insert(
        "cf-connecting-ip",
        HeaderValue::from_str(&CLIENT.to_string()).unwrap(),
    );
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, CLIENT);
    assert_eq!(source, ClientIpSource::ForwardedHeader);

    // Same trust rules apply: missing override header → peer fallback
    // (the spoofed XFF is not consulted either — it is not the trusted
    // header).
    let mut headers = xff_headers(&format!("{CLIENT}"));
    headers.remove("cf-connecting-ip");
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, TRUSTED_PEER);
    assert_eq!(source, ClientIpSource::PeerSocket);

    // Malformed override header value → peer fallback.
    let headers = xff_headers(&format!("{CLIENT}"));
    let mut headers = headers.clone();
    headers.insert("cf-connecting-ip", HeaderValue::from_static("not-an-ip"));
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, TRUSTED_PEER);
    assert_eq!(source, ClientIpSource::PeerSocket);
}

#[test]
fn req101_ipv6_chain_entry_resolves() {
    let config = ClientIpConfig {
        trusted_proxies: vec![IpNet::parse("fd00::/8").unwrap()],
        num_trusted_hops: 1,
        trusted_header: None,
    };
    let peer: IpAddr = "fd00::1".parse().unwrap();
    let client: IpAddr = "2001:db8::42".parse().unwrap();
    // Chain shape: spoofed entry (left), client (as seen by the first
    // trusted hop), and the address our direct peer appended (right).
    let headers = xff_headers(&format!("{SPOOFED}, {client}, fd00::99"));
    let (ip, source) = resolved_ip(&headers, Some(peer), &config).unwrap();
    assert_eq!(ip, client);
    assert_eq!(source, ClientIpSource::ForwardedHeader);
}

#[test]
fn resolver_v4_mapped_peer_matches_v4_trust() {
    // Dual-stack socket reports the peer as ::ffff:10.0.0.1; documented
    // decision: treated as IPv4, so the 10.0.0.0/8 trust entry applies.
    let config = config_trusting_peer(0);
    let peer: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
    let headers = xff_headers("::ffff:203.0.113.7");
    let (ip, source) = resolved_ip(&headers, Some(peer), &config).unwrap();
    assert_eq!(ip, CLIENT); // normalized to plain v4
    assert_eq!(source, ClientIpSource::ForwardedHeader);
}

// ---------------------------------------------------------------------------
// REQ-THROTTLE-102 — malformed/missing header → peer IP
// ---------------------------------------------------------------------------

#[test]
fn req102_missing_header_falls_back_to_peer() {
    let config = config_trusting_peer(1);
    let headers = HeaderMap::new();
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, TRUSTED_PEER);
    assert_eq!(source, ClientIpSource::PeerSocket);
}

#[test]
fn req102_empty_xff_falls_back_to_peer() {
    let config = config_trusting_peer(1);
    let headers = xff_headers("");
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, TRUSTED_PEER);
    assert_eq!(source, ClientIpSource::PeerSocket);
}

#[test]
fn regression_v4mapped_spoof_after_empty_entries() {
    // Regression for proptest seed
    // `entries = ["::ffff:6.6.6.6", "", "", ""], hops = 3`: the
    // right-to-left walk must skip the empty (malformed) entries and
    // normalize the v4-mapped IPv6 address to plain IPv4.
    let config = config_trusting_peer(3);
    let headers = xff_headers("::ffff:6.6.6.6,,,");
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, SPOOFED);
    assert_eq!(source, ClientIpSource::ForwardedHeader);
}

#[test]
fn req102_whitespace_and_junk_entries_fall_back_to_peer() {
    let config = config_trusting_peer(1);
    // "  " trims to an empty (malformed) chosen entry.
    let headers = xff_headers("   , 10.0.0.254");
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, TRUSTED_PEER);
    assert_eq!(source, ClientIpSource::PeerSocket);

    // Only-whitespace chain.
    let headers = xff_headers("   ");
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, TRUSTED_PEER);
    assert_eq!(source, ClientIpSource::PeerSocket);
}

#[test]
fn req102_non_utf8_header_value_falls_back_to_peer() {
    let config = config_trusting_peer(0);
    let mut headers = HeaderMap::new();
    // 0xff/0xfe are legal header bytes (obs-text) but not valid UTF-8.
    headers.insert(
        "x-forwarded-for",
        HeaderValue::from_bytes(&[0xff, 0xfe, 0x80]).unwrap(),
    );
    let (ip, source) = resolved_ip(&headers, Some(TRUSTED_PEER), &config).unwrap();
    assert_eq!(ip, TRUSTED_PEER);
    assert_eq!(source, ClientIpSource::PeerSocket);
}

// ---------------------------------------------------------------------------
// REQ-THROTTLE-103 — no ConnectInfo → policy
// ---------------------------------------------------------------------------

#[test]
fn req103_no_peer_is_missing_identity() {
    let config = config_trusting_peer(1);
    let headers = xff_headers(&format!("{CLIENT}"));
    assert_eq!(
        resolve_client_identity(&headers, None, &config),
        Err(MissingClientIdentity)
    );
    // Even with no config at all.
    assert_eq!(
        resolve_client_identity(&HeaderMap::new(), None, &ClientIpConfig::default()),
        Err(MissingClientIdentity)
    );
}

// ---------------------------------------------------------------------------
// Layer wiring (Tower service level)
// ---------------------------------------------------------------------------

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

fn connect_info(ip: [u8; 4]) -> http::Extensions {
    let mut ext = http::Extensions::new();
    ext.insert(axum::extract::ConnectInfo(SocketAddr::from((ip, 44300))));
    ext
}

async fn layer_status(
    layer: RateLimitLayer<InMemoryBackend>,
    peer: [u8; 4],
    xff: Option<&str>,
) -> StatusCode {
    let mut service = layer.layer(OkService);
    let mut builder = http::Request::builder();
    if let Some(xff) = xff {
        builder = builder.header("x-forwarded-for", xff);
    }
    let mut request = builder.body(()).unwrap();
    *request.extensions_mut() = connect_info(peer);
    service.call(request).await.unwrap().status()
}

#[tokio::test]
async fn layer_default_keys_by_peer_and_ignores_spoofed_xff() {
    // Untrusted peer, 2/second: the spoofed XFF must NOT open new budgets.
    let layer = RateLimitLayer::new(Quota::per_second(2), InMemoryBackend::new());
    let spoof = format!("{SPOOFED}");
    assert_eq!(
        layer_status(layer.clone(), [198, 51, 100, 9], Some(&spoof)).await,
        StatusCode::OK
    );
    assert_eq!(
        layer_status(layer.clone(), [198, 51, 100, 9], Some(&spoof)).await,
        StatusCode::OK
    );
    // Third request from the SAME peer is limited even with a fresh spoof.
    assert_eq!(
        layer_status(layer.clone(), [198, 51, 100, 9], Some(&spoof)).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // A different peer still has its own budget.
    assert_eq!(
        layer_status(layer.clone(), [198, 51, 100, 10], Some(&spoof)).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn layer_trusted_proxy_resolves_forwarded_identity() {
    let layer = RateLimitLayer::new(Quota::per_second(1), InMemoryBackend::new())
        .with_client_ip(config_trusting_peer(1));
    // Chain shape: spoofed entry, client (appended by the first trusted
    // hop), and the address our direct peer appended.
    let client = format!("{CLIENT}, 10.0.0.254");
    assert_eq!(
        layer_status(layer.clone(), [10, 0, 0, 1], Some(&client)).await,
        StatusCode::OK
    );
    assert_eq!(
        layer_status(layer.clone(), [10, 0, 0, 1], Some(&client)).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        layer_status(
            layer.clone(),
            [10, 0, 0, 1],
            Some("203.0.113.8, 10.0.0.254")
        )
        .await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn layer_missing_connect_info_rejects_with_503_by_default() {
    let layer = RateLimitLayer::new(Quota::per_second(10), InMemoryBackend::new())
        .with_client_ip(config_trusting_peer(1));
    let mut service = layer.layer(OkService);
    // No ConnectInfo extension.
    let request = http::Request::builder().body(()).unwrap();
    assert_eq!(
        service.call(request).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn layer_missing_connect_info_fallback_key_opt_in() {
    let layer = RateLimitLayer::new(Quota::per_second(1), InMemoryBackend::new())
        .with_missing_client_policy(MissingClientPolicy::FallbackKey("shared".into()));
    let mut service = layer.layer(OkService);
    let request = http::Request::builder().body(()).unwrap();
    assert_eq!(
        service.call(request).await.unwrap().status(),
        StatusCode::OK
    );
    // Second request shares the single fallback bucket.
    let request = http::Request::builder().body(()).unwrap();
    assert_eq!(
        service.call(request).await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn layer_custom_key_extractor_keeps_non_ip_keying() {
    let extractor: throttle_kit::KeyExtractor = std::sync::Arc::new(|headers, _| {
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("anonymous")
            .to_string()
    });
    let layer = RateLimitLayer::new(Quota::per_second(1), InMemoryBackend::new())
        .with_key_extractor(extractor);
    let mut service = layer.layer(OkService);

    let request = http::Request::builder()
        .header("authorization", "Bearer tenant-a")
        .header("x-forwarded-for", "6.6.6.6")
        .body(())
        .unwrap();
    assert_eq!(
        service.call(request).await.unwrap().status(),
        StatusCode::OK
    );
    // Same key exhausted even though the XFF (ignored) differs.
    let request = http::Request::builder()
        .header("authorization", "Bearer tenant-a")
        .header("x-forwarded-for", "7.7.7.7")
        .body(())
        .unwrap();
    assert_eq!(
        service.call(request).await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    // Different key unaffected.
    let request = http::Request::builder()
        .header("authorization", "Bearer tenant-b")
        .body(())
        .unwrap();
    assert_eq!(
        service.call(request).await.unwrap().status(),
        StatusCode::OK
    );
}

// ---------------------------------------------------------------------------
// Property tests — adversarial chains vs. an independent oracle
// ---------------------------------------------------------------------------

/// Independent oracle for the spec's right-to-left walk: implemented as
/// an explicit skip loop rather than the index arithmetic of the
/// implementation under test.
fn oracle(header: &str, hops: usize, peer: IpAddr) -> Result<(IpAddr, ClientIpSource), ()> {
    // Non-ASCII header values (obs-text) are malformed → peer fallback.
    if !header.is_ascii() {
        return Ok((peer, ClientIpSource::PeerSocket));
    }
    let entries: Vec<&str> = header.split(',').map(str::trim).collect();
    let mut skipped = 0usize;
    for entry in entries.iter().rev() {
        if skipped < hops {
            skipped += 1;
            continue;
        }
        return match entry.parse::<IpAddr>() {
            // Documented spec decision: the resolved identity
            // normalizes v4-mapped IPv6 to plain IPv4.
            Ok(IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
                Some(v4) => Ok((IpAddr::V4(v4), ClientIpSource::ForwardedHeader)),
                None => Ok((IpAddr::V6(v6), ClientIpSource::ForwardedHeader)),
            },
            Ok(ip) => Ok((ip, ClientIpSource::ForwardedHeader)),
            Err(_) => Ok((peer, ClientIpSource::PeerSocket)),
        };
    }
    Ok((peer, ClientIpSource::PeerSocket))
}

fn adversarial_entry() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just("   ".to_string()),
        Just("6.6.6.6".to_string()),         // spoofed client
        Just("1.2.3".to_string()),           // truncated v4
        Just("999.999.999.999".to_string()), // out-of-range octets
        Just("garbage".to_string()),
        Just("::1".to_string()),
        Just("::ffff:6.6.6.6".to_string()), // v4-mapped spoof
        Just("fd00::dead".to_string()),
        Just("2001:db8::1".to_string()),
        Just("203.0.113.[7".to_string()),
        "[^\n]{0,24}".prop_map(|s| s), // arbitrary junk (no newline: header-safe)
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn prop_adversarial_chain_matches_oracle(
        entries in prop::collection::vec(adversarial_entry(), 0..8),
        hops in 0usize..6,
    ) {
        // The peer is trusted; hops vary freely, entries are hostile.
        let config = config_trusting_peer(hops);
        // Entries containing ',' shift the split for BOTH the resolver
        // and the oracle identically, so joining is spec-faithful.
        // Only header-value-safe strings reach the resolver; raw byte
        // hostility is covered by prop_resolution_never_panics.
        let header = entries.join(",");
        if HeaderValue::from_str(&header).is_err() {
            prop_assume!(false, "invalid header value covered elsewhere");
        }
        let headers = xff_headers(&header);
        let peer = Some(TRUSTED_PEER);

        let expected = oracle(&header, hops, TRUSTED_PEER);
        let actual = resolved_ip(&headers, peer, &config);

        prop_assert_eq!(
            actual.map_err(|_| ()),
            expected,
            "header/hops mismatch"
        );
    }

    #[test]
    fn prop_untrusted_peer_always_peer_socket(
        entries in prop::collection::vec(adversarial_entry(), 0..6),
        hops in 0usize..6,
    ) {
        let config = ClientIpConfig {
            trusted_proxies: vec![IpNet::parse("192.168.0.0/16").unwrap()],
            num_trusted_hops: hops,
            trusted_header: None,
        };
        let header = entries.join(",");
        if HeaderValue::from_str(&header).is_err() {
            prop_assume!(false, "invalid header value covered elsewhere");
        }
        let headers = xff_headers(&header);
        let resolved = resolve_client_identity(&headers, Some(TRUSTED_PEER), &config).unwrap();
        prop_assert_eq!(resolved.ip, TRUSTED_PEER);
        prop_assert_eq!(resolved.source, ClientIpSource::PeerSocket);
    }

    #[test]
    fn prop_resolution_never_panics(
        raw in prop::collection::vec(any::<u8>(), 0..64),
        hops in 0usize..8,
        trusted in 0usize..3,
        peer_is_v6 in any::<bool>(),
    ) {
        let nets: Vec<IpNet> = ["10.0.0.0/8", "172.16.0.0/12", "fc00::/7"]
            .iter()
            .take(trusted)
            .map(|s| IpNet::parse(s).unwrap())
            .collect();
        let config = ClientIpConfig {
            trusted_proxies: nets,
            num_trusted_hops: hops,
            trusted_header: None,
        };
        let mut headers = HeaderMap::new();
        // Invalid UTF-8 or control bytes are part of the fuzz surface.
        if let Ok(value) = HeaderValue::from_bytes(&raw) {
            headers.insert("x-forwarded-for", value);
        }
        let peer: IpAddr = if peer_is_v6 {
            IpAddr::V6("fd00::1".parse().unwrap())
        } else {
            TRUSTED_PEER
        };
        // Ok or Err are both fine — the invariant is: never panic.
        let _ = resolve_client_identity(&headers, Some(peer), &config);
        let _ = resolve_client_identity(&headers, None, &config);
    }
}
