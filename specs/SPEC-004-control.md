---
id: SPEC-004
title: Device control (UPnP / SOAP)
status: accepted
owner: EmmiSiu
crate: rincon-control
depends_on: [SPEC-000, SPEC-003, SPEC-007]
supersedes: null
last_reviewed: 2026-09-20
---

# SPEC-004 — Device control

## 1. Problem

Once we have a stream URL and a device address, the speaker must be told to play it. Sonos
exposes the standard UPnP `AVTransport` and `RenderingControl` services over SOAP on port 1400.
Two subtleties make this more than "POST some XML":

1. **Grouping.** If the target speaker is grouped, only the *coordinator* accepts transport
   commands. Sending `Play` to a member is silently ignored or produces an error, which reads to
   the user as "the app is broken".
2. **Injection.** The stream URL and its DIDL-Lite metadata are interpolated into an XML
   document. Unescaped interpolation is a textbook injection bug, in a request we are sending to
   a device on the user's network.

## 2. Goals

- `SetAVTransportURI` + `Play` against the correct coordinator, reliably, first try.
- Volume read/write, transport stop, and current-state query.
- Every outbound XML document correctly escaped, with a test that proves it.
- Every response parsed defensively, including SOAP `Fault` bodies.

## 3. Non-goals

- Queue manipulation, favourites, alarms, or playlists. Rincon injects one live stream.
- GENA event subscriptions (`SUBSCRIBE`/`NOTIFY`) in v0.1; the engine polls instead.
- Music service authentication of any kind.

## 4. Interface contract

```rust
#[async_trait]
pub trait TransportControl: Send + Sync + fmt::Debug {
    async fn set_stream_uri(&self, target: &Device, uri: &StreamUrl, meta: &TrackMeta)
        -> Result<(), ControlError>;
    async fn play(&self, target: &Device) -> Result<(), ControlError>;
    async fn stop(&self, target: &Device) -> Result<(), ControlError>;
    async fn volume(&self, target: &Device) -> Result<Volume, ControlError>;
    async fn set_volume(&self, target: &Device, v: Volume) -> Result<(), ControlError>;
    /// Resolves the coordinator that actually accepts transport commands.
    async fn coordinator_of(&self, target: &Device) -> Result<Device, ControlError>;
}
```

`Volume` is a newtype clamped to `0..=100` at construction, so an out-of-range value cannot be
constructed anywhere in the program.

## 5. Requirements

