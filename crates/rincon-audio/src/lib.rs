//! System audio capture and the lock-free bridge from the real-time callback to async consumers.
//!
//! # Why this crate exists separately
//!
//! Everything platform-specific in Rincon lives here, behind one trait. Discovery, control,
//! streaming, and orchestration know only [`AudioCapture`] and [`ring::FrameReceiver`]; they
//! contain no `#[cfg(windows)]` at all. Porting to macOS or webOS therefore means adding a
//! backend module in this crate and nothing else — the promise SPEC-000 G5 makes.
//!
//! # Backends
//!
//! | Backend | Availability | Purpose |
//! | ------- | ------------ | ------- |
//! | [`synthetic::SyntheticCapture`] | every platform | deterministic audio for tests and CI |
//! | [`loopback::LoopbackCapture`] | Windows, macOS, with the `loopback` feature | the real device |
//!
//! # The real-time contract
//!
//! The producer path — everything reachable from a capture callback — must not allocate, lock,
//! or perform I/O. That is enforced by a test, not by convention: see
//! `ring::tests::rq_aud_001_producer_path_does_not_allocate`, which installs a counting
//! allocator and asserts zero allocations across two hundred callbacks, including during
//! overrun.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use rincon_core::audio::AudioFormat;
use rincon_core::metrics::{CounterSnapshot, SessionCounters};

pub mod convert;
pub mod ring;
pub mod synthetic;

#[cfg(all(feature = "loopback", any(windows, target_os = "macos")))]
pub mod loopback;

pub use ring::{FrameReceiver, FrameSender, RingError};
pub use synthetic::{SyntheticCapture, SyntheticSource};

/// Why capture could not start, or could not continue.
///
/// Each variant maps to exactly one sentence in the interface and one remediation. A generic
/// "capture failed" would collapse four different user problems into one unhelpful dialog.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    /// The machine has no active playback endpoint.
    #[error("no audio playback device is available")]
    NoOutputDevice,

    /// Another application holds the endpoint in exclusive mode.
    #[error("the audio device is held exclusively by another application")]
    DeviceBusy,

    /// The endpoint reports a format Rincon cannot stream.
    #[error("unsupported device format: {detail}")]
    UnsupportedFormat {
        /// What the device reported.
        detail: String,
    },

    /// The device vanished mid-session.
    #[error("the audio device was lost")]
    DeviceLost,

    /// The platform backend could not be initialised at all.
    #[error("audio backend unavailable: {detail}")]
    BackendUnavailable {
        /// Diagnostic detail for the log, not for the user.
        detail: String,
    },
}

impl CaptureError {
    /// Whether retrying could plausibly succeed.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(self, Self::DeviceBusy | Self::DeviceLost | Self::NoOutputDevice)
    }
}

/// Anything that owns an OS audio stream and stops it when dropped.
///
/// A blanket implementation covers every concrete guard, so a backend just returns its own
/// type and the session stores it type-erased. Dropping the session drops the guard, which is
/// what makes `RQ-AUD-008` structural rather than a cleanup step somebody must remember.
pub trait CaptureGuard: Send + fmt::Debug {}

impl<T: Send + fmt::Debug> CaptureGuard for T {}

/// A live capture, tying the audio source to its consumer and its counters.
#[derive(Debug)]
pub struct CaptureSession {
    format: AudioFormat,
    frames: FrameReceiver,
    counters: Arc<SessionCounters>,
    /// Stops the OS stream on drop. Only ever moved, never called; its `Drop` is the point.
    guard: Box<dyn CaptureGuard>,
}

impl CaptureSession {
    /// Assembles a session from a backend's parts.
    #[must_use]
    pub fn new(
        format: AudioFormat,
        frames: FrameReceiver,
        counters: Arc<SessionCounters>,
        guard: impl CaptureGuard + 'static,
    ) -> Self {
        Self { format, frames, counters, guard: Box::new(guard) }
    }

    /// The negotiated capture format.
    #[must_use]
    pub const fn format(&self) -> AudioFormat {
        self.format
    }

    /// Mutable access to the consumer half of the ring.
    pub const fn frames_mut(&mut self) -> &mut FrameReceiver {
        &mut self.frames
    }

    /// Splits the session into its consumer and a handle that keeps the device alive.
    ///
    /// Used by the engine, which hands the receiver to the stream server while retaining the
    /// guard for the session's lifetime.
    #[must_use]
    pub fn split(self) -> (FrameReceiver, Arc<SessionCounters>, Box<dyn CaptureGuard>) {
        (self.frames, self.counters, self.guard)
    }

    /// Shared counters, safe to poll from any thread.
    #[must_use]
    pub fn counters(&self) -> Arc<SessionCounters> {
        Arc::clone(&self.counters)
    }

    /// A snapshot of the counters.
    #[must_use]
    pub fn stats(&self) -> CounterSnapshot {
        self.counters.snapshot()
    }
}

/// A backend able to observe the system's audio output.
#[async_trait]
pub trait AudioCapture: Send + Sync + fmt::Debug {
    /// The format this backend expects to produce, if known before starting.
    fn format_hint(&self) -> Option<AudioFormat>;

    /// Begins capture.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] when no device is available, the device is busy, or the
    /// backend cannot be initialised.
    async fn start(&self) -> Result<CaptureSession, CaptureError>;
}

