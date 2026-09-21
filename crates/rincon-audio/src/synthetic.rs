//! A deterministic capture backend, so everything downstream is testable without a sound card.
//!
//! Every crate above this one — the stream server, the engine, the IPC layer — needs *some*
//! audio to exist before it can be exercised. Depending on real hardware for that would mean
//! those layers are only tested on a developer's laptop, never in CI, which is precisely where
//! a regression needs catching.
//!
//! `SyntheticCapture` produces a reproducible signal from a seed: the same seed yields the same
//! samples on every platform and every run, so a test can assert on exact byte output.

use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rincon_core::audio::AudioFormat;
use rincon_core::metrics::SessionCounters;

use crate::ring::{self, FrameReceiver};
use crate::{AudioCapture, CaptureError, CaptureSession};

/// Base frequency of the generated tone, in hertz. A 440 Hz A is easy to recognise by ear when
/// a developer routes the synthetic backend to a real speaker.
const BASE_HZ: f32 = 440.0;

/// A reproducible signal generator.
///
/// Pure: the sample at a given frame index depends only on the seed and the index, never on
/// wall time or previous calls. That is what makes golden tests over this backend possible.
#[derive(Debug, Clone, Copy)]
pub struct SyntheticSource {
    format: AudioFormat,
    seed: u64,
}

impl SyntheticSource {
    /// A generator for `format`, reproducible from `seed`.
    #[must_use]
    pub const fn new(format: AudioFormat, seed: u64) -> Self {
        Self { format, seed }
    }

    /// The format this source produces.
    #[must_use]
    pub const fn format(&self) -> AudioFormat {
        self.format
    }

    /// The sample for one channel of one frame.
    ///
    /// Each channel gets its own detuned partial so that a channel swap anywhere downstream
    /// shows up as a different waveform rather than an identical one.
    ///
    /// Covers: RQ-AUD-007
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "frame indices are session-scale; float precision is ample for a test tone"
    )]
    pub fn sample(&self, frame: u64, channel: u16) -> f32 {
        let rate =
            f32::from(u16::try_from(self.format.sample_rate.hz() / 100).unwrap_or(480)) * 100.0;
        let detune = f32::from(channel).mul_add(0.05, 1.0);
        let seed_offset = (self.seed % 1_000) as f32 / 1_000.0;
        let phase = ((frame as f32 / rate) * BASE_HZ).mul_add(detune, seed_offset);
        (phase * TAU).sin() * 0.25
    }

    /// Fills `out` with `out.len() / channels` interleaved frames starting at `start_frame`.
    pub fn fill(&self, start_frame: u64, out: &mut [f32]) {
        let channels = self.format.channels.get();
        if channels == 0 {
            return;
        }
        for (index, slot) in out.iter_mut().enumerate() {
            let frame = start_frame + (index / channels as usize) as u64;
            let channel = u16::try_from(index % channels as usize).unwrap_or(0);
            *slot = self.sample(frame, channel);
        }
    }
}

/// A capture backend that generates audio instead of reading a device.
#[derive(Debug, Clone)]
pub struct SyntheticCapture {
    source: SyntheticSource,
    capacity_ms: u32,
}

impl SyntheticCapture {
    /// A backend producing `format`, reproducible from `seed`.
    #[must_use]
    pub const fn new(format: AudioFormat, seed: u64) -> Self {
        Self { source: SyntheticSource::new(format, seed), capacity_ms: ring::DEFAULT_CAPACITY_MS }
    }

    /// Overrides the ring depth, for tests that want to force an overrun.
    #[must_use]
    pub const fn with_capacity_ms(mut self, capacity_ms: u32) -> Self {
        self.capacity_ms = capacity_ms;
        self
    }

    /// The underlying pure generator, for tests that want samples without a session.
    #[must_use]
    pub const fn source(&self) -> SyntheticSource {
        self.source
    }
}

#[async_trait]
impl AudioCapture for SyntheticCapture {
    fn format_hint(&self) -> Option<AudioFormat> {
        Some(self.source.format)
    }

    async fn start(&self) -> Result<CaptureSession, CaptureError> {
        let format = self.source.format;
        let counters = SessionCounters::new();
        let (mut tx, rx) = ring::channel(format, self.capacity_ms, Arc::clone(&counters));

        let stop = Arc::new(AtomicBool::new(false));
        let source = self.source;
        let worker_stop = Arc::clone(&stop);

        // 10 ms of audio per tick: small enough to look like a real callback, large enough
        // that the sleep granularity on Windows does not dominate.
        let frames_per_tick = format.frames_in(10);
        let channels = format.channels.get() as usize;
        let tick = Duration::from_millis(10);

        let handle = thread::Builder::new()
            .name("rincon-synthetic-capture".into())
            .spawn(move || {
                let mut buffer = vec![0.0_f32; frames_per_tick * channels];
                let mut next_frame: u64 = 0;
                let mut deadline = Instant::now();

                while !worker_stop.load(Ordering::Acquire) {
                    source.fill(next_frame, &mut buffer);
                    tx.push(&buffer);
                    next_frame += frames_per_tick as u64;

                    deadline += tick;
                    if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                        thread::sleep(remaining);
                    } else {
                        // Fell behind (a debugger, a loaded CI runner): resynchronise rather
                        // than spinning to catch up, which would burn a core.
                        deadline = Instant::now();
                    }
                }
                tx.mark_device_lost();
            })
            .map_err(|source| CaptureError::BackendUnavailable {
                detail: format!("cannot spawn synthetic capture thread: {source}"),
            })?;

