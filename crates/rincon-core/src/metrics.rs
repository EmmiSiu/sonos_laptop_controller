//! Lock-free session counters and the health model derived from them.
//!
//! Rincon sends nothing home (SPEC-000 NG5), so the only diagnostic that will ever exist is the
//! one the user can see. That raises the bar for counting: every dropout must be attributed to
//! a layer, at the moment it happens, from a thread that may be a real-time audio callback.
//!
//! Hence `Relaxed` atomics and nothing else. A counter increment on the capture path must be a
//! single uncontended instruction — not a mutex, not a channel send, not an allocation.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Which layer lost the audio.
///
/// Attribution is the difference between a bug report that can be acted on and one that says
/// "it crackled". Each variant maps to a different fix, so they must never be merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropLayer {
    /// The OS reported a gap before we ever saw the samples. Machine under load.
    Capture,
    /// The ring overran: the consumer could not keep up with the producer.
    Ring,
    /// The socket back-pressured: the network could not absorb the stream.
    Socket,
    /// The speaker disconnected or stalled.
    Device,
}

impl DropLayer {
    /// Every layer, for exhaustive reporting.
    pub const ALL: [Self; 4] = [Self::Capture, Self::Ring, Self::Socket, Self::Device];

    /// A short label for logs and the diagnostics bundle.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Ring => "ring",
            Self::Socket => "socket",
            Self::Device => "device",
        }
    }
}

impl fmt::Display for DropLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Monotonic counters for one streaming session.
///
/// Cloneable via [`Arc`]; every clone observes the same counts. Reads never block, so the UI
/// may poll at any rate without perturbing audio.
#[derive(Debug, Default)]
pub struct SessionCounters {
    frames_captured: AtomicU64,
    frames_served: AtomicU64,
    underruns: AtomicU64,
    reconnects: AtomicU64,
    dropped: [AtomicU64; 4],
}

impl SessionCounters {
    /// A fresh set of counters, all zero.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Records `n` frames arriving from the capture backend.
    pub fn record_captured(&self, n: u64) {
        self.frames_captured.fetch_add(n, Ordering::Relaxed);
    }

    /// Records `n` frames written to the socket.
    pub fn record_served(&self, n: u64) {
        self.frames_served.fetch_add(n, Ordering::Relaxed);
    }

    /// Records `n` frames lost at `layer`.
    ///
    /// Covers: RQ-OBS-001
    pub fn record_dropped(&self, layer: DropLayer, n: u64) {
        let slot = match layer {
            DropLayer::Capture => 0,
            DropLayer::Ring => 1,
            DropLayer::Socket => 2,
            DropLayer::Device => 3,
        };
        // Indexing a fixed-size array with a value derived from a closed enum: the bound is
        // proven by the match above, which is why `get` would only add noise here.
        if let Some(counter) = self.dropped.get(slot) {
            counter.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Records the consumer finding the ring empty.
    pub fn record_underrun(&self) {
        self.underruns.fetch_add(1, Ordering::Relaxed);
    }

    /// Records the speaker re-establishing its connection.
    pub fn record_reconnect(&self) {
        self.reconnects.fetch_add(1, Ordering::Relaxed);
    }

    /// Frames received from the capture backend.
    #[must_use]
    pub fn frames_captured(&self) -> u64 {
        self.frames_captured.load(Ordering::Relaxed)
    }

    /// Frames written to the socket.
    #[must_use]
    pub fn frames_served(&self) -> u64 {
        self.frames_served.load(Ordering::Relaxed)
    }

    /// Times the consumer found the ring empty.
    #[must_use]
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Times the speaker reconnected.
    #[must_use]
    pub fn reconnects(&self) -> u64 {
        self.reconnects.load(Ordering::Relaxed)
    }

    /// Frames dropped at a specific layer.
    #[must_use]
    pub fn dropped_at(&self, layer: DropLayer) -> u64 {
        let slot = match layer {
            DropLayer::Capture => 0,
            DropLayer::Ring => 1,
            DropLayer::Socket => 2,
            DropLayer::Device => 3,
        };
        self.dropped.get(slot).map_or(0, |c| c.load(Ordering::Relaxed))
    }

    /// Total frames dropped across every layer.
    #[must_use]
    pub fn dropped_total(&self) -> u64 {
        self.dropped.iter().map(|c| c.load(Ordering::Relaxed)).sum()
    }

    /// An immutable snapshot, safe to serialise and send across the IPC boundary.
    #[must_use]
    pub fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            frames_captured: self.frames_captured(),
            frames_served: self.frames_served(),
            underruns: self.underruns(),
            reconnects: self.reconnects(),
            dropped_capture: self.dropped_at(DropLayer::Capture),
            dropped_ring: self.dropped_at(DropLayer::Ring),
            dropped_socket: self.dropped_at(DropLayer::Socket),
            dropped_device: self.dropped_at(DropLayer::Device),
        }
    }
}

