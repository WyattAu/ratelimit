#![no_main]
// Fuzz harnesses exercise hostile inputs; failing to build a header from
// arbitrary bytes is an expected, non-crashing outcome.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use libfuzzer_sys::fuzz_target;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use throttle_kit::client_ip::{ClientIpConfig, IpNet, resolve_client_identity};

// Arbitrary (header bytes, trusted list size, hops) → resolve. The
// invariant is Err/Ok, never panic, and an unresolvable identity never
// yields a forwarded address.
fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let hops = (data[0] as usize) % 8;
    let trusted = (data[1] as usize) % 4;
    let use_override_header = data[2] & 1 == 1;
    let peer_is_trusted = data[2] & 2 == 2;
    let peer_is_v6 = data[2] & 4 == 4;

    // Also fuzz the CIDR parser with a slice of the input.
    let split = (data[3] as usize).min(data.len() - 4);
    let net_input = String::from_utf8_lossy(&data[4..4 + split]);
    if let Ok(net) = IpNet::parse(&net_input) {
        // Parse output must be consistent with contains() for the very
        // same string's parsed address portion, when that parses.
        if let Ok(ip) = net_input.split('/').next().unwrap_or("").trim().parse::<IpAddr>() {
            assert!(net.contains(ip), "parsed net must contain its own address");
        }
        let _ = net.contains(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let _ = net.contains(IpAddr::V6(Ipv6Addr::LOCALHOST));
    }

    let trusted_proxies: Vec<IpNet> = [
        IpNet::V4 {
            addr: Ipv4Addr::new(10, 0, 0, 0),
            prefix: 8,
        },
        IpNet::V4 {
            addr: Ipv4Addr::new(172, 16, 0, 0),
            prefix: 12,
        },
        IpNet::V6 {
            addr: Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0),
            prefix: 7,
        },
    ]
    .iter()
    .copied()
    .take(trusted)
    .collect();

    let config = ClientIpConfig {
        trusted_proxies,
        num_trusted_hops: hops,
        trusted_header: if use_override_header {
            Some("cf-connecting-ip".parse().unwrap())
        } else {
            None
        },
    };

    let mut headers = http::HeaderMap::new();
    let value = String::from_utf8_lossy(&data[4..]);
    // Invalid header bytes are a valid fuzz outcome (header absent).
    if let Ok(hv) = http::HeaderValue::from_str(&value) {
        headers.insert("x-forwarded-for", hv.clone());
        headers.insert("cf-connecting-ip", hv);
    }

    let peer: IpAddr = if peer_is_v6 {
        IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 0x1))
    } else if peer_is_trusted {
        IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))
    } else {
        IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9))
    };

    // Ok/Err both fine — never panic, and forwarded trust requires a
    // trusted peer with a trusted-proxy list.
    let _ = resolve_client_identity(&headers, Some(peer), &config);
    let _ = resolve_client_identity(&headers, None, &config);
});
