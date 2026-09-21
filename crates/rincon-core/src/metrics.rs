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
    /// Discarded because nothing was listening yet.
    ///
    /// Capture starts several seconds before the speaker fetches the stream — it has to, since
    /// the server needs a source before it can bind, and the speaker will not connect until it
    /// has been handed a URL. Everything captured in that gap was never going to be served.
    ///
    /// It is counted, because silent loss is a defect and a diagnostics bundle should show it.
    /// It is a *separate* layer, because it is not a quality problem, and a health indicator
    /// that turns amber on every successful session is an indicator nobody reads.
    NoConsumer,
}

impl DropLayer {
    /// Every layer, for exhaustive reporting.
    pub const ALL: [Self; 5] =
        [Self::Capture, Self::Ring, Self::Socket, Self::Device, Self::NoConsumer];

    /// A short label for logs and the diagnostics bundle.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Ring => "ring",
            Self::Socket => "socket",
            Self::Device => "device",
            Self::NoConsumer => "no-consumer",
        }
    }

    /// Whether loss at this layer says anything about the quality of the audio a listener got.
    ///
    /// The health model is built from these only. See [`Health::evaluate`].
    #[must_use]
    pub const fn affects_quality(self) -> bool {
        !matches!(self, Self::NoConsumer)
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
    dropped: [AtomicU64; 5],
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
        // Indexing a fixed-size array with a value derived from a closed enum: the bound is
        // proven by the match, which is why `get` would only add noise here.
        if let Some(counter) = self.dropped.get(slot_of(layer)) {
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
        self.dropped.get(slot_of(layer)).map_or(0, |c| c.load(Ordering::Relaxed))
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
            dropped_no_consumer: self.dropped_at(DropLayer::NoConsumer),
        }
    }
}

/// The counter slot for a layer. One `match`, so the two accessors cannot disagree.
const fn slot_of(layer: DropLayer) -> usize {
    match layer {
        DropLayer::Capture => 0,
        DropLayer::Ring => 1,
        DropLayer::Socket => 2,
        DropLayer::Device => 3,
        DropLayer::NoConsumer => 4,
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
    /// Frames discarded before anything was listening. Not a quality problem.
    pub dropped_no_consumer: u64,
}

impl CounterSnapshot {
    /// Total frames lost across every layer, including the ones nobody was waiting for.
    ///
    /// This is the number a diagnostics bundle reports, because it is what happened.
    #[must_use]
    pub const fn dropped_total(&self) -> u64 {
        self.quality_drops() + self.dropped_no_consumer
    }

    /// Frames lost that a listener would have heard.
    ///
    /// This is the number the health model uses. The difference matters: a perfectly healthy
    /// session discards several seconds of audio between capture starting and the speaker
    /// connecting, and counting that as a defect would leave the indicator permanently amber.
    #[must_use]
    pub const fn quality_drops(&self) -> u64 {
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
            dropped_no_consumer: self
                .dropped_no_consumer
                .saturating_sub(earlier.dropped_no_consumer),
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
    /// Uses [`CounterSnapshot::quality_drops`] rather than the total: frames discarded before
    /// anything was listening are not a defect, and treating them as one would make every
    /// healthy session look degraded.
    ///
    /// Covers: RQ-OBS-009
    #[must_use]
    pub const fn evaluate(window: &CounterSnapshot, peer_connected: bool) -> Self {
        if !peer_connected {
            return Self::Lost;
        }
        if window.quality_drops() > 0 || window.underruns > 3 {
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

        // Frames nobody was waiting for are counted, but kept out of the quality figure.
        let session = SessionCounters::new();
        session.record_dropped(DropLayer::NoConsumer, 500_000);
        session.record_dropped(DropLayer::Ring, 3);
        let snapshot = session.snapshot();
        assert_eq!(snapshot.dropped_total(), 500_003, "diagnostics report what happened");
        assert_eq!(snapshot.quality_drops(), 3, "health counts only what a listener lost");

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
        assert_eq!(Health::evaluate(&losing, true), Health::Poor, "any real loss is poor");

        // The regression this pins down was measured on real hardware: a successful session
        // discarded 54% of captured frames while waiting for the speaker to connect. Counting
        // those would have shown "Poor" for a session the listener heard perfectly.
        let waiting = CounterSnapshot { dropped_no_consumer: 313_920, ..clean };
        assert_eq!(
            Health::evaluate(&waiting, true),
            Health::Good,
            "frames discarded before anyone was listening are not a quality problem"
        );

        // Disconnection dominates everything else.
        assert_eq!(Health::evaluate(&clean, false), Health::Lost);
        assert_eq!(Health::evaluate(&losing, false), Health::Lost);
    }

    #[test]
    fn every_layer_that_affects_quality_is_part_of_the_quality_sum() {
        // `quality_drops` is a hand-written sum over named fields. Adding a variant to
        // `DropLayer` without adding it there would silently exclude real loss from the health
        // model, and every existing test would still pass. This makes the two disagree loudly.
        for layer in DropLayer::ALL {
            let c = SessionCounters::new();
            c.record_dropped(layer, 1);
            let snapshot = c.snapshot();
            assert_eq!(snapshot.dropped_total(), 1, "{layer} is missing from dropped_total()");
            assert_eq!(
                snapshot.quality_drops() == 1,
                layer.affects_quality(),
                "{layer} disagrees with its own affects_quality()"
            );
        }
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