| ID | Requirement | Verification | Covered by |
| -- | ----------- | ------------ | ---------- |
| `RQ-CTL-001` | A generated SOAP envelope MUST be byte-exact against the committed golden fixture for a given action and arguments. | `golden` | `crates/rincon-control/src/soap.rs:112`<br>`crates/rincon-control/src/soap.rs:253`<br>`crates/rincon-testkit/src/mock_sonos.rs:304` |
| `RQ-CTL-002` | Every interpolated value MUST be XML-escaped for `&`, `<`, `>`, `"`, and `'`. | `property` | `crates/rincon-control/src/soap.rs:112`<br>`crates/rincon-control/src/soap.rs:277` |
| `RQ-CTL-003` | A value containing `</s:Body>` or similar MUST NOT be able to break out of its element. | `property` | `crates/rincon-control/src/soap.rs:112`<br>`crates/rincon-control/src/soap.rs:289` |
| `RQ-CTL-004` | DIDL-Lite metadata MUST be escaped **twice** (it is an XML document embedded in an XML text node). | `golden` | `crates/rincon-control/src/didl.rs:51`<br>`crates/rincon-control/src/didl.rs:95` |
| `RQ-CTL-005` | The `SOAPACTION` header MUST be exactly `"<serviceType>#<action>"`, quoted. | `unit` | `crates/rincon-control/src/soap.rs:149`<br>`crates/rincon-control/src/soap.rs:261`<br>`crates/rincon-testkit/src/mock_sonos.rs:304` |
| `RQ-CTL-006` | A SOAP `Fault` response MUST be parsed into a typed `ControlError::Upnp { code, description }`, never treated as success. | `unit` | `crates/rincon-control/src/soap.rs:165`<br>`crates/rincon-control/src/soap.rs:313`<br>`crates/rincon-testkit/src/mock_sonos.rs:337` |
| `RQ-CTL-007` | UPnP error `701` (transition not available) and `714` (illegal MIME) MUST map to distinct, actionable error variants. | `unit` | `crates/rincon-control/src/error.rs:81`<br>`crates/rincon-control/src/error.rs:166`<br>`crates/rincon-testkit/src/mock_sonos.rs:337` |
| `RQ-CTL-008` | `coordinator_of` MUST return the coordinator when the target is a grouped member, and the target itself when standalone. | `unit` | `crates/rincon-control/src/lib.rs:453`<br>`crates/rincon-control/src/topology.rs:55`<br>`crates/rincon-control/src/topology.rs:105`<br>`crates/rincon-control/src/topology.rs:191`<br>`crates/rincon-testkit/src/mock_sonos.rs:351` |
| `RQ-CTL-009` | Control requests MUST be sent only to private addresses (shares the SPEC-003 guard). | `unit` | `crates/rincon-control/src/lib.rs:477`<br>`crates/rincon-core/src/net.rs:187`<br>`crates/rincon-core/src/net.rs:325` |
| `RQ-CTL-010` | Every request MUST carry a timeout of at most 5 s and MUST NOT retry non-idempotent actions automatically. | `integration` | `crates/rincon-control/src/lib.rs:402` |
| `RQ-CTL-011` | Response bodies MUST be capped at 1 MiB. | `integration` | `crates/rincon-control/src/lib.rs:432` |
| `RQ-CTL-012` | `Volume` MUST be unconstructible outside `0..=100`. | `property` | `crates/rincon-control/src/volume.rs:35`<br>`crates/rincon-control/src/volume.rs:110` |
| `RQ-CTL-013` | Zone-group topology parsing MUST reject a DOCTYPE, as description parsing does. | `unit` | `crates/rincon-control/src/soap.rs:165`<br>`crates/rincon-control/src/soap.rs:365`<br>`crates/rincon-control/src/topology.rs:105`<br>`crates/rincon-control/src/topology.rs:231`<br>`crates/rincon-core/src/xml.rs:46`<br>`crates/rincon-core/src/xml.rs:104` |

## 6. Failure modes

| Condition | Detection | Response | User-visible result |
| --------- | --------- | -------- | ------------------- |
| Speaker busy with another source | UPnP `701` | Surface as recoverable | "Kitchen is playing something else. Take over?" |
| Speaker rejects the stream MIME | UPnP `714` | Try the alternate framing once | transparent, then "This speaker rejected the audio format." |
| Target is a grouped member | topology query | Retarget the coordinator automatically | transparent |
| Device offline mid-session | connect error / timeout | `Unreachable`; engine tears the session down | "Lost connection to Kitchen." |
| Stale IP after DHCP change | connect refused | Trigger rediscovery by UDN | transparent when the UDN is found again |

## 7. Performance & resource budget

| Metric | Budget | Measured by |
| ------ | ------ | ----------- |
| `SetAVTransportURI` + `Play` round trip | < 400 ms on LAN | `rq_ctl_010` |
| Envelope construction | < 50 µs | `benches/soap.rs` |
| Allocations per command | ≤ 4 | `benches/soap.rs` |

## 8. Security considerations

- **XML injection** is the headline risk and is closed by `RQ-CTL-002/003/004` with property
  tests that generate hostile strings, not just a fixed list.
- **SSRF** shares the private-address validator with discovery (`RQ-CTL-009`).
- **Response parsing** is capped and DOCTYPE-free (`RQ-CTL-011/013`).
- **No credentials exist** in this path — a deliberate design property, not an omission. Sonos
  local control is unauthenticated on the LAN, so there is no secret for Rincon to mishandle.

## 9. Minimum Viable Test (MVT)

```bash
just mvt-control        # tells the discovered speaker to play a known-good test tone URL
```

Passes when the speaker audibly plays the tone. The CI equivalent asserts, against
`rincon-testkit::MockSonos`, that the exact expected SOAP bytes arrived in the expected order.

## 10. Open questions

- [ ] Is polling `GetTransportInfo` every 2 s acceptable, or does GENA eventing become necessary
      for responsive UI state once grouping changes mid-session?
