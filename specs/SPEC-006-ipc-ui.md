---
id: SPEC-006
title: IPC boundary & interface
status: accepted
owner: EmmiSiu
crate: apps/desktop
depends_on: [SPEC-005, SPEC-007]
supersedes: null
last_reviewed: 2026-09-20
---

# SPEC-006 — IPC boundary & interface

## 1. Problem

The UI is a WebView. A WebView is a browser, and a browser executes whatever ends up in the DOM.
The strings it renders — room names, model names, error text — arrive from unauthenticated
devices on the LAN (SPEC-003). The IPC bridge between that WebView and a Rust process holding a
live capture of the user's system audio is therefore a genuine trust boundary, not plumbing.

Separately: the interface has one honest job beyond looking good. It must set the expectation
about Sonos's 1–2 s buffer *before* the user discovers it with a video, or the product feels
broken through no fault of the code.

## 2. Goals

- A narrow, typed, enumerable command surface. Adding a command should feel deliberate.
- Types generated from Rust, so a backend change breaks the TypeScript build rather than
  producing a runtime `undefined` in front of the user.
- Dark-first interface with a real-time health surface, readable at a glance from across a room.
- Honest latency communication, placed where it is read, not buried in an about box.

## 3. Non-goals

- Remote control over the network. The UI talks to its own process only.
- Theming, plugins, or a settings page beyond what a session needs in v0.1.
- An equaliser or any DSP. Rincon transports audio; it does not colour it.

## 4. Command surface

```rust
#[tauri::command] async fn scan_devices(state: State<'_, App>) -> IpcResult<Vec<DeviceDto>>;
#[tauri::command] async fn connect(state: State<'_, App>, id: String) -> IpcResult<()>;
#[tauri::command] async fn disconnect(state: State<'_, App>) -> IpcResult<()>;
#[tauri::command] async fn set_volume(state: State<'_, App>, level: u8) -> IpcResult<()>;
#[tauri::command] async fn session_state(state: State<'_, App>) -> IpcResult<SessionDto>;
#[tauri::command] async fn diagnostics(state: State<'_, App>) -> IpcResult<DiagnosticsDto>;
#[tauri::command] async fn firewall_status(state: State<'_, App>) -> IpcResult<FirewallDto>;
```

Push direction: a single `rincon://session` event carrying `SessionDto` on every transition.
The frontend never polls.

## 5. Requirements

| ID | Requirement | Verification | Covered by |
| -- | ----------- | ------------ | ---------- |
| `RQ-UI-001` | Commands MUST accept a device **identifier**, never a raw address; the Rust side resolves it against known devices. | `unit` | `crates/rincon-ipc/src/lib.rs:243`<br>`crates/rincon-ipc/src/lib.rs:430` |
| `RQ-UI-002` | An unknown device id MUST return a typed `IpcError::UnknownDevice`, never panic and never 500. | `unit` | `crates/rincon-ipc/src/lib.rs:82`<br>`crates/rincon-ipc/src/lib.rs:243`<br>`crates/rincon-ipc/src/lib.rs:450` |
| `RQ-UI-003` | Every IPC error MUST serialise as `{ code, message, retryable }` with a stable `code`. | `unit` | `crates/rincon-core/src/stream_url.rs:82`<br>`crates/rincon-ipc/src/lib.rs:40`<br>`crates/rincon-ipc/src/lib.rs:467` |
| `RQ-UI-004` | The Tauri CSP MUST forbid `unsafe-inline`, `unsafe-eval`, and any remote origin. | `unit` | `apps/desktop/src/lib/guards.spec.ts:56`<br>`scripts/spec-guard.mjs:124` |
| `RQ-UI-005` | The frontend MUST NOT use `v-html`, `innerHTML`, or `eval` anywhere. | `unit` | `apps/desktop/src/lib/guards.spec.ts:29` |
| `RQ-UI-006` | The latency characteristic MUST be visible in the connected state, not hidden behind a menu. | `manual` | `crates/rincon-ipc/src/lib.rs:148`<br>`crates/rincon-ipc/src/lib.rs:296`<br>`crates/rincon-ipc/src/lib.rs:519`<br>`docs/manual-test-log.md:18` |
| `RQ-UI-007` | On `FirewallSuspected`, the UI MUST show the exact remediation command and MUST NOT silently execute a privileged action. | `unit` | `crates/rincon-ipc/src/lib.rs:161`<br>`crates/rincon-ipc/src/lib.rs:314`<br>`crates/rincon-ipc/src/lib.rs:489` |
| `RQ-UI-008` | TypeScript bindings MUST be generated from the Rust DTOs; a mismatch MUST fail the frontend build. | `integration` | `crates/rincon-ipc/src/bindings.rs:161`<br>`.github/workflows/ci.yml:108` |
| `RQ-UI-009` | The window MUST remain interactive during a scan; no command may block the UI thread. | `manual` | `docs/manual-test-log.md:33` |
| `RQ-UI-010` | `withGlobalTauri` MUST be `false`, and the app MUST enable only the capabilities it uses. | `unit` | `scripts/spec-guard.mjs:124` |
| `RQ-UI-011` | Displayed device text MUST be rendered as text, and MUST be truncated at 64 characters. | `unit` | `crates/rincon-core/src/device.rs:102`<br>`crates/rincon-core/src/device.rs:222`<br>`apps/desktop/src/lib/sanitize.spec.ts:6`<br>`apps/desktop/src/lib/sanitize.spec.ts:20` |

