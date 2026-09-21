---
id: SPEC-002
title: HTTP stream server
status: accepted
owner: EmmiSiu
crate: rincon-stream
depends_on: [SPEC-000, SPEC-001, SPEC-007]
supersedes: null
last_reviewed: 2026-09-20
---

# SPEC-002 — HTTP stream server

## 1. Problem

A Sonos player will not accept a push. It is an HTTP *client*: you hand it a URL and it fetches
the audio itself. So the laptop must become a server on the LAN, serving a never-ending audio
response that begins the moment the speaker connects and ends when the user stops.

This inverts the usual threat model. We are opening a listening socket on a home network and
inviting a device to pull our microphone-adjacent system audio from it. The server is therefore
the single most security-sensitive component in the project.

## 2. Goals

- Serve an unbounded PCM stream that a Sonos player accepts on first try.
- Start emitting bytes within 50 ms of the speaker's `GET`.
- Survive the speaker disconnecting and reconnecting without restarting capture.
- Be hostile by default: unguessable URL, peer allowlist, connection cap, no directory surface.

## 3. Non-goals

- TLS. Sonos players do not validate certificates for stream URLs and a self-signed cert on a
  LAN adds a trust-store problem without adding a defence. Confidentiality is provided by the
  allowlist plus the unguessable path; see SPEC-007 §Rationale.
- Range requests / seeking. The stream is live; there is nothing to seek to.
- Serving more than one concurrent consumer per session in v0.1.

## 4. Wire format

Default framing is **L16 PCM inside a WAV container with a maximally sized data chunk**.

The 44-byte RIFF header declares `data` size `0xFFFF_FFFF - 44` and the server then ignores it,
serving until the user stops or the socket closes.

> **A 32-bit field cannot describe an endless stream.** At 44.1 kHz stereo 16-bit, the largest
> declarable `data` chunk is 4.29 GB — about **6 h 45 m** of audio, not the 24 h an earlier draft
> of this spec assumed. No WAV header can express more; RF64 and Wave64 can, and Sonos accepts
> neither. Players stream until the socket closes rather than counting bytes, which is why every
> WAV-over-HTTP streamer works this way. `RQ-STRM-015` asserts we do not stop at the boundary;
> if a firmware is found that enforces it, `Framing::Chunked` is the escape hatch.

This is the framing with the widest Sonos firmware compatibility. Chunked transfer encoding is
implemented behind `Framing::Chunked` but is not the default until SPEC-000's open question is
resolved on real hardware.

```
GET /s/<token>/stream.wav

HTTP/1.1 200 OK
Content-Type: audio/x-wav
Accept-Ranges: none
Cache-Control: no-store
Connection: close
X-Content-Type-Options: nosniff

<44-byte RIFF header><interleaved little-endian i16 samples ...>
```

## 5. Interface contract

```rust
pub struct StreamServer { /* ... */ }

pub struct StreamConfig {
    pub bind: SocketAddr,          // the LAN interface, never 0.0.0.0 by accident
    pub allowed_peers: Vec<IpAddr>,// the target player, plus loopback for diagnostics
    pub format: AudioFormat,
    pub framing: Framing,
    pub max_connections: usize,
}

impl StreamServer {
    pub async fn bind(config: StreamConfig, source: FrameReceiver)
        -> Result<Handle, StreamError>;
}

pub struct Handle {
    pub url: StreamUrl,            // http://<lan-ip>:<port>/s/<token>/stream.wav
    pub local_addr: SocketAddr,
    /* shutdown, stats */
}
```

## 6. Requirements

