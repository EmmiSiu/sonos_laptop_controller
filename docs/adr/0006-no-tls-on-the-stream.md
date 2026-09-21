# ADR-0006 — No TLS on the audio stream

**Status:** accepted · **Date:** 2026-09-20

## Context

Rincon serves the user's system audio over HTTP on their LAN. Any reviewer will ask why it is
not HTTPS, and "we did not get to it" would be the wrong answer — this is a decision, and it
should be recorded as one.

## Options

**Self-signed certificate.** Serve HTTPS with a certificate generated per session. Sonos
players do not validate certificates for stream URLs in a way that makes this meaningful, so it
provides encryption without authentication. An attacker who can get on the path — the only
attacker encryption would stop — can substitute their own certificate and the player will
accept it.

**Install a certificate authority into the user's trust store.** Makes the certificate
verifiable. Also installs a CA on the user's machine whose private key sits in their profile,
which is a substantially worse security outcome than the problem it solves. A music utility has
no business doing this.

**Plain HTTP, with the limitation stated.** What we do.

## Decision

Plain HTTP, with four compensating controls and an honest statement of what they do not cover.

## The compensating controls

1. **Bind scope** — a specific LAN interface. The wildcard requires calling a method literally
   named `allow_wildcard_bind()`.
2. **Peer allowlist** — only the selected speaker's address, with IPv4-mapped IPv6 normalised so
   the rule can be neither bypassed nor accidentally self-defeated.
3. **Unguessable URL** — 128 bits of OS entropy per session, compared in constant time, `404`
   on a miss so a prober cannot distinguish a wrong token from a missing route.
4. **No capture without a session** — the audio device is not open when nothing is streaming.

## What this does not cover

**Passive Wi-Fi sniffing.** On an open network, or one whose key an attacker has, the stream is
readable. The honest statement is: *the stream is as confidential as the user's Wi-Fi is.*

That sentence is in `SECURITY.md` and in SPEC-007 §7. Writing it down is the point of this
record — the failure mode we are avoiding is not the absence of TLS, it is implying an
encryption guarantee we do not provide.

**Active ARP spoofing.** Listed as accepted residual risk in SPEC-007 §8.

## Consequences

**Good.** No certificate lifecycle, no trust-store manipulation, no TLS dependency in a program
that parses untrusted input, and no false sense of security.

**Bad.** A security-conscious user on a shared network has to decide whether that is acceptable.
They can, because we told them.

## Revisit if

Sonos ships verifiable HTTPS stream ingestion, or a firmware generation appears that validates
certificates against a real trust chain. Either would make TLS meaningful rather than
decorative.
