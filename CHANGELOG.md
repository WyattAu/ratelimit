# Changelog

All notable changes to this project are documented here. Format: [Keep a
Changelog](https://keepachangelog.com/) — versions follow [semver](https://semver.org).

## [Unreleased]

## [0.4.1] - 2026-09-05

### Fixed
- Pinned proptest max_global_rejects in code — assume-throttle abort no longer masquerades as a test failure in CI/release gates

## [0.4.0] - 2026-09-05

### Security

- **Breaking:** the Tower layer no longer trusts `X-Forwarded-For`
  unconditionally. Client identity is now resolved by
  `client_ip::resolve_client_identity` (REQ-THROTTLE-100..104):
  forwarded headers are believed only for peers listed in
  `ClientIpConfig::trusted_proxies` (empty default = headers ignored,
  secure by default), via a right-to-left walk skipping
  `num_trusted_hops` entries; malformed/too-short headers fall back to
  the peer IP; requests without `ConnectInfo` fail closed with `503`
  (`MissingClientPolicy::Reject`) unless `FallbackKey` is opted into.
  Closes threat-model OPEN-1 (fresh GCRA budget per request via a
  client-set header). Requires axum routers to be served with
  `.into_make_service_with_connect_info::<SocketAddr>()`.
- Added `MissingClientPolicy` and a `KeyExtractor` override
  (`.with_key_extractor`) for non-IP keys (API keys, tenant ids).

### MIGRATION

- **Behind a proxy?** You must now configure `trusted_proxies` (CIDR
  list of your own proxies) and `num_trusted_hops` (how many rightmost
  XFF entries your infrastructure appends; default 1 fits
  `internet → ALB → nginx → app`, use 0 for a single direct proxy).
  See README "Behind a proxy".
- **Directly exposed?** Nothing to do — the default (socket address,
  headers ignored) is stricter and safe.
- Keying by API key? Switch from the implicit behavior to
  `RateLimitLayer::with_key_extractor`.

### Added

- `client_ip` module: `ClientIpConfig`, `MissingClientPolicy`, `IpNet`
  (dependency-free CIDR), `resolve_client_identity`; unit, boundary,
  property (independent-oracle), and fuzz coverage
  (`fuzz/fuzz_targets/fuzz_client_ip.rs`).
- REQUIREMENTS.md with REQ-THROTTLE-100..104 traceability.

## [0.3.0] - 2026-09-02

### Added

- SQLite-backed GCRA rate limiter (distributed-friendly persistence
  without Redis).

### Changed

- Performance: fewer clones on the check path.

## [0.2.0] - 2026-09-02

### Added

- Keyed multi-client limiting and a synchronous API.
- `SlidingWindowBackend` behind a feature flag.
- `Quota::from_parts` constructor for custom interval/burst.
- `metrics` feature flag: utilization histogram for rate limit checks.

## [0.1.0] - 2026-09-01

### Added

- GCRA algorithm — smooth, memory-efficient rate limiting.
- In-memory backend via `DashMap` (default) and optional Redis backend
  for distributed deployments.
- Tower `Layer` with `X-RateLimit-*` response headers.
- Configurable burst / token-bucket capacity and presets.
