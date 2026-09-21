# Security

Rincon's security material lives in three places, each for a different reader.

| Document | For | Contains |
| -------- | --- | -------- |
| [`../SECURITY.md`](../SECURITY.md) | A user or a reporter | How to report a vulnerability, what the app does with your data, what is out of scope |
| [`../specs/SPEC-007-security.md`](../specs/SPEC-007-security.md) | A reviewer | The threat model: assets, trust boundaries, the attack/mitigation matrix, 15 numbered requirements |
| [`adr/0006-no-tls-on-the-stream.md`](adr/0006-no-tls-on-the-stream.md) | Anyone asking "why no HTTPS?" | The decision, the rejected alternatives, and the limitation stated plainly |

This page exists so that none of them has to repeat the others.

---

## The one-paragraph version

Rincon captures everything you hear and serves it from an open port on your home network. The
attacker we defend against is one who is already on that LAN: they can answer our discovery
probe with a forged address, serve a hostile XML document, or probe our stream port. Every
input from them is validated, capped, and parsed without a DTD; every destination we dial is
checked against a private-address allowlist; the stream is reachable only by the speaker you
chose, at a URL with 128 bits of entropy, compared in constant time. The app makes no outbound
internet connection at all.

The stream is not encrypted, and [ADR-0006](adr/0006-no-tls-on-the-stream.md) explains why that
is a decision rather than an omission — along with the limitation it leaves in place.

---

## Where each defence lives in the code

A reviewer wanting to check the claims rather than read about them:

| Claim | Where |
| ----- | ----- |
| No public address is ever dialled | `crates/rincon-core/src/net.rs` — including the IPv4-mapped and IPv4-compatible IPv6 bypasses, and the cloud metadata address |
| No XML parse sees a DOCTYPE | `crates/rincon-core/src/xml.rs`, called before every parser |
| Every network read is bounded | `crates/rincon-core/src/limits.rs` — every cap and deadline in one auditable file |
| The stream token never reaches a log or the WebView | `crates/rincon-core/src/stream_url.rs`, `crates/rincon-stream/src/token.rs` |
| Only the chosen speaker may connect | `crates/rincon-stream/src/config.rs` |
| Nothing interpolated into SOAP can escape its element | `crates/rincon-control/src/soap.rs` |
| Capture cannot outlive a session | `crates/rincon-engine/src/driver.rs`, `crates/rincon-audio/src/lib.rs` |
| The renderer cannot ask us to dial a host | `crates/rincon-ipc/src/lib.rs` |

Each of those has a test annotated with the requirement it proves, and `just spec-guard` fails
the build if one loses its test.

---

## Running the security checks

```bash
just deny       # licences, advisories, sources, bans
just audit      # known vulnerabilities, Rust and npm
just fuzz parse_ssdp 300
just spec-guard # every security requirement still has a test
```

CI runs the first three on every pull request and again on a daily schedule, so an advisory
published overnight turns the build red without anyone pushing a commit.
