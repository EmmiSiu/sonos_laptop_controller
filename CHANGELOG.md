# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **The health indicator is measured rather than asserted.** `Streaming.health` was whatever the
  reducer wrote when the speaker connected — always `Good` — so `RQ-OBS-009` was satisfied by a
  pure function nothing called, and `Degraded` was unreachable outside the tests. The engine now
  samples the counters every 500 ms over a rolling two-second window and reports what they say.
- **Frames discarded before the speaker connects no longer count as lost audio.** On real
  hardware a session the operator heard perfectly reported 54% of frames dropped: capture
  necessarily runs for several seconds before anything fetches the stream, and the ring
  correctly discards that audio. It now has its own layer (`no-consumer`), is still counted and
  still appears in the diagnostics bundle, and is excluded from the health model.
- **The interface no longer computes its own, different, drop total.** It summed
  `dropped_ring + dropped_socket`, silently omitting capture and device loss, which would have
  disagreed with the health indicator beside it. The number now comes from one place.

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