/// Test-only allocation probe.
///
/// # Why this module contains the workspace's only `unsafe`
///
/// `RQ-AUD-001` forbids allocation on the producer path. The only way to *prove* that, rather
/// than assert it in a comment, is to count allocations while the path runs — and counting
/// allocations requires implementing [`std::alloc::GlobalAlloc`], which is an `unsafe trait`.
///
/// The exemption is confined to `#[cfg(test)]`, so no `unsafe` reaches a shipped binary.
/// Recorded in `docs/adr/0004-test-only-allocation-probe.md`.
///
/// The counter is thread-local, so tests running in parallel cannot perturb one another's
/// measurements.
#[cfg(test)]
#[allow(unsafe_code, reason = "GlobalAlloc is an unsafe trait; see the module docs and ADR-0004")]
#[allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` and `redundant_pub_crate` disagree here; `pub(crate)` is the               visibility that is actually true, so the rustc lint wins"
)]
pub(crate) mod alloc_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        /// `Some(n)` while a measurement is running on this thread; `None` otherwise.
        static COUNTER: Cell<Option<u64>> = const { Cell::new(None) };
    }

    /// Delegates to the system allocator, counting calls on threads that are measuring.
    pub(crate) struct Counting;

    // SAFETY: every method forwards directly to `System`, which upholds the `GlobalAlloc`
    // contract. The added bookkeeping touches only a thread-local `Cell<Option<u64>>` and
    // never allocates, so it cannot re-enter the allocator. `try_with` is used because a
    // thread-local may already be destroyed during thread teardown, when `with` would panic.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            bump();
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            bump();
            unsafe { System.realloc(ptr, layout, new_size) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            bump();
            unsafe { System.alloc_zeroed(layout) }
        }
    }

    fn bump() {
        let _ = COUNTER.try_with(|cell| {
            if let Some(n) = cell.get() {
                cell.set(Some(n + 1));
            }
        });
    }

    /// Runs `body` and returns how many allocations it performed on this thread.
    pub(crate) fn count(body: impl FnOnce()) -> u64 {
        COUNTER.with(|cell| cell.set(Some(0)));
        body();
        COUNTER.with(|cell| cell.replace(None)).unwrap_or(0)
    }
}

#[cfg(test)]
#[global_allocator]
static GLOBAL: alloc_probe::Counting = alloc_probe::Counting;

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

    use std::sync::atomic::{AtomicBool, Ordering};

    use rincon_core::audio::SampleEncoding;

    use super::*;

    fn format() -> AudioFormat {
        AudioFormat::new(48_000, 2, SampleEncoding::F32Le).unwrap()
    }

    /// A guard that records whether it was dropped, so the lifetime contract is observable.
    #[derive(Debug)]
    struct SpyGuard(Arc<AtomicBool>);

    impl Drop for SpyGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    /// Covers: RQ-AUD-008, RQ-SEC-007
    #[test]
    fn rq_aud_008_drop_stops_capture() {
        let dropped = Arc::new(AtomicBool::new(false));
        let counters = SessionCounters::new();
        let (_tx, rx) = ring::channel(format(), 100, Arc::clone(&counters));

        let session = CaptureSession::new(format(), rx, counters, SpyGuard(Arc::clone(&dropped)));
        assert!(!dropped.load(Ordering::Acquire), "the guard must live while the session does");

        drop(session);
        assert!(
            dropped.load(Ordering::Acquire),
            "dropping the session must release the OS stream handle"
        );
    }

    /// Covers: RQ-SEC-008
    #[test]
    fn rq_sec_008_no_implicit_disk_writes() {
        // Structural guarantee: this crate has no filesystem API in scope. The assertion is
        // that no backend can write audio anywhere, because there is nothing here to write
        // with -- `std::fs` is not imported, and the dependency list carries no file crate.
        let manifest = include_str!("../Cargo.toml");
        for forbidden in ["hound", "symphonia", "tempfile"] {
            assert!(
                !manifest.contains(forbidden),
                "`{forbidden}` would give the capture path a way to persist audio"
            );
        }
        let sources =
            [include_str!("ring.rs"), include_str!("convert.rs"), include_str!("synthetic.rs")];
        for source in sources {
            assert!(!source.contains("std::fs"), "the audio path must not touch the filesystem");
            assert!(!source.contains("File::create"), "the audio path must not create files");
        }
    }

    #[test]
    fn capture_errors_classify_retryability_correctly() {
        assert!(CaptureError::DeviceBusy.retryable(), "another app may release the device");
        assert!(CaptureError::DeviceLost.retryable(), "a reconnected device can be reopened");
        assert!(CaptureError::NoOutputDevice.retryable(), "the user can plug speakers in");
        assert!(
            !CaptureError::BackendUnavailable { detail: "no WASAPI".into() }.retryable(),
            "a missing backend will not fix itself"
        );
        assert!(!CaptureError::UnsupportedFormat { detail: "32ch".into() }.retryable());
    }

    #[test]
    fn the_allocation_probe_actually_detects_allocations() {
        // A probe that always reports zero would make RQ-AUD-001 vacuous, so prove it counts.
        let observed = alloc_probe::count(|| {
            let v: Vec<u8> = Vec::with_capacity(4096);
            std::hint::black_box(&v);
        });
        assert!(observed >= 1, "the allocation probe did not observe an obvious allocation");

        let quiet = alloc_probe::count(|| {
            let sum: u64 = (0..1000).sum();
            std::hint::black_box(sum);
        });
        assert_eq!(quiet, 0, "pure arithmetic must not register as an allocation");
    }
}
