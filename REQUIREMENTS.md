# Requirements — throttle-kit

Numbered, testable requirements. Every requirement maps to at least one named
test; every security-relevant test cites at least one requirement. Doc
comments on the implementing public item carry `REQ-THROTTLE-NNN` tags.

Scope: GCRA rate limiting (lib) + Tower layer client-IP identity
(`tower` feature).

## Security

| ID | Requirement | Priority |
|----|-------------|----------|
| REQ-THROTTLE-100 | An untrusted direct peer is keyed by its socket address regardless of the content (or presence) of `X-Forwarded-For` or any override header; an **empty** `trusted_proxies` list means forwarded headers are ignored entirely (secure by default) | MUST |
| REQ-THROTTLE-101 | For a trusted direct peer, identity is the forwarded-header entry found by walking RIGHT-TO-LEFT, skipping `num_trusted_hops` entries (our proxy chain); entries left of the resolved one are never consulted, so attacker-controlled left entries cannot be selected when the hop count matches the real proxy chain | MUST |
| REQ-THROTTLE-102 | A malformed, missing, non-ASCII, or too-short forwarded header (chain ≤ `num_trusted_hops`) falls back to the peer IP and emits a `tracing::warn!` — it never panics and never walks further left | MUST |
| REQ-THROTTLE-103 | When no `ConnectInfo` is available, `MissingClientPolicy` applies: the default `Reject` fails closed with `503`; `FallbackKey` is an explicit opt-in that shares one bucket | MUST |
| REQ-THROTTLE-104 | CIDR membership is exact at prefix edges (0, 1, /31, /32, /128, /0); host bits in a constructed network are masked; v4-mapped IPv6 (`::ffff:a.b.c.d`) is evaluated as IPv4 and matches only IPv4 networks | MUST |

## Traceability Matrix (REQ-THROTTLE-100..104)

| Requirement | Test (fn, file) | Property class |
|-------------|-----------------|----------------|
| REQ-THROTTLE-100 | `req100_untrusted_peer_ignores_spoofed_xff`, `req100_default_config_ignores_headers` (`tests/client_ip.rs`); `layer_default_keys_by_peer_and_ignores_spoofed_xff` (service level) | unit |
| REQ-THROTTLE-100 | `prop_untrusted_peer_always_peer_socket` | property |
| REQ-THROTTLE-101 | `req101_right_to_left_walk_skips_hops`, `req101_chain_longer_than_hops_resolves_entry`, `req101_hops_zero_takes_rightmost_entry`, `req101_trusted_header_override`, `req101_ipv6_chain_entry_resolves`, `resolver_v4_mapped_peer_matches_v4_trust` (`tests/client_ip.rs`); `layer_trusted_proxy_resolves_forwarded_identity` (service level) | unit |
| REQ-THROTTLE-101 | `prop_adversarial_chain_matches_oracle` (independent right-to-left oracle) | property |
| REQ-THROTTLE-102 | `req102_missing_header_falls_back_to_peer`, `req102_empty_xff_falls_back_to_peer`, `req102_whitespace_and_junk_entries_fall_back_to_peer`, `req102_non_utf8_header_value_falls_back_to_peer`, `req101_chain_exactly_hops_falls_back_to_peer`, `req101_malformed_chosen_entry_falls_back_no_leftward_walk` (`tests/client_ip.rs`) | unit |
| REQ-THROTTLE-103 | `req103_no_peer_is_missing_identity`, `layer_missing_connect_info_rejects_with_503_by_default`, `layer_missing_connect_info_fallback_key_opt_in` (`tests/client_ip.rs`) | unit |
| REQ-THROTTLE-104 | `cidr_v4_prefix_zero_contains_all_v4`, `cidr_v4_slash32_exact_match_only`, `cidr_v4_slash31_contains_both_addresses`, `cidr_v4_prefix_boundaries`, `cidr_v6_full_and_zero`, `cidr_v6_prefix_spot_checks`, `cidr_v4_mapped_v6_treated_as_v4`, `cidr_direct_literal_struct_construction_masks_host_bits`, `parse_bare_ip_implies_max_prefix`, `parse_accepts_prefix_forms_and_trims`, `parse_normalizes_v4_mapped`, `parse_rejects_invalid_input` (`tests/client_ip.rs`) | unit |
| Robustness (all) | `prop_resolution_never_panics`; fuzz target `fuzz/fuzz_targets/fuzz_client_ip.rs` | property/fuzz |

## Test Count Delta (REQ-THROTTLE-100..104 addition)

- Before: 58 tests (`--all-features`: 29 lib + 19 integration + 8
  proptest + 2 doc) — no client-IP coverage; `extract_key` (former
  OPEN-1) was untested.
- Added: 35 test cases in `tests/client_ip.rs` (24 CIDR/resolver unit +
  5 service-level integration + 3 proptests) plus 1 fuzz target
  (`fuzz/fuzz_targets/fuzz_client_ip.rs`).
- After: 93 tests + 1 fuzz target.