| ID | Requirement | Verification | Covered by |
| -- | ----------- | ------------ | ---------- |
| `RQ-STRM-001` | The WAV header MUST be byte-exact for a given `AudioFormat`, matching the committed golden fixture. | `golden` | `rincon_stream::wav::tests::rq_strm_001_header_matches_golden` |
| `RQ-STRM-002` | The declared `data` chunk size MUST be the largest value a 32-bit RIFF field can express, and the RIFF chunk size MUST NOT wrap. | `unit` | `rincon_stream::wav::tests::rq_strm_002_data_chunk_is_the_largest_the_format_allows` |
| `RQ-STRM-015` | The server MUST keep serving after the stream passes the declared `data` size; a session MUST NOT terminate at that boundary. | `integration` | `rincon_stream::tests::rq_strm_015_serving_continues_past_the_declared_size` |
| `RQ-STRM-003` | The stream path MUST contain at least 128 bits of cryptographically random entropy, generated per session. | `unit` | `rincon_stream::token::tests::rq_strm_003_token_has_128_bits_of_entropy` |
| `RQ-STRM-004` | A request from an IP that is not in `allowed_peers` MUST be refused with `403` and MUST NOT reach the audio source. | `integration` | `rincon_stream::tests::rq_strm_004_rejects_unallowlisted_peer` |
| `RQ-STRM-005` | Token comparison MUST be constant-time with respect to the token value. | `unit` | `rincon_stream::token::tests::rq_strm_005_comparison_is_constant_time` |
| `RQ-STRM-006` | A request with a wrong token MUST return `404` (not `403`), so a prober cannot distinguish "wrong token" from "no such route". | `integration` | `rincon_stream::tests::rq_strm_006_wrong_token_is_indistinguishable_from_404` |
| `RQ-STRM-007` | The server MUST refuse to bind to `0.0.0.0` unless `allow_wildcard_bind` is explicitly set. | `unit` | `rincon_stream::config::tests::rq_strm_007_wildcard_bind_requires_opt_in` |
| `RQ-STRM-008` | Concurrent connections beyond `max_connections` MUST be rejected with `503` without disturbing the active stream. | `integration` | `rincon_stream::tests::rq_strm_008_enforces_connection_cap` |
| `RQ-STRM-009` | A consumer disconnecting MUST NOT terminate the server or the capture session. | `integration` | `rincon_stream::tests::rq_strm_009_survives_consumer_disconnect` |
| `RQ-STRM-010` | A `HEAD` request MUST return the same headers as `GET` with no body. | `integration` | `rincon_stream::tests::rq_strm_010_head_returns_headers_without_body` |
| `RQ-STRM-011` | When the ring underruns, the server MUST emit digital silence rather than stalling the socket or closing the connection. | `integration` | `rincon_stream::tests::rq_strm_011_underrun_emits_silence_not_stall` |
| `RQ-STRM-012` | The server MUST NOT expose any route other than the tokenised stream path: `/`, `/..`, and any traversal attempt MUST return `404`. | `integration` | `rincon_stream::tests::rq_strm_012_no_other_routes_exist` |
| `RQ-STRM-013` | Response headers MUST include `X-Content-Type-Options: nosniff` and MUST NOT include a `Server` banner revealing the version. | `integration` | `rincon_stream::tests::rq_strm_013_security_headers_present` |
| `RQ-STRM-014` | Bytes served MUST equal bytes read from the ring, modulo injected silence, with no duplication or reordering. | `integration` | `rincon_stream::tests::rq_strm_014_byte_stream_is_faithful` |

## 7. Failure modes

| Condition | Detection | Response | User-visible result |
| --------- | --------- | -------- | ------------------- |
| Port already in use | `bind()` → `AddrInUse` | Retry on an ephemeral port | transparent |
| Windows Firewall blocks inbound | Speaker never connects within 8 s | `PeerNeverConnected` | Firewall remediation panel (SPEC-006 `RQ-UI-007`) |
| Speaker drops mid-session (Wi-Fi blip) | Connection closed | Keep serving; speaker re-`GET`s the same URL | Health indicator amber, then green |
| Ring underrun | `read_frames` returns short | Emit silence, count it | Health indicator amber |

## 8. Performance & resource budget

| Metric | Budget | Measured by |
| ------ | ------ | ----------- |
| `GET` → first audio byte | < 50 ms | `rq_strm_014` timing assertion |
| Per-connection heap | < 256 KiB | `benches/stream.rs` |
| CPU while streaming 48 kHz stereo | < 3% of one core | manual |
| Copies per sample, ring → socket | ≤ 1 | code review + `benches/stream.rs` |

## 9. Security considerations

The full analysis lives in SPEC-007. In summary, the server is protected by four independent
layers, any one of which is sufficient to stop a casual LAN attacker:

1. **Bind scope** — a specific LAN interface, never the wildcard by default (`RQ-STRM-007`).
2. **Peer allowlist** — only the selected player's IP (`RQ-STRM-004`).
3. **Unguessable path** — 128-bit per-session token, constant-time compare (`RQ-STRM-003/005`).
4. **Zero other surface** — one route, no listings, no verbose errors (`RQ-STRM-012`).

## 10. Minimum Viable Test (MVT)

```bash
just mvt-stream         # serves system audio; open the printed URL in VLC
```

Passes when VLC plays the laptop's audio from `http://<lan-ip>:<port>/s/<token>/stream.wav`,
and a second client from a non-allowlisted IP receives `403`.

## 11. Open questions

- [ ] Some firmware sends `Range: bytes=0-` on the first `GET`. Confirm that answering `200`
      (not `206`) is accepted across firmware generations.
- [ ] Does injecting silence on underrun beat closing the connection, from the speaker's point
      of view? Measure reconnect cost on real hardware.
