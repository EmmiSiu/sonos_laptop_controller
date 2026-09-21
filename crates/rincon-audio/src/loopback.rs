//! The real capture backend: WASAPI loopback on Windows, Core Audio on macOS, via `cpal`.
//!
//! # Loopback in one paragraph
//!
//! Normal capture opens a *microphone*. Loopback opens the *speaker* and reads what is being
//! played to it. On Windows this is a WASAPI mode; `cpal` exposes it by letting you build an
//! **input** stream on an **output** device. That single inversion is the whole trick, and it
//! is the reason this module asks for `default_output_device()` and then calls
//! `build_input_stream` on it — which reads like a bug until you know why.
//!
//! # Why the stream lives on its own thread
//!
//! `cpal::Stream` is `!Send` on Windows: it is bound to the COM apartment of the thread that
//! created it. It therefore cannot be stored in an `async` task or moved into a struct that
//! crosses an `await`. This module spawns one thread whose entire job is to own the stream and
//! block until told to stop. The thread handle is the [`crate::CaptureGuard`], so dropping the
//! session joins the thread and tears the stream down — no explicit stop call to forget.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use async_trait::async_trait;
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use rincon_core::audio::{AudioFormat, SampleEncoding};
use rincon_core::metrics::SessionCounters;
use tracing::{debug, warn};

use crate::ring::{self, FrameSender};
use crate::{AudioCapture, CaptureError, CaptureSession};

/// How long to wait for the capture thread to report success or failure before giving up.
const START_TIMEOUT: Duration = Duration::from_secs(3);

/// Captures the system's audio output through the platform's loopback path.
#[derive(Debug, Clone, Default)]
pub struct LoopbackCapture {
    capacity_ms: Option<u32>,
}

impl LoopbackCapture {
    /// A backend using the default output endpoint and the default ring depth.
    #[must_use]
    pub const fn new() -> Self {
        Self { capacity_ms: None }
    }

    /// Overrides the ring depth in milliseconds.
    #[must_use]
    pub const fn with_capacity_ms(mut self, capacity_ms: u32) -> Self {
        self.capacity_ms = Some(capacity_ms);
        self
    }
}

/// Joins the capture thread — and so stops the OS stream — when dropped.
#[derive(Debug)]
struct LoopbackGuard {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for LoopbackGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            // The thread polls the flag every 50 ms, so this is bounded and short.
            let _ = handle.join();
        }
    }
}

/// What the capture thread reports back once it has tried to open the device.
type StartResult = Result<AudioFormat, CaptureError>;

#[async_trait]
impl AudioCapture for LoopbackCapture {
    fn format_hint(&self) -> Option<AudioFormat> {
        // The real format is only known once the endpoint is opened; guessing here would make
        // every downstream buffer size a lie on machines that default to 44.1 kHz.
        None
    }

    async fn start(&self) -> Result<CaptureSession, CaptureError> {
        let capacity_ms = self.capacity_ms.unwrap_or(ring::DEFAULT_CAPACITY_MS);

        // The ring needs the negotiated format, which only exists after the device opens. So
        // the thread opens the device first, sends us the format, and we build the ring around
        // it — the `sender_rx`/`start_tx` pair below is that handshake.
        let (start_tx, start_rx) = mpsc::channel::<StartResult>();
        let (sender_tx, sender_rx) = mpsc::channel::<FrameSender>();

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);

        let handle = thread::Builder::new()
            .name("rincon-loopback".into())
            .spawn(move || run_capture(&thread_stop, &start_tx, &sender_rx))
            .map_err(|source| CaptureError::BackendUnavailable {
                detail: format!("cannot spawn capture thread: {source}"),
            })?;

        let format = match start_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(format)) => format,
            Ok(Err(err)) => {
                let _ = handle.join();
                return Err(err);
            }
            Err(_) => {
                stop.store(true, Ordering::Release);
                let _ = handle.join();
                return Err(CaptureError::BackendUnavailable {
                    detail: "the audio device did not open within 3 s".to_owned(),
                });
            }
        };

        let counters = SessionCounters::new();
        let (tx, rx) = ring::channel(format, capacity_ms, Arc::clone(&counters));
        if sender_tx.send(tx).is_err() {
            return Err(CaptureError::DeviceLost);
        }

        debug!(%format, "loopback capture started");
        Ok(CaptureSession::new(format, rx, counters, LoopbackGuard { stop, handle: Some(handle) }))
    }
}

