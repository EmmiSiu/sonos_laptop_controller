---
id: SPEC-001
title: Audio capture (WASAPI loopback)
status: accepted
owner: EmmiSiu
crate: rincon-audio
depends_on: [SPEC-000]
supersedes: null
last_reviewed: 2026-09-20
---

# SPEC-001 — Audio capture

## 1. Problem

To stream "whatever the laptop is playing", we must read the final mixed output of the system
audio engine. On Windows that is WASAPI *loopback* capture: opening the render endpoint in
capture mode. The capture callback runs on a real-time thread owned by the OS — it must never
block, allocate, take a lock, or panic, or the user hears a click.

The rest of Rincon is `async` on Tokio. Bridging a hard real-time callback to an async consumer
without a lock is the central engineering problem of this module.

## 2. Goals

- Capture the default render endpoint in loopback at its native format.
- Hand samples to the async world through a wait-free single-producer/single-consumer ring.
- Account for every lost sample. Silent data loss is a defect; counted data loss is a metric.
- Provide a deterministic synthetic backend so every downstream crate is testable in CI on
  machines with no sound card.

## 3. Non-goals

- Resampling. SPEC-002 derives the wire format from the capture format instead.
- Per-process capture (SPEC-000 NG4).
- Device hot-plug migration in v0.1. If the default endpoint changes, the session reports
  `DeviceLost` and the engine restarts capture.

## 4. Interface contract

```rust
/// A backend able to observe the system's audio output.
#[async_trait]
pub trait AudioCapture: Send + Sync + fmt::Debug {
    /// Begins capture and returns the consumer half of the ring.
    async fn start(&self) -> Result<CaptureSession, CaptureError>;
}

pub struct CaptureSession {
    pub format: AudioFormat,
    pub frames:  FrameReceiver,
    pub stats:   Arc<CaptureStats>,
}

/// Wait-free consumer. `read_frames` never blocks and never allocates.
pub struct FrameReceiver { /* ... */ }

/// Monotonic counters. Lock-free reads; safe to poll from the UI thread.
pub struct CaptureStats {
    pub frames_captured: AtomicU64,
    pub frames_dropped:  AtomicU64, // producer overran a slow consumer
    pub underruns:       AtomicU64, // consumer found the ring empty
    pub callbacks:       AtomicU64,
}
```

## 5. Requirements

| ID | Requirement | Verification | Covered by |
| -- | ----------- | ------------ | ---------- |
| `RQ-AUD-001` | The producer path MUST NOT allocate, lock, or perform I/O once capture has started. | `unit` | `rincon_audio::ring::tests::rq_aud_001_producer_path_does_not_allocate` |
| `RQ-AUD-002` | The ring MUST preserve frame alignment: a partial frame MUST never be visible to the consumer. | `property` | `rincon_audio::ring::tests::rq_aud_002_never_splits_a_frame` |
| `RQ-AUD-003` | On overrun the ring MUST discard the **oldest** whole frames and increment `frames_dropped` by exactly the number discarded. | `property` | `rincon_audio::ring::tests::rq_aud_003_overrun_drops_oldest_and_counts` |
| `RQ-AUD-004` | `f32` samples outside `[-1.0, 1.0]` MUST be clamped, never wrapped, when converted to `i16`. | `property` | `rincon_audio::convert::tests::rq_aud_004_clamps_never_wraps` |
| `RQ-AUD-005` | Conversion endpoints MUST be exact: `+1.0 → 32767`, `-1.0 → -32768`, `0.0 → 0`. | `unit` | `rincon_audio::convert::tests::rq_aud_005_endpoints_are_exact` |
| `RQ-AUD-006` | A NaN or infinite input sample MUST convert to silence (`0`), never to an arbitrary value. | `property` | `rincon_audio::convert::tests::rq_aud_006_non_finite_becomes_silence` |
| `RQ-AUD-007` | The crate MUST expose a `SyntheticCapture` backend on every platform, producing a deterministic signal from a seed. | `unit` | `rincon_audio::synthetic::tests::rq_aud_007_is_deterministic_for_a_seed` |
| `RQ-AUD-008` | Dropping a `CaptureSession` MUST stop the underlying stream and MUST NOT leak the OS handle. | `integration` | `rincon_audio::tests::rq_aud_008_drop_stops_capture` |
| `RQ-AUD-009` | Ring capacity MUST be configurable and MUST default to at least 500 ms of audio at the negotiated format. | `unit` | `rincon_audio::ring::tests::rq_aud_009_default_capacity_covers_500ms` |
| `RQ-AUD-010` | When the device disappears, the consumer MUST observe a terminal `DeviceLost` state rather than blocking forever or silently returning silence. | `unit` | `rincon_audio::ring::tests::rq_aud_010_device_loss_is_observable` |
| `RQ-AUD-011` | Interleaved multi-channel frames MUST round-trip through the ring in channel order. | `property` | `rincon_audio::ring::tests::rq_aud_011_channel_order_is_preserved` |

## 6. Failure modes

| Condition | Detection | Response | User-visible result |
| --------- | --------- | -------- | ------------------- |
| No default render endpoint | `start()` → `NoOutputDevice` | Engine stays `Idle` | "No playback device found. Connect speakers or headphones." |
| Endpoint changed (BT headset connected) | Stream error / close | `DeviceLost`; engine restarts capture, stream survives | Brief gap, toast "Audio device changed, reconnecting" |
| Another app holds the endpoint in exclusive mode | `start()` → `DeviceBusy` | Retry with backoff, max 5 attempts | "Another app has exclusive control of the audio device." |
| Consumer stalls beyond ring capacity | `frames_dropped` climbs | Drop oldest, keep streaming | Health indicator turns amber |

## 7. Performance & resource budget

| Metric | Budget | Measured by |
| ------ | ------ | ----------- |
| Producer path, one 480-frame callback | < 20 µs | `benches/ring.rs` |
| Added latency, capture → ring read | < 15 ms | `benches/ring.rs` |
| Steady-state allocations after `start()` | 0 | `rq_aud_001`, counting allocator |
| Ring memory, 48 kHz stereo f32, 1 s | ≈ 384 KiB | `rq_aud_009` |

## 8. Security considerations

Loopback capture reads **all** system audio, including conference calls. Therefore:

- Capture MUST NOT start until the user has chosen a target device. No capture at app launch.
- Captured audio MUST NEVER be written to disk except through an explicit, user-initiated debug
  dump to a path the user picked.
- The resulting stream is offered only to the allowlisted peer (SPEC-002 `RQ-STRM-004`).

See SPEC-007 §Privacy.

## 9. Minimum Viable Test (MVT)

```bash
just mvt-audio          # captures 5 s of system audio -> artifacts/mvt-audio.wav
```

Passes when the file is a valid 5 s WAV at the endpoint's native rate, RMS is non-zero while
music plays, and `frames_dropped == 0`.

## 10. Open questions

- [ ] Does `cpal` surface `AUDCLNT_E_DEVICE_INVALIDATED` distinctly enough to map onto
      `DeviceLost`, or do we need a raw WASAPI backend behind the same trait?
- [ ] Silence suppression: Sonos may drop a stalled stream. Inject digital silence when the
      system is idle, or accept the reconnect? Prototype both against real firmware.
