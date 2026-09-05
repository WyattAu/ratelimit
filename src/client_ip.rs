//! Client identity resolution for rate limiting behind proxies.
//!
//! The rate-limit key derived from a request is attacker-controllable
//! territory: any client can set `X-Forwarded-For` (XFF) and, if the
//! service believes it blindly, mint a fresh GCRA budget per request
//! (the threat model's former OPEN-1, a complete bypass). This module
//! resolves a client identity that is secure by default.
//!
//! # Resolution algorithm (spec)
//!
//! Given the request's direct peer address (from
//! `ConnectInfo<SocketAddr>` in the request extensions) and the
//! [`ClientIpConfig`]:
//!
//! 1. Direct peer NOT in `trusted_proxies` → client = peer IP (headers
//!    are lies). An **empty** `trusted_proxies` list therefore means the
//!    forwarded header is ignored entirely.
//! 2. Direct peer trusted → parse the forwarded header (`XFF`, or the
//!    `trusted_header` override), split on `,`, trim each entry, walk
//!    RIGHT-TO-LEFT skipping `num_trusted_hops` entries (our proxy
//!    chain), and take the next entry leftward as the client. NEVER
//!    trust left-to-right: every entry left of the resolved one is
//!    attacker-controlled. Malformed header (missing, non-ASCII, or a
//!    chosen entry that is not an IP address), missing header, or chain
//!    shorter than `num_trusted_hops` → fall back to the peer IP and
//!    emit a `tracing::warn!`.
//! 3. No `ConnectInfo` available → the caller applies
//!    [`MissingClientPolicy`] (default [`MissingClientPolicy::Reject`]
//!    → fail closed with `503`).
//!
//! # What `num_trusted_hops` counts
//!
//! The rightmost header entries were appended by *our own trusted
//! infrastructure* (each trusted proxy appends the address it saw).
//! `num_trusted_hops` is how many of those rightmost entries belong to
//! our proxy chain **in front of the direct peer** and must be skipped:
//!
//! - `internet → ALB → nginx → app` (both ALB and nginx trusted, nginx
//!   is the direct peer): nginx appends the ALB's address, so XFF is
//!   `"client, alb-ip"` and the default `num_trusted_hops = 1` skips
//!   `alb-ip`, yielding `client`.
//! - `internet → nginx → app` (nginx is the only proxy and the direct
//!   peer): XFF is `"client"`; set `num_trusted_hops = 0` to take the
//!   rightmost entry. Leaving the default `1` here falls back to the
//!   peer (nginx's) address — all clients then share one bucket. That
//!   is *safe but coarse*; never compensate by trusting entries further
//!   left than your own infrastructure actually appends.
//!
//! Spoofed entries always sit LEFT of the entries our proxies appended,
//! so a correctly configured hop count never reads them.
//!
//! # IPv4-mapped IPv6
//!
//! v4-mapped IPv6 addresses (`::ffff:a.b.c.d`) are **treated as IPv4**
//! everywhere: for [`IpNet`] membership and for the resolved identity.
//! A dual-stack socket reporting the peer as `::ffff:203.0.113.9` thus
//! matches a `203.0.113.0/24` trust entry and yields the key
//! `"203.0.113.9"`. They never match IPv6 networks.
//!
//! # axum plumbing
//!
//! The peer address comes from `ConnectInfo<SocketAddr>`, which axum
//! only injects when the router is served with
//! `.into_make_service_with_connect_info::<SocketAddr>()`. Without it,
//! step 3 applies: requests are rejected (default) or share a fallback
//! bucket.

use std::borrow::Cow;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use http::{HeaderMap, HeaderName};

/// Errors from [`IpNet::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientIpError {
    /// The address portion is not a valid IP address.
    #[error("invalid IP address: {0}")]
    InvalidIp(String),
    /// The prefix length is not a number or exceeds the address family's
    /// width (32 for IPv4, 128 for IPv6).
    #[error("invalid prefix length: {0}")]
    InvalidPrefix(String),
}

/// An IP network: CIDR prefix membership without any new dependency.
///
/// Construct with [`IpNet::parse`] (`"10.0.0.0/8"`, or a bare IP which
/// becomes `/32` / `/128`) or the struct literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IpNet {
    /// An IPv4 CIDR range.
    V4 {
        /// Network address. Host bits beyond `prefix` are ignored by
        /// [`IpNet::contains`].
        addr: Ipv4Addr,
        /// Prefix length, 0–32.
        prefix: u8,
    },
    /// An IPv6 CIDR range.
    V6 {
        /// Network address. Host bits beyond `prefix` are ignored by
        /// [`IpNet::contains`].
        addr: Ipv6Addr,
        /// Prefix length, 0–128.
        prefix: u8,
    },
}