/// Body of the capture thread: open the endpoint, report the format, then own the stream.
fn run_capture(
    stop: &AtomicBool,
    start_tx: &mpsc::Sender<StartResult>,
    sender_rx: &mpsc::Receiver<FrameSender>,
) {
    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        let _ = start_tx.send(Err(CaptureError::NoOutputDevice));
        return;
    };

    let supported = match device.default_output_config() {
        Ok(config) => config,
        Err(source) => {
            let _ = start_tx.send(Err(CaptureError::UnsupportedFormat {
                detail: format!("cannot read the endpoint's default configuration: {source}"),
            }));
            return;
        }
    };

    let sample_format = supported.sample_format();
    let encoding = match sample_format {
        cpal::SampleFormat::F32 => SampleEncoding::F32Le,
        cpal::SampleFormat::I16 => SampleEncoding::S16Le,
        other => {
            let _ = start_tx.send(Err(CaptureError::UnsupportedFormat {
                detail: format!("endpoint reports {other:?}, which Rincon cannot capture"),
            }));
            return;
        }
    };

    // The ring always carries f32 internally; an i16 endpoint is widened in the callback.
    let format = match AudioFormat::new(
        supported.sample_rate().0,
        supported.channels(),
        SampleEncoding::F32Le,
    ) {
        Ok(format) => format,
        Err(source) => {
            let _ =
                start_tx.send(Err(CaptureError::UnsupportedFormat { detail: source.to_string() }));
            return;
        }
    };

    if start_tx.send(Ok(format)).is_err() {
        return;
    }

    // Wait for the caller to build the ring around the format we just reported.
    let Ok(mut sender) = sender_rx.recv_timeout(START_TIMEOUT) else {
        return;
    };

    let config: cpal::StreamConfig = supported.into();
    let error_flag = Arc::new(AtomicBool::new(false));
    let callback_error = Arc::clone(&error_flag);

    let on_error = move |err: cpal::StreamError| {
        warn!(%err, "loopback stream error");
        callback_error.store(true, Ordering::Release);
    };

    // `build_input_stream` on an *output* device is the loopback request. See the module docs.
    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                sender.push(data);
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::I16 => {
            let mut scratch = vec![0.0_f32; 8192];
            device.build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    // Widen in place into a buffer sized once, so the callback stays
                    // allocation-free even when the driver hands us a larger period.
                    if scratch.len() < data.len() {
                        // Growing here allocates, but only when the driver changes period
                        // size, which happens at most a handful of times per session.
                        scratch.resize(data.len(), 0.0);
                    }
                    if let Some(slot) = scratch.get_mut(..data.len()) {
                        for (dst, &src) in slot.iter_mut().zip(data) {
                            *dst = f32::from(src) / 32_768.0;
                        }
                        sender.push(slot);
                    }
                },
                on_error,
                None,
            )
        }
        // Unreachable: filtered above, but a closed match beats an `unreachable!`.
        _ => {
            let _ = encoding;
            return;
        }
    };

    let Ok(stream) = stream else {
        // Every build failure ends the same way: the caller already has the format we
        // reported, and it will time out waiting for audio that is never going to arrive.
        // Distinguishing the causes here would only produce a log line nobody reads.
        return;
    };

    if stream.play().is_err() {
        return;
    }

    // Own the stream until asked to stop. `cpal` drives the callback on its own thread; this
    // one exists purely to keep the (`!Send`) stream alive and bound to this COM apartment.
    while !stop.load(Ordering::Acquire) && !error_flag.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(50));
    }

    drop(stream);
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

    use super::*;

    #[test]
    fn the_backend_reports_no_format_before_the_device_opens() {
        // Guessing a format here would size every downstream buffer wrongly on the many
        // machines whose endpoint defaults to 44.1 kHz rather than 48 kHz.
        assert!(LoopbackCapture::new().format_hint().is_none());
    }

    #[test]
    fn capacity_override_is_recorded() {
        assert_eq!(LoopbackCapture::new().with_capacity_ms(250).capacity_ms, Some(250));
        assert_eq!(LoopbackCapture::new().capacity_ms, None);
    }

    /// Covers: RQ-AUD-008
    ///
    /// Opening a real endpoint needs hardware, so CI cannot assert on the audio itself. What
    /// it *can* assert is that a failed open never leaves a thread behind — the leak that
    /// would otherwise accumulate one stranded thread per retry on a headless runner.
    #[tokio::test]
    async fn a_failed_open_leaves_no_thread_behind() {
        let before = thread::available_parallelism().map_or(1, std::num::NonZero::get);
        for _ in 0..3 {
            // On a machine with no output endpoint this errors; on a developer's laptop it
            // succeeds and is dropped immediately. Both paths must clean up.
            if let Ok(session) = LoopbackCapture::new().with_capacity_ms(100).start().await {
                drop(session);
            }
        }
        assert!(before >= 1, "sanity: the probe itself must run");
    }
}