        Ok(CaptureSession::new(format, rx, counters, SyntheticGuard { stop, handle: Some(handle) }))
    }
}

/// Stops the generator thread when the session is dropped.
#[derive(Debug)]
struct SyntheticGuard {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for SyntheticGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            // The worker checks the flag every 10 ms, so this join is short and bounded.
            let _ = handle.join();
        }
    }
}

/// A receiver that yields a fixed script of buffers, for tests that need exact control.
///
/// Where [`SyntheticCapture`] models "a device that keeps producing", this models "exactly
/// these samples, then silence" — which is what a golden test over the stream server needs.
#[must_use]
pub fn scripted(
    format: AudioFormat,
    buffers: &[Vec<f32>],
) -> (FrameReceiver, Arc<SessionCounters>) {
    let counters = SessionCounters::new();
    let total_frames: usize = buffers.iter().map(Vec::len).sum::<usize>().max(1);
    let capacity_ms =
        u32::try_from(total_frames * 1000 / format.sample_rate.hz() as usize).unwrap_or(1000) + 100;

    let (mut tx, rx) = ring::channel(format, capacity_ms.max(100), Arc::clone(&counters));
    for buffer in buffers {
        tx.push(buffer);
    }
    tx.mark_device_lost();
    (rx, counters)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::float_cmp,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]

    use rincon_core::audio::SampleEncoding;

    use super::*;

    fn format() -> AudioFormat {
        AudioFormat::new(48_000, 2, SampleEncoding::F32Le).unwrap()
    }

    /// Covers: RQ-AUD-007
    #[test]
    fn rq_aud_007_is_deterministic_for_a_seed() {
        let a = SyntheticSource::new(format(), 42);
        let b = SyntheticSource::new(format(), 42);

        let mut first = vec![0.0_f32; 4096];
        let mut second = vec![0.0_f32; 4096];
        a.fill(0, &mut first);
        b.fill(0, &mut second);
        assert_eq!(first, second, "the same seed must produce identical audio");

        // Resuming at an offset matches generating straight through: the source is a pure
        // function of the frame index, not a stateful oscillator that drifts.
        let mut tail = vec![0.0_f32; 2048];
        a.fill(1024, &mut tail);
        assert_eq!(tail, first[2048..], "generation must be position-independent");

        // A different seed produces different audio, or the seed does nothing.
        let mut other = vec![0.0_f32; 4096];
        SyntheticSource::new(format(), 7).fill(0, &mut other);
        assert_ne!(first, other, "different seeds must produce different audio");
    }

    #[test]
    fn generated_audio_stays_inside_full_scale() {
        let source = SyntheticSource::new(format(), 1);
        let mut buffer = vec![0.0_f32; 48_000 * 2];
        source.fill(0, &mut buffer);
        for (index, sample) in buffer.iter().enumerate() {
            assert!(sample.is_finite(), "sample {index} is not finite");
            assert!(sample.abs() <= 1.0, "sample {index} = {sample} exceeds full scale");
        }
    }

    #[test]
    fn channels_are_distinguishable() {
        // If left and right were identical, a channel swap anywhere downstream would be
        // invisible to every test in the workspace.
        let source = SyntheticSource::new(format(), 3);
        let mut buffer = vec![0.0_f32; 1024];
        source.fill(0, &mut buffer);
        let left: Vec<f32> = buffer.iter().step_by(2).copied().collect();
        let right: Vec<f32> = buffer.iter().skip(1).step_by(2).copied().collect();
        assert_ne!(left, right, "the two channels must carry different waveforms");
    }

    #[test]
    fn a_scripted_source_delivers_exactly_what_it_was_given() {
        let script = vec![vec![0.1, 0.2, 0.3, 0.4], vec![0.5, 0.6]];
        let (mut rx, _) = scripted(format(), &script);

        let mut got = Vec::new();
        let mut out = vec![0.0_f32; 8];
        while let Ok(n) = rx.read(&mut out) {
            if n == 0 {
                break;
            }
            got.extend_from_slice(&out[..n]);
        }
        assert_eq!(got, vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);
    }
}
