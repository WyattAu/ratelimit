# Changelog

All notable changes to this project are documented here. Format: [Keep a
Changelog](https://keepachangelog.com/) — versions follow [semver](https://semver.org).

## [Unreleased]

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