impl IpNet {
    /// Parse `"10.0.0.0/8"`, `"2001:db8::/32"`, or a bare IP (implicit
    /// `/32` for IPv4, `/128` for IPv6). A v4-mapped IPv6 address
    /// (`::ffff:10.0.0.1`) is normalized to IPv4.
    pub fn parse(s: &str) -> Result<Self, ClientIpError> {
        let (addr_str, prefix) = match s.split_once('/') {
            Some((addr, prefix)) => {
                let prefix = prefix
                    .trim()
                    .parse::<u8>()
                    .map_err(|_| ClientIpError::InvalidPrefix(prefix.to_string()))?;
                (addr, Some(prefix))
            }
            None => (s, None),
        };

        let addr: IpAddr = addr_str
            .trim()
            .parse()
            .map_err(|_| ClientIpError::InvalidIp(addr_str.to_string()))?;
        let addr = normalize_ip(addr);

        match (addr, prefix) {
            (IpAddr::V4(addr), None) => Ok(IpNet::V4 { addr, prefix: 32 }),
            (IpAddr::V4(addr), Some(prefix)) if prefix <= 32 => Ok(IpNet::V4 {
                addr: mask_v4(addr, prefix),
                prefix,
            }),
            (IpAddr::V4(_), Some(prefix)) => Err(ClientIpError::InvalidPrefix(prefix.to_string())),
            (IpAddr::V6(addr), None) => Ok(IpNet::V6 { addr, prefix: 128 }),
            (IpAddr::V6(addr), Some(prefix)) if prefix <= 128 => Ok(IpNet::V6 {
                addr: mask_v6(addr, prefix),
                prefix,
            }),
            (IpAddr::V6(_), Some(prefix)) => Err(ClientIpError::InvalidPrefix(prefix.to_string())),
        }
    }

    /// Exact CIDR membership at the prefix edge.
    ///
    /// A v4-mapped IPv6 argument is evaluated as IPv4 (see [module
    /// docs](self)), so it matches IPv4 networks only. Cross-family
    /// membership is otherwise `false`.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = normalize_ip(ip);
        match (self, ip) {
            (IpNet::V4 { addr, prefix }, IpAddr::V4(other)) => {
                let mask = v4_mask(*prefix);
                mask & u32::from(*addr) == mask & u32::from(other)
            }
            (IpNet::V6 { addr, prefix }, IpAddr::V6(other)) => {
                let mask = v6_mask(*prefix);
                mask & u128::from(*addr) == mask & u128::from(other)
            }
            _ => false,
        }
    }
}

/// Contiguous prefix mask for IPv4. No shift overflow: prefix 0 → 0,
/// prefix ≥ 32 → all ones.
fn v4_mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else if prefix >= 32 {
        u32::MAX
    } else {
        u32::MAX << (32 - prefix)
    }
}

/// Contiguous prefix mask for IPv6. No shift overflow: prefix 0 → 0,
/// prefix ≥ 128 → all ones.
fn v6_mask(prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else if prefix >= 128 {
        u128::MAX
    } else {
        u128::MAX << (128 - prefix)
    }
}

fn mask_v4(addr: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    Ipv4Addr::from(v4_mask(prefix) & u32::from(addr))
}

fn mask_v6(addr: Ipv6Addr, prefix: u8) -> Ipv6Addr {
    Ipv6Addr::from(v6_mask(prefix) & u128::from(addr))
}

/// v4-mapped IPv6 (`::ffff:a.b.c.d`) is treated as IPv4 everywhere.
fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        other => other,
    }
}

/// Configuration for deriving the rate-limit key from the client IP.
///
/// See the [module docs](self) for the resolution algorithm.
///
/// The default is secure by default: no trusted proxies means the
/// forwarded header is ignored and every client is keyed by its socket
/// address.
#[derive(Debug, Clone, Default)]
pub struct ClientIpConfig {
    /// Peers whose forwarded header we may believe. EMPTY DEFAULT =
    /// forwarded header ignored entirely (secure by default).
    pub trusted_proxies: Vec<IpNet>,
    /// Right-to-left walk: skip this many entries (our proxy chain), the
    /// next entry leftward is the client. NEVER trust left-to-right.
    /// Default 1.
    pub num_trusted_hops: usize,
    /// Header override for platforms with dedicated headers
    /// (`CF-Connecting-IP`, `True-Client-IP`). Same trust rules apply.
    /// Default `None` = `X-Forwarded-For`.
    pub trusted_header: Option<HeaderName>,
}