/// A point-in-time copy of [`SessionCounters`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CounterSnapshot {
    /// Frames received from the capture backend.
    pub frames_captured: u64,
    /// Frames written to the socket.
    pub frames_served: u64,
    /// Times the consumer found the ring empty.
    pub underruns: u64,
    /// Times the speaker reconnected.
    pub reconnects: u64,
    /// Frames lost in the capture layer.
    pub dropped_capture: u64,
    /// Frames lost in the ring.
    pub dropped_ring: u64,
    /// Frames lost at the socket.
    pub dropped_socket: u64,
    /// Frames lost because the device went away.
    pub dropped_device: u64,
}

impl CounterSnapshot {
    /// Total frames lost across every layer.
    #[must_use]
    pub const fn dropped_total(&self) -> u64 {
        self.dropped_capture + self.dropped_ring + self.dropped_socket + self.dropped_device
    }

    /// Difference between two snapshots, for windowed health evaluation.
    #[must_use]
    pub const fn since(&self, earlier: &Self) -> Self {
        Self {
            frames_captured: self.frames_captured.saturating_sub(earlier.frames_captured),
            frames_served: self.frames_served.saturating_sub(earlier.frames_served),
            underruns: self.underruns.saturating_sub(earlier.underruns),
            reconnects: self.reconnects.saturating_sub(earlier.reconnects),
            dropped_capture: self.dropped_capture.saturating_sub(earlier.dropped_capture),
            dropped_ring: self.dropped_ring.saturating_sub(earlier.dropped_ring),
            dropped_socket: self.dropped_socket.saturating_sub(earlier.dropped_socket),
            dropped_device: self.dropped_device.saturating_sub(earlier.dropped_device),
        }
    }
}

/// What the user's health indicator shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// Clean audio.
    Good,
    /// Occasional underruns, no lost frames.
    Fair,
    /// Frames are being lost.
    Poor,
    /// The speaker is no longer connected.
    Lost,
}

impl Health {
    /// Derives health from a windowed counter delta.
    ///
    /// Written as a pure function of the delta so the indicator can never disagree with what
    /// was actually measured, and so the thresholds are unit-testable.
    ///
    /// Covers: RQ-OBS-009
    #[must_use]
    pub const fn evaluate(window: &CounterSnapshot, peer_connected: bool) -> Self {
        if !peer_connected {
            return Self::Lost;
        }
        if window.dropped_total() > 0 || window.underruns > 3 {
            return Self::Poor;
        }
        if window.underruns > 0 { Self::Fair } else { Self::Good }
    }

    /// A short label for the UI.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Good => "good",
            Self::Fair => "fair",
            Self::Poor => "poor",
            Self::Lost => "lost",
        }
    }
}

impl fmt::Display for Health {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
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

