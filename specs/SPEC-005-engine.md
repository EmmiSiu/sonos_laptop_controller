---
id: SPEC-005
title: Session orchestration
status: accepted
owner: EmmiSiu
crate: rincon-engine
depends_on: [SPEC-001, SPEC-002, SPEC-003, SPEC-004]
supersedes: null
last_reviewed: 2026-09-20
---

# SPEC-005 — Session orchestration

## 1. Problem

Four modules must be sequenced correctly under failure: capture must be running before the
server has anything to serve; the server must be bound before the speaker is handed a URL; the
URL must be reachable from the speaker's subnet; and any of the four can fail or vanish at any
moment. Written as ad-hoc `async` code, this becomes an unreviewable tangle of `if let Err`.

Rincon instead models the session as an **explicit state machine with a pure transition
function**. Every possible transition is enumerated in one `match`, is unit-testable without
sockets or audio hardware, and is impossible to reach by accident.

## 2. Goals

- One authoritative `SessionState` that the UI renders directly, with no derived duplicate.
- A pure reducer `fn step(state, event) -> (state, Vec<Effect>)` — deterministic, no I/O.
- Effects executed by a thin driver whose only job is calling the other crates.
- Every failure path produces a state the UI can render, never a silent hang.

## 3. Non-goals

- Multiple simultaneous sessions to different rooms. One laptop, one target, in v0.1.
- Persisting sessions across app restarts.

## 4. State machine

```mermaid
stateDiagram-v2
    [*] --> Idle
    Idle --> Scanning: StartScan
    Scanning --> DevicesFound: ScanCompleted(devices)
    Scanning --> NoDevices: ScanCompleted(empty)
    NoDevices --> Scanning: StartScan
    DevicesFound --> Scanning: StartScan
    DevicesFound --> Preparing: Connect(device)
    Preparing --> Streaming: PlaybackConfirmed
    Preparing --> Failed: PrepareFailed(reason)
    Streaming --> Degraded: HealthDropped(reason)
    Degraded --> Streaming: HealthRecovered
    Streaming --> Stopping: Disconnect
    Degraded --> Stopping: Disconnect
    Degraded --> Failed: PeerLost
    Stopping --> Idle: TeardownComplete
    Failed --> Idle: Acknowledge
    Failed --> Preparing: Retry
```

`Preparing` expands into an ordered effect chain; each step's failure is attributable:

```mermaid
flowchart TD
    P0[Preparing] --> P1[resolve coordinator]
    P1 --> P2[start audio capture]
    P2 --> P3[bind stream server on the interface that reaches the speaker]
    P3 --> P4[SetAVTransportURI]
    P4 --> P5[Play]
    P5 --> P6{peer connected within 8 s?}
    P6 -- yes --> S[Streaming]
    P6 -- no --> F["Failed(FirewallSuspected)"]
```

## 5. Interface contract

```rust
pub enum SessionState {
    Idle,
    Scanning,
    DevicesFound { devices: Vec<Device> },
    NoDevices,
    Preparing { target: Device, step: PrepareStep },
    Streaming { target: Device, url: StreamUrl, health: Health },
    Degraded  { target: Device, reason: DegradeReason },
    Stopping  { target: Device },
    Failed    { reason: FailureReason, retryable: bool },
}

/// Pure. No I/O, no clock, no allocation beyond the returned effects.
#[must_use]
pub fn step(state: SessionState, event: Event) -> Transition;

pub struct Transition { pub state: SessionState, pub effects: Vec<Effect> }
```

## 6. Requirements