/// Policy for requests whose client identity cannot be resolved
/// (algorithm step 3: no `ConnectInfo` in the request extensions).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MissingClientPolicy {
    /// Default: fail closed with `503` when identity is unresolvable.
    #[default]
    Reject,
    /// Explicit opt-in fallback key (shared bucket). All such requests
    /// are rate limited as one identity.
    FallbackKey(Cow<'static, str>),
}

/// Where a resolved client identity came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientIpSource {
    /// The direct peer's socket address (header ignored or used as
    /// fallback).
    PeerSocket,
    /// An entry from the forwarded header, after the trust walk.
    ForwardedHeader,
}

/// A successfully resolved client identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedClient {
    /// The client IP. v4-mapped IPv6 is normalized to IPv4.
    pub ip: IpAddr,
    /// Whether this came from the socket or the forwarded header.
    pub source: ClientIpSource,
}

/// No peer address was available (no `ConnectInfo` in the request
/// extensions) — the caller applies its [`MissingClientPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingClientIdentity;

/// Resolve the client identity for a request.
///
/// Implements the algorithm in the [module docs](self) verbatim:
/// REQ-THROTTLE-100 (untrusted peer → socket address regardless of
/// headers), REQ-THROTTLE-101 (trusted peer → right-to-left walk minus
/// hops), REQ-THROTTLE-102 (malformed/missing header → peer IP), and
/// REQ-THROTTLE-103 (no peer → `Err(MissingClientIdentity)`, caller
/// applies [`MissingClientPolicy`]).
///
/// `peer` is the direct peer address, from
/// [`peer_ip_from_extensions`] (axum `ConnectInfo<SocketAddr>`).
pub fn resolve_client_identity(
    headers: &HeaderMap,
    peer: Option<IpAddr>,
    config: &ClientIpConfig,
) -> Result<ResolvedClient, MissingClientIdentity> {
    // Step 3: no ConnectInfo — the caller decides.
    let peer = peer.ok_or(MissingClientIdentity)?;

    // Step 1: untrusted (or no) trusted_proxies — headers are lies.
    let peer_trusted = !config.trusted_proxies.is_empty()
        && config.trusted_proxies.iter().any(|net| net.contains(peer));
    if !peer_trusted {
        return Ok(ResolvedClient {
            ip: normalize_ip(peer),
            source: ClientIpSource::PeerSocket,
        });
    }

    // Step 2: trusted peer — walk the forwarded header right-to-left.
    // `split` is double-ended: rev() + nth(hops) is the spec's
    // "take entries[len-1-hops]" walk with no intermediate allocation,
    // and yields nothing when the chain is shorter than the hop count.
    let header_value = match config.trusted_header.as_ref() {
        Some(name) => headers.get(name),
        None => headers.get("x-forwarded-for"),
    };
    let resolved = header_value
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').rev().nth(config.num_trusted_hops))
        .map(str::trim)
        .and_then(|entry| entry.parse().ok());

    match resolved {
        Some(ip) => Ok(ResolvedClient {
            ip: normalize_ip(ip),
            source: ClientIpSource::ForwardedHeader,
        }),
        None => {
            tracing::warn!(
                target: "throttle_kit::client_ip",
                "malformed or too-short forwarded header; falling back to peer address"
            );
            Ok(ResolvedClient {
                ip: normalize_ip(peer),
                source: ClientIpSource::PeerSocket,
            })
        }
    }
}

/// Extract the direct peer IP from axum's `ConnectInfo<SocketAddr>`
/// request extension, if present.
///
/// The router must be served with
/// `.into_make_service_with_connect_info::<SocketAddr>()` for this to
/// find anything.
pub fn peer_ip_from_extensions(extensions: &http::Extensions) -> Option<IpAddr> {
    extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|connect_info| connect_info.0.ip())
}

/// Convenience wrapper: resolve the client identity for a full request.
pub fn resolve_from_request<B>(
    request: &http::Request<B>,
    config: &ClientIpConfig,
) -> Result<ResolvedClient, MissingClientIdentity> {
    resolve_client_identity(
        request.headers(),
        peer_ip_from_extensions(request.extensions()),
        config,
    )
}
