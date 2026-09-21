# Security Policy

## Reporting a vulnerability

**Do not open a public issue.** Use GitHub's private reporting:
[Security → Report a vulnerability](https://github.com/EmmiSiu/rincon/security/advisories/new).

What helps:

- What an attacker gains, and what position they need to be in to get it.
- Reproduction steps, or a proof of concept.
- The version or commit you tested.
- A diagnostics bundle, if one is relevant. The app produces a redacted one.

What to expect:

| Stage | Target |
| ----- | ------ |
| Acknowledgement | 72 hours |
| Initial assessment | 7 days |
| Fix or mitigation for a confirmed high-severity issue | 30 days |
| Public advisory | after a fix ships, or 90 days, whichever comes first |

Rincon is a volunteer project with no bug bounty. Credit in the advisory is offered unless you
would rather not be named.

## What Rincon does with your data

Nothing leaves your network. There is no account, no telemetry, no analytics, no crash
reporting, and no update check. The app makes **zero** outbound internet connections in the
audio or control path, and every destination it dials is validated against a private-address
allowlist before a socket is opened.

Captured audio is never written to disk unless you explicitly ask for a debug dump, and then
only to a path you chose. Capture does not run unless a session is active.

A diagnostics bundle is built offline and redacted as it is built: the stream token, your
hostname, and the final octet of every IP address are removed before the text exists — so a
bundle on disk is safe by construction rather than by someone remembering.

## Threat model

The full analysis is [`specs/SPEC-007-security.md`](specs/SPEC-007-security.md). Summary:

**In scope** — an attacker already on your LAN who can send arbitrary UDP to our discovery
socket, answer our probes with a forged `LOCATION`, serve a hostile XML document, probe our
stream port, or ARP-spoof your speaker.

**Out of scope, deliberately:**

- A local attacker with code execution as you. They can read the audio device directly; Rincon
  is not a barrier and does not pretend to be one.
- Malicious Sonos firmware. If the speaker is hostile, your audio is going to a hostile device
  by definition — that is the feature.
- Physical access to an unlocked machine.

## Why the stream is not encrypted

This is the question a reviewer should ask, so the answer is written down rather than assumed.

Sonos players fetch stream URLs over plain HTTP and do not validate certificates in a way that
would make a self-signed one meaningful. Adding TLS would mean either installing a certificate
authority into your trust store — a worse security outcome than the problem it solves — or
serving a certificate nothing verifies, which is encryption without authentication and stops no
attacker who is actually on the path.

The honest position: **the stream is as confidential as your Wi-Fi is.** Rincon therefore
restricts who may connect, makes the URL unguessable, and never captures without an active
session. It does not claim encryption it does not provide. If Sonos ships verifiable HTTPS
stream ingestion, this decision gets revisited in an ADR.

## Supported versions

Pre-1.0: only the latest release receives fixes.

## Hardening already in place

| Concern | Mechanism |
| ------- | --------- |
| SSRF via a forged discovery response | Private-address allowlist before any socket opens; IPv4-mapped and IPv4-compatible IPv6 bypasses closed; cloud metadata refused |
| SSRF via redirect | Redirects disabled on every client |
| XXE, entity expansion | Every XML document is refused if it declares a `DOCTYPE` or an `ENTITY`, before a parser sees it |
| Memory exhaustion | Every network read has an explicit byte cap and deadline, declared in one auditable module |
| Parser crash | Property tests assert no input panics; fuzz targets run nightly |
| Unauthorised stream access | Peer allowlist, 128-bit token compared in constant time, `404` on a miss, one route and no other surface |
| XSS in the interface | Strict CSP with no `unsafe-inline` or `unsafe-eval`, no remote origins, text-only rendering, device names sanitised and truncated at the domain boundary |
| XML injection into device commands | Every interpolated value escaped by construction, property-tested against generated hostile input |
| Memory safety | `unsafe` denied workspace-wide; one test-only exemption, documented in an ADR |
| Supply chain | `cargo-deny` (licences, advisories, sources), lockfile committed, GitHub Actions pinned by commit SHA, releases built only by CI with provenance attestation |