| ID | Requirement | Verification | Covered by |
| -- | ----------- | ------------ | ---------- |
| `RQ-ENG-001` | `step` MUST be pure: identical `(state, event)` inputs MUST produce identical outputs. | `property` | `crates/rincon-engine/src/machine.rs:417`<br>`crates/rincon-engine/src/machine.rs:749` |
| `RQ-ENG-002` | Every `(state, event)` pair MUST be handled; an inapplicable event MUST leave the state unchanged and emit no effects. | `property` | `crates/rincon-engine/src/machine.rs:417`<br>`crates/rincon-engine/src/machine.rs:764`<br>`crates/rincon-engine/src/machine.rs:872` |
| `RQ-ENG-003` | The preparation chain MUST run in the order: resolve coordinator → start capture → bind server → set URI → play. | `unit` | `crates/rincon-engine/src/machine.rs:417`<br>`crates/rincon-engine/src/machine.rs:895` |
| `RQ-ENG-004` | The stream server MUST bind to the local address that reached the target during discovery, not to an arbitrary interface. | `unit` | `crates/rincon-engine/src/driver.rs:352`<br>`crates/rincon-engine/src/machine.rs:417`<br>`crates/rincon-engine/src/machine.rs:934` |
| `RQ-ENG-005` | If the peer has not connected within 8 s of `Play`, the state MUST become `Failed(FirewallSuspected)`, which is retryable. | `unit` | `crates/rincon-engine/src/driver.rs:395`<br>`crates/rincon-engine/src/lib.rs:272`<br>`crates/rincon-engine/src/machine.rs:417`<br>`crates/rincon-engine/src/machine.rs:957` |
| `RQ-ENG-006` | Leaving `Streaming` or `Degraded` by any path MUST emit teardown effects for both capture and server; no effect may be skipped on the error path. | `property` | `crates/rincon-engine/src/machine.rs:417`<br>`crates/rincon-engine/src/machine.rs:983` |
| `RQ-ENG-007` | A transient underrun MUST move the session to `Degraded`, not `Failed`, and MUST auto-recover. | `unit` | `crates/rincon-engine/src/machine.rs:417`<br>`crates/rincon-engine/src/machine.rs:1040` |
| `RQ-ENG-008` | Reconnection MUST use exponential backoff starting at 250 ms, capped at 8 s, with at most 6 attempts. | `unit` | `crates/rincon-engine/src/backoff.rs:45`<br>`crates/rincon-engine/src/backoff.rs:92` |
| `RQ-ENG-009` | Every state transition MUST emit exactly one observable `StateChanged` event to subscribers. | `unit` | `crates/rincon-engine/src/driver.rs:150`<br>`crates/rincon-engine/src/driver.rs:218`<br>`crates/rincon-engine/src/lib.rs:299` |
| `RQ-ENG-010` | A full connect → stream → disconnect cycle MUST leave zero live tasks, sockets, or capture streams. | `integration` | `crates/rincon-engine/src/driver.rs:218`<br>`crates/rincon-engine/src/lib.rs:231` |
| `RQ-ENG-011` | The driver MUST be usable with fake implementations of every trait, so the whole engine is testable without hardware. | `integration` | `crates/rincon-engine/src/lib.rs:125` |

## 7. Failure taxonomy

`FailureReason` is a closed enum; each variant maps to one user-facing sentence and one
remediation affordance. This is what stops "something went wrong" ever reaching the UI.

| Variant | Cause | Retryable | Remediation offered |
| ------- | ----- | --------- | ------------------- |
| `NoCaptureDevice` | No render endpoint | yes | Open Windows sound settings |
| `CaptureBusy` | Exclusive-mode app | yes | Name the blocking app if resolvable |
| `DeviceUnreachable` | Speaker offline / IP changed | yes | Rescan |
| `RejectedByDevice { upnp_code }` | UPnP fault | depends | Explain the specific code |
| `FirewallSuspected` | Bound and playing, peer never connected | yes | Firewall rule panel (SPEC-006) |
| `NetworkIsolated` | Speaker on another subnet/VLAN | no | Explain AP isolation |
| `Internal` | Bug | no | Copy diagnostics bundle |

## 8. Performance & resource budget

| Metric | Budget | Measured by |
| ------ | ------ | ----------- |
| `step` execution | < 1 µs | `benches/machine.rs` |
| Connect → audible (excluding the speaker's own buffer) | < 1.5 s | `rq_eng_010` |
| Live tasks while `Streaming` | ≤ 4 | `rq_eng_010` |

## 9. Minimum Viable Test (MVT)

```bash
just mvt-engine         # full session against testkit fakes, no hardware
```

Passes when the engine reaches `Streaming`, serves verifiable bytes, and returns to `Idle` with
`rq_eng_010`'s leak assertions clean.

## 10. Open questions

- [ ] Should `Degraded` auto-recovery be time-based or quality-based (n consecutive clean
      seconds)? Quality-based is better but needs a metric the UI can also show.