    /// Covers: RQ-OBS-001
    #[test]
    fn rq_obs_001_drops_are_attributed() {
        let c = SessionCounters::new();
        c.record_dropped(DropLayer::Ring, 128);
        c.record_dropped(DropLayer::Socket, 7);
        c.record_dropped(DropLayer::Ring, 2);

        assert_eq!(c.dropped_at(DropLayer::Ring), 130, "ring drops accumulate separately");
        assert_eq!(c.dropped_at(DropLayer::Socket), 7);
        assert_eq!(c.dropped_at(DropLayer::Capture), 0, "untouched layers stay zero");
        assert_eq!(c.dropped_at(DropLayer::Device), 0);
        assert_eq!(c.dropped_total(), 137);

        // Every layer must be individually addressable; a merged counter would fail this.
        for layer in DropLayer::ALL {
            let fresh = SessionCounters::new();
            fresh.record_dropped(layer, 1);
            assert_eq!(fresh.dropped_at(layer), 1, "{layer} is not separately counted");
            assert_eq!(fresh.dropped_total(), 1);
        }
    }

    /// Covers: RQ-OBS-002
    #[test]
    fn rq_obs_002_counters_are_lock_free() {
        // The structural guarantee: the counters are atomics, so concurrent writers from a
        // real-time thread and readers from the UI cannot block one another. This exercises
        // that with contention and asserts nothing is lost.
        let counters = SessionCounters::new();
        let writers: Vec<_> = (0..4)
            .map(|_| {
                let c = Arc::clone(&counters);
                std::thread::spawn(move || {
                    for _ in 0..10_000 {
                        c.record_captured(1);
                        c.record_underrun();
                    }
                })
            })
            .collect();

        // A reader polling throughout must never block or observe a torn value.
        let reader = {
            let c = Arc::clone(&counters);
            std::thread::spawn(move || {
                let mut last = 0;
                for _ in 0..1_000 {
                    let now = c.frames_captured();
                    assert!(now >= last, "counters must be monotonic under concurrency");
                    last = now;
                }
            })
        };

        for w in writers {
            w.join().expect("writer thread panicked");
        }
        reader.join().expect("reader thread panicked");

        assert_eq!(counters.frames_captured(), 40_000, "no increments lost");
        assert_eq!(counters.underruns(), 40_000);
    }

    /// Covers: RQ-OBS-009
    #[test]
    fn rq_obs_009_health_is_a_pure_function_of_the_window() {
        let clean = CounterSnapshot { frames_captured: 240_000, ..Default::default() };
        assert_eq!(Health::evaluate(&clean, true), Health::Good);

        let blips = CounterSnapshot { underruns: 2, ..clean };
        assert_eq!(Health::evaluate(&blips, true), Health::Fair);

        let many_blips = CounterSnapshot { underruns: 4, ..clean };
        assert_eq!(Health::evaluate(&many_blips, true), Health::Poor, "4 underruns is poor");

        let losing = CounterSnapshot { dropped_ring: 1, ..clean };
        assert_eq!(Health::evaluate(&losing, true), Health::Poor, "any loss is poor");

        // Disconnection dominates everything else.
        assert_eq!(Health::evaluate(&clean, false), Health::Lost);
        assert_eq!(Health::evaluate(&losing, false), Health::Lost);
    }

    #[test]
    fn health_thresholds_sit_exactly_where_the_spec_says() {
        let at_three = CounterSnapshot { underruns: 3, ..Default::default() };
        assert_eq!(Health::evaluate(&at_three, true), Health::Fair, "<= 3 underruns is fair");
        let at_four = CounterSnapshot { underruns: 4, ..Default::default() };
        assert_eq!(Health::evaluate(&at_four, true), Health::Poor);
    }

    #[test]
    fn windowed_deltas_never_underflow() {
        let earlier = CounterSnapshot { frames_captured: 100, underruns: 5, ..Default::default() };
        let later = CounterSnapshot { frames_captured: 350, underruns: 5, ..Default::default() };
        let delta = later.since(&earlier);
        assert_eq!(delta.frames_captured, 250);
        assert_eq!(delta.underruns, 0);

        // A reset mid-session must not produce a gigantic bogus delta.
        let reversed = earlier.since(&later);
        assert_eq!(reversed.frames_captured, 0);
    }

    #[test]
    fn snapshots_round_trip_through_serde_for_the_ipc_boundary() {
        let c = SessionCounters::new();
        c.record_captured(10);
        c.record_dropped(DropLayer::Device, 3);
        let snap = c.snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        assert_eq!(serde_json::from_str::<CounterSnapshot>(&json).unwrap(), snap);
    }
}
