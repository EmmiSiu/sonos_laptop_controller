---
id: SPEC-008
title: Observability & diagnostics
status: accepted
owner: EmmiSiu
crate: workspace (cross-cutting)
depends_on: [SPEC-005, SPEC-007]
supersedes: null
last_reviewed: 2026-09-21
---

# SPEC-008 — Observability & diagnostics

## 1. Problem

Audio failures are ephemeral and subjective. "It crackled for a second" is unactionable unless
the program already counted the event. And because Rincon sends nothing home (SPEC-000 NG5), the
only diagnostic that exists is the one the user can see and choose to share.

So observability here is not a dashboard. It is: count the right things locally, show them
honestly, and make a redacted bundle the user can attach to an issue in one click.

## 2. Goals

- Every dropout is counted and attributable to a layer (capture, ring, socket, device, or
  "nothing was listening yet").
- Logs useful enough to debug a user's report, safe enough to paste in a public issue.
- A diagnostics bundle a non-technical user can produce and a maintainer can act on.

## 3. Non-goals

- Remote telemetry, crash reporting, or metrics upload of any kind. Ever.
- A metrics backend (Prometheus, OTLP). There is no server; the process is the whole system.

## 4. Instrumentation model

Three layers, each with a different audience:

| Layer | Audience | Mechanism | Retention |
| ----- | -------- | --------- | --------- |
| Counters | UI health indicator | Lock-free atomics, sampled by the host and pushed at 4 Hz | Session lifetime |
| Spans & events | Maintainer debugging a report | `tracing`, rolling file | 7 days / 10 MB cap |
| Bundle | Issue attachment | Redacted snapshot on demand | User-controlled |

## 5. Requirements

| ID | Requirement | Verification | Covered by |
| -- | ----------- | ------------ | ---------- |
| `RQ-OBS-001` | Every dropped frame MUST be counted and attributed to exactly one layer. | `unit` | `crates/rincon-audio/src/ring.rs:550`<br>`crates/rincon-core/src/metrics.rs:107`<br>`crates/rincon-core/src/metrics.rs:320` |
| `RQ-OBS-002` | Counters MUST be readable without locking and without perturbing the audio path. | `unit` | `crates/rincon-core/src/metrics.rs:351` |
| `RQ-OBS-003` | Logs MUST default to `info`, be overridable by `RINCON_LOG`, and MUST NOT include audio samples. | `unit` | `crates/rincon-core/src/telemetry.rs:53`<br>`crates/rincon-core/src/telemetry.rs:204`<br>`crates/rincon-core/src/telemetry.rs:217`<br>`crates/rincon-core/src/telemetry.rs:262` |
| `RQ-OBS-004` | Log output MUST redact the stream token, replacing it with `<redacted>`. | `unit` | `crates/rincon-core/src/stream_url.rs:133`<br>`crates/rincon-stream/src/token.rs:202` |
| `RQ-OBS-005` | The log file MUST be size-capped at 10 MB with rotation; it MUST NOT be able to fill the disk. | `unit` | `crates/rincon-core/src/telemetry.rs:217`<br>`crates/rincon-core/src/telemetry.rs:282` |
| `RQ-OBS-006` | A diagnostics bundle MUST include app version, OS build, interface summary, counters, and the last 500 log lines. | `unit` | `crates/rincon-core/src/diagnostics.rs:145`<br>`crates/rincon-core/src/diagnostics.rs:274` |
| `RQ-OBS-007` | The bundle MUST redact the stream token, the machine hostname, and all but the last octet of each IP. | `property` | `crates/rincon-core/src/diagnostics.rs:293` |
| `RQ-OBS-008` | Producing a bundle MUST NOT make any network request. | `unit` | `crates/rincon-core/src/diagnostics.rs:145`<br>`crates/rincon-core/src/diagnostics.rs:308` |
| `RQ-OBS-009` | The UI health indicator MUST reflect a degradation within 500 ms of it being counted. | `unit` | `crates/rincon-core/src/metrics.rs:275`<br>`crates/rincon-core/src/metrics.rs:392`<br>`crates/rincon-engine/src/driver.rs:446`<br>`crates/rincon-engine/src/lib.rs:171`<br>`crates/rincon-engine/src/machine.rs:657`<br>`crates/rincon-engine/src/machine.rs:830` |

## 6. Health model

The indicator the user sees is a pure function of a counter **delta** over a rolling two-second
window, so it is unit-testable and cannot drift from what is actually measured. The window
matters twice over: it is what makes the indicator recover on its own, and it is why loss that
predates every sample in the window is invisible to it — correctly, because that loss is not
happening now.

| Health | Condition |
| ------ | --------- |
| `Good` | 0 quality drops, 0 underruns, peer connected |
| `Fair` | ≤ 3 underruns, 0 quality drops |
| `Poor` | any quality drops, or > 3 underruns |
| `Lost` | peer disconnected |

### Not every dropped frame is a quality problem

> **Measured on real hardware, 2026-09-21.** A session the operator listened to and described as
> perfect reported **54% of captured frames dropped**.

Nothing was wrong. Capture has to start before the server can bind, and the speaker will not
connect until it has been handed a URL — so several seconds of audio are captured, correctly
discarded by the ring's drop-oldest policy, and counted, before anyone is listening. Attributing
that to the ring made a healthy session indistinguishable from a broken one.

So the layers split in two:

- **Quality layers** — `capture`, `ring`, `socket`, `device`. Audio a listener lost.
- **`no-consumer`** — discarded before anything was fetching the stream. Counted, reported in
  the bundle, and excluded from health.

`CounterSnapshot::dropped_total` is the first figure a bug report argues over, so it includes
everything. `CounterSnapshot::quality_drops` is what the health model and the interface use.
A unit test asserts that every layer whose `affects_quality()` is true is part of that sum, so
adding a sixth layer cannot silently escape the health model.

The same rule applies to the watch itself: while nothing is fetching the stream, it takes no
measurement at all and clears its window, because SPEC-002 requires a session to *survive* the
speaker disconnecting and reconnecting rather than blame it for the silence.

## 7. Diagnostics bundle

```
rincon-diagnostics-2026-09-20T18-42-11Z.txt
├── version, build hash, profile
├── OS build, CPU, RAM
├── interfaces:    wlan0 192.168.1.xxx/24 (up)
├── session:       Streaming, 00:14:22, format 48000 Hz / 2ch / f32
├── counters:      captured 41,472,000  dropped 0  underruns 2  reconnects 0
├──   by layer:    capture 0 ring 0 socket 0 device 0 (before-connect 313,920)
├── device:        Sonos One, firmware 15.x, addr 192.168.1.xxx
└── log tail:      last 500 lines, token redacted
```

Redaction is applied when the bundle is built, never as a display-time filter — so a bundle
written to disk is safe by construction rather than by remembering.

## 8. Minimum Viable Test (MVT)

```bash
just mvt-diagnostics    # builds a bundle from a synthetic session
```

Passes when the bundle contains every section in §7 and `rq_obs_007` finds no unredacted token,
hostname, or full IP anywhere in the output.

## 9. Open questions

- [ ] Is 4 Hz polling enough for a level meter that feels live, or should the meter take a
      separate lower-cost path from the counters?
