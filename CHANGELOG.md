# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Spec suite (`SPEC-000` … `SPEC-008`) with numbered requirements that CI checks for test
  coverage.
- `rincon-core` — domain types whose invariants live in the type system, the private-address
  guard (closing the IPv4-mapped and IPv4-compatible IPv6 bypasses and the cloud-metadata
  address), the XML `DOCTYPE` guard, lock-free session counters, size-capped logging, and
  offline diagnostics bundles that are redacted as they are built.
- `rincon-audio` — WASAPI loopback capture, a lock-free drop-oldest ring proven
  allocation-free by a counting allocator, sample conversion with clipping and non-finite
  handling, and a deterministic synthetic backend so every downstream crate is testable
  without hardware.
- `rincon-discovery` — SSDP probing on every interface, datagram parsing lenient enough for
  real devices, and description parsing that resists the embedded sub-device trap.
- `rincon-control` — SOAP envelopes whose escaping is proven by property tests, double-escaped
  DIDL-Lite metadata, zone-group coordinator resolution, and one error variant per action a
  user can actually take.
- `rincon-stream` — the HTTP server, guarded by a peer allowlist, a 128-bit constant-time
  token, a connection cap, and exactly one route.
- `scripts/spec-guard.mjs` — fails the build when a requirement has no test, when a test names
  a requirement that does not exist, or when a structural claim the specs make about the
  repository stops being true.

### Changed

- **`SPEC-002` `RQ-STRM-002` corrected during implementation.** It originally required a WAV
  `data` chunk large enough that a 24-hour session would never reach it. A 32-bit RIFF field
  cannot express that — the ceiling is about 6 h 45 m at CD-quality stereo, and no WAV header
  can do better. The requirement now states the real property (declare the maximum the format
  allows, do not let the RIFF size wrap), and the new `RQ-STRM-015` asserts that the server
  keeps serving past the declared size, which is the behaviour that actually matters.

### Fixed

- The audio ring dropped frames without counting them when every block was in the consumer's
  hands. Silent data loss is a defect; the ring now reclaims the oldest filled block under
  pressure and attributes every lost frame to the `Ring` layer.

[Unreleased]: https://github.com/EmmiSiu/rincon/compare/main...HEAD
