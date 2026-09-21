---
id: SPEC-008
title: Observability & diagnostics
status: accepted
owner: EmmiSiu
crate: workspace (cross-cutting)
depends_on: [SPEC-005, SPEC-007]
supersedes: null
last_reviewed: 2026-09-20
---

# SPEC-008 — Observability & diagnostics

## 1. Problem

Audio failures are ephemeral and subjective. "It crackled for a second" is unactionable unless
the program already counted the event. And because Rincon sends nothing home (SPEC-000 NG5), the
only diagnostic that exists is the one the user can see and choose to share.

So observability here is not a dashboard. It is: count the right things locally, show them
honestly, and make a redacted bundle the user can attach to an issue in one click.

## 2. Goals

- Every dropout is counted and attributable to a layer (capture, ring, socket, device).
- Logs useful enough to debug a user's report, safe enough to paste in a public issue.
- A diagnostics bundle a non-technical user can produce and a maintainer can act on.

## 3. Non-goals

- Remote telemetry, crash reporting, or metrics upload of any kind. Ever.
- A metrics backend (Prometheus, OTLP). There is no server; the process is the whole system.

## 4. Instrumentation model

Three layers, each with a different audience:

| Layer | Audience | Mechanism | Retention |
| ----- | -------- | --------- | --------- |
| Counters | UI health indicator | Lock-free atomics, polled at 4 Hz | Session lifetime |
| Spans & events | Maintainer debugging a report | `tracing`, rolling file | 7 days / 10 MB cap |
| Bundle | Issue attachment | Redacted snapshot on demand | User-controlled |

## 5. Requirements

| ID | Requirement | Verification | Covered by |
| -- | ----------- | ------------ | ---------- |
| `RQ-OBS-001` | Every dropped frame MUST be counted and attributed to exactly one layer. | `unit` | `rincon_core::metrics::tests::rq_obs_001_drops_are_attributed` |
| `RQ-OBS-002` | Counters MUST be readable without locking and without perturbing the audio path. | `unit` | `rincon_core::metrics::tests::rq_obs_002_counters_are_lock_free` |
| `RQ-OBS-003` | Logs MUST default to `info`, be overridable by `RINCON_LOG`, and MUST NOT include audio samples. | `unit` | `rincon_core::telemetry::tests::rq_obs_003_log_level_and_no_samples` |
| `RQ-OBS-004` | Log output MUST redact the stream token, replacing it with `<redacted>`. | `unit` | `rincon_stream::token::tests::rq_sec_005_token_is_redacted_in_debug` |
| `RQ-OBS-005` | The log file MUST be size-capped at 10 MB with rotation; it MUST NOT be able to fill the disk. | `unit` | `rincon_core::telemetry::tests::rq_obs_005_log_is_size_capped` |
| `RQ-OBS-006` | A diagnostics bundle MUST include app version, OS build, interface summary, counters, and the last 500 log lines. | `unit` | `rincon_core::diagnostics::tests::rq_obs_006_bundle_contents` |
| `RQ-OBS-007` | The bundle MUST redact the stream token, the machine hostname, and all but the last octet of each IP. | `property` | `rincon_core::diagnostics::tests::rq_obs_007_bundle_is_redacted` |
| `RQ-OBS-008` | Producing a bundle MUST NOT make any network request. | `unit` | `rincon_core::diagnostics::tests::rq_obs_008_bundle_is_offline` |
| `RQ-OBS-009` | The UI health indicator MUST reflect a degradation within 500 ms of it being counted. | `unit` | `rincon_engine::tests::rq_obs_009_health_propagates_promptly` |

## 6. Health model

The indicator the user sees is a pure function of counters over a 5-second window, so it is
unit-testable and cannot drift from what is actually measured.

| Health | Condition |
| ------ | --------- |
| `Good` | 0 drops, 0 underruns, peer connected |
| `Fair` | ≤ 3 underruns, 0 drops |
| `Poor` | any dropped frames, or > 3 underruns |
| `Lost` | peer disconnected for > 2 s |

## 7. Diagnostics bundle

```
rincon-diagnostics-2026-09-20T18-42-11Z.txt
├── version, build hash, profile
├── OS build, CPU, RAM
├── interfaces:    wlan0 192.168.1.xxx/24 (up)
├── session:       Streaming, 00:14:22, format 48000 Hz / 2ch / f32
├── counters:      captured 41,472,000  dropped 0  underruns 2  reconnects 0
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