## 6. Interface states

| State | What the user sees | Primary action |
| ----- | ------------------ | -------------- |
| `Idle` | Radar illustration, "Find my speakers" | Scan |
| `Scanning` | Sweeping radar, elapsed counter | Cancel |
| `NoDevices` | Checklist: same Wi-Fi, AP isolation, manual IP | Retry / Enter IP |
| `DevicesFound` | Room cards: name, model, signal | Connect |
| `Preparing` | Stepper showing the five prepare steps, current one live | Cancel |
| `Streaming` | Live level meter, elapsed, volume, latency note | Stop |
| `Degraded` | Same, amber, with the specific reason | Stop / wait |
| `Failed` | One sentence cause + one remediation button | Retry / Copy diagnostics |

The stepper in `Preparing` is deliberate: when connection fails, the user has already seen which
of the five steps was reached, which is most of the diagnosis.

## 7. Performance & resource budget

| Metric | Budget | Measured by |
| ------ | ------ | ----------- |
| Installer size | < 15 MB | release job assertion |
| Idle RSS | < 60 MB | manual |
| IPC round trip, `session_state` | < 5 ms | `rq_ui_003` timing |
| Level-meter frame budget | 60 fps, no layout thrash | manual |

## 8. Security considerations

| Risk | Mitigation | Requirement |
| ---- | ---------- | ----------- |
| XSS via a crafted room name | Text rendering only, no `v-html`, CSP without `unsafe-inline` | `RQ-UI-004/005/011` |
| Renderer compromise reaching the OS | Minimal Tauri capabilities, `withGlobalTauri: false`, no shell plugin | `RQ-UI-010` |
| Privilege escalation via a "fix my firewall" button | The panel is advisory: it shows the command, the user runs it | `RQ-UI-007` |
| Address injection through IPC | Commands take opaque ids the backend resolves | `RQ-UI-001` |
| Supply-chain via frontend deps | Lockfile committed, `npm audit` gate, no runtime CDN | SPEC-007 |

## 9. Minimum Viable Test (MVT)

```bash
just mvt-ui             # dev build; scan runs against testkit fakes
```

Passes when a click on Scan produces cards from the fake responder, Connect walks the five
prepare steps visibly, and the latency note is on screen in the connected state.

## 10. Open questions

- [ ] Should the level meter read from real audio (a second ring consumer, costing a copy) or
      from cheap RMS computed in the capture thread? Measure the copy before deciding.
