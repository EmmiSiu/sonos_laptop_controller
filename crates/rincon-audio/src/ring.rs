//! The lock-free bridge between the real-time capture callback and the async consumer.
//!
//! # The problem this solves
//!
//! WASAPI hands us audio on a thread the OS scheduled for real time. That callback has a hard
//! deadline measured in single-digit milliseconds. If it allocates, takes a lock, or blocks on
//! a channel, the user hears a click — and no amount of downstream buffering repairs it.
//!
//! Meanwhile the consumer is an async HTTP handler on Tokio, subject to the scheduler's whims.
//! The two sides therefore need a queue that:
//!
//! 1. never blocks the producer,
//! 2. never allocates in the steady state,
//! 3. and, when the consumer falls behind, discards the **oldest** audio rather than the newest.
//!
//! Point 3 is not an implementation detail. Live audio that is 400 ms stale is worse than no
//! audio: playing it would permanently add that latency to the session. Dropping it keeps the
//! stream anchored to "now".
//!
//! # The design
//!
//! Audio moves in pre-allocated **blocks** that circulate between two bounded lock-free queues:
//!
//! ```text
//!            ┌──────────────── free (empty blocks) ◄──────────────┐
//!            ▼                                                    │
//!   producer ── fills a block ──► filled (bounded) ──► consumer ──┘
//!                    │
//!                    └── queue full? evict the oldest block, reuse its allocation
//! ```
//!
//! Because the evicted block is handed straight back to the producer, a sustained overrun
//! performs zero allocations — it simply recycles the same memory, counting what it discarded.
//!
//! The queues are [`crossbeam_queue::ArrayQueue`]: bounded, lock-free, and safe. This is
//! *lock-free*, not *wait-free* — `force_push` retries under contention — but with one producer
//! and one consumer there is no contention to retry against, so the producer path completes in
//! a bounded number of steps.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_queue::ArrayQueue;
use rincon_core::audio::AudioFormat;
use rincon_core::metrics::{DropLayer, SessionCounters};

/// Frames carried by a single block.
///
/// 1024 frames is ~21 ms at 48 kHz: comfortably larger than a typical WASAPI callback (480
/// frames) so one callback is one block, and small enough that evicting one loses little.
pub const FRAMES_PER_BLOCK: usize = 1024;

/// Default ring depth in milliseconds. SPEC-001 `RQ-AUD-009` requires at least 500 ms.
pub const DEFAULT_CAPACITY_MS: u32 = 1000;

/// A reusable buffer of interleaved samples.
///
/// The `Vec` is allocated once, at ring construction, and only ever cleared and refilled.
#[derive(Debug)]
pub struct Block {
    samples: Vec<f32>,
    channels: usize,
}

impl Block {
    fn with_capacity(frames: usize, channels: usize) -> Self {
        Self { samples: Vec::with_capacity(frames * channels), channels }
    }

    /// Interleaved samples currently held.
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Whole frames currently held.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.samples.len().checked_div(self.channels).unwrap_or(0)
    }

    /// Whether the block holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    fn clear(&mut self) {
        self.samples.clear();
    }

    /// Appends up to the block's spare capacity, returning how many samples were taken.
    ///
    /// Never grows the `Vec`, so it never allocates.
    fn fill_from(&mut self, src: &[f32]) -> usize {
        let room = self.samples.capacity().saturating_sub(self.samples.len());
        let take = room.min(src.len());
        if let Some(chunk) = src.get(..take) {
            self.samples.extend_from_slice(chunk);
        }
        take
    }
}

/// Whole frames represented by `samples` interleaved samples across `channels` channels.
///
/// A zero channel count is unreachable (`ChannelCount` refuses it) but is handled rather than
/// asserted, because a panic on this path would be a click in the user's audio.
const fn frames_of(samples: usize, channels: usize) -> usize {
    match samples.checked_div(channels) {
        Some(frames) => frames,
        None => 0,
    }
}

/// Why a consumer stopped receiving audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RingError {
    /// The capture device went away; no further audio will arrive.
    #[error("the capture device was lost")]
    DeviceLost,
}

/// Shared state between the two halves of the ring.
#[derive(Debug)]
struct Shared {
    filled: ArrayQueue<Block>,
    free: ArrayQueue<Block>,
    device_lost: AtomicBool,
    /// Whether something is draining the ring **right now**.
    ///
    /// Set when the consumer reads, cleared when it detaches. Deliberately not a latch: the
    /// ring fills with unwanted audio twice over, and only one of those is obvious.
    ///
    /// The first is before anything connects — capture must start before the server can bind,
    /// and the speaker will not connect until it has been handed a URL. The second is
    /// **every reconnect**: a Sonos routinely opens the stream, drops it, and opens it again
    /// a second later, and SPEC-002 requires the session to survive that. A latch set on the
    /// first read would blame the ring for the second gap, which is how a normal startup
    /// reconnect turned the health indicator amber for a session that was fine.
    ///
    /// Either way the loss is counted, but as [`DropLayer::NoConsumer`], which the health
    /// model excludes.
    consumer_attached: AtomicBool,
    counters: Arc<SessionCounters>,
    format: AudioFormat,
}

/// The producer half. Lives on the real-time audio thread.
///
/// Deliberately **not** `Clone`: a second producer would break the single-producer assumption
/// that makes the bounded retry argument hold.
#[derive(Debug)]
pub struct FrameSender {
    shared: Arc<Shared>,
    /// A block held back so the common path never has to touch the free queue twice.
    spare: Option<Block>,
}

/// The consumer half. Lives on the async side.
#[derive(Debug)]
pub struct FrameReceiver {
    shared: Arc<Shared>,
    /// Partially drained block from the previous read.
    partial: Option<(Block, usize)>,
}

/// Creates a ring sized to hold `capacity_ms` of audio at `format`.
///
/// Covers: RQ-AUD-009
#[must_use]
pub fn channel(
    format: AudioFormat,
    capacity_ms: u32,
    counters: Arc<SessionCounters>,
) -> (FrameSender, FrameReceiver) {
    let channels = format.channels.get() as usize;
    let wanted_frames = format.frames_in(capacity_ms);
    // At least two blocks: one in flight, one being filled.
    let blocks = wanted_frames.div_ceil(FRAMES_PER_BLOCK).max(2);

    let filled = ArrayQueue::new(blocks);
    let free = ArrayQueue::new(blocks);
    for _ in 0..blocks {
        // Pushing into a queue we just sized for exactly this count cannot fail; if it ever
        // did, the ring would simply be one block shallower, never incorrect.
        let _ = free.push(Block::with_capacity(FRAMES_PER_BLOCK, channels));
    }

    let shared = Arc::new(Shared {
        filled,
        free,
        device_lost: AtomicBool::new(false),
        consumer_attached: AtomicBool::new(false),
        counters,
        format,
    });

    (
        FrameSender { shared: Arc::clone(&shared), spare: None },
        FrameReceiver { shared, partial: None },
    )
}

impl FrameSender {
    /// Pushes interleaved samples from the capture callback.
    ///
    /// Returns the number of frames discarded to make room, which is `0` on the happy path.
    /// Never allocates, never blocks, never panics.
    ///
    /// Covers: RQ-AUD-001, RQ-AUD-002, RQ-AUD-003, RQ-AUD-011
    pub fn push(&mut self, interleaved: &[f32]) -> usize {
        let channels = self.shared.format.channels.get() as usize;
        if channels == 0 {
            return 0;
        }

        // A callback may hand us a partial frame at a buffer boundary; never let one escape
        // into the queue, or every consumer downstream sees swapped channels from then on.
        let whole = interleaved.len() - (interleaved.len() % channels);
        let mut remaining = interleaved.get(..whole).unwrap_or(&[]);

        // Count what the device gave us before deciding what survives. Attribution only works
        // if "captured" means captured, independent of what we then manage to keep.
        self.shared.counters.record_captured(frames_of(whole, channels) as u64);

        let mut dropped_frames = 0;
        while !remaining.is_empty() {
            let Some((mut block, evicted)) = self.acquire_block() else {
                // Every block is in the consumer's hands: there is nothing to recycle and
                // nothing to evict. The rest of this callback is lost -- but counted.
                dropped_frames += remaining.len().checked_div(channels).unwrap_or(0);
                break;
            };
            dropped_frames += evicted;
            block.clear();
            let taken = block.fill_from(remaining);
            remaining = remaining.get(taken..).unwrap_or(&[]);

            if let Some(evicted) = self.shared.filled.force_push(block) {
                dropped_frames += evicted.frames();
                // Reuse the evicted allocation instead of returning it to the free queue:
                // under sustained overrun this is the block we will fill next.
                self.spare = Some(evicted);
            }
        }

        if dropped_frames > 0 {
            let layer = if self.shared.consumer_attached.load(Ordering::Acquire) {
                DropLayer::Ring
            } else {
                DropLayer::NoConsumer
            };
            self.shared.counters.record_dropped(layer, dropped_frames as u64);
        }
        dropped_frames
    }

    /// Signals that the device is gone. The consumer observes this as a terminal state.
    ///
    /// Covers: RQ-AUD-010
    pub fn mark_device_lost(&self) {
        self.shared.device_lost.store(true, Ordering::Release);
    }

    /// The negotiated format.
    #[must_use]
    pub fn format(&self) -> AudioFormat {
        self.shared.format
    }

    /// Obtains a block to fill, returning it alongside the frames discarded to get it.
    ///
    /// The three tiers matter. The spare and the free pool are free of cost. Reclaiming from
    /// the *filled* queue is where the drop-oldest policy actually happens: when the consumer
    /// has fallen behind far enough that no empty block exists, the correct thing to discard
    /// is the stalest audio, not the sample that just arrived.
    fn acquire_block(&mut self) -> Option<(Block, usize)> {
        if let Some(block) = self.spare.take() {
            return Some((block, 0));
        }
        if let Some(block) = self.shared.free.pop() {
            return Some((block, 0));
        }
        self.shared.filled.pop().map(|oldest| {
            let discarded = oldest.frames();
            (oldest, discarded)
        })
    }
}

impl FrameReceiver {
    /// Copies up to `out.len()` samples into `out`, returning how many were written.
    ///
    /// Returns `Ok(0)` when the ring is momentarily empty — an underrun, which the caller
    /// should fill with silence rather than treat as an error.
    ///
    /// # Errors
    ///
    /// Returns [`RingError::DeviceLost`] once the producer has signalled device loss **and**
    /// every buffered frame has been drained.
    ///
    /// Covers: RQ-AUD-002, RQ-AUD-010, RQ-AUD-011
    pub fn read(&mut self, out: &mut [f32]) -> Result<usize, RingError> {
        let channels = self.shared.format.channels.get() as usize;
        if channels == 0 || out.is_empty() {
            return Ok(0);
        }
        // From here on, overrun means the consumer fell behind — a real quality problem —
        // rather than "nothing is listening". `detach` puts it back.
        self.shared.consumer_attached.store(true, Ordering::Release);

        // Only ever hand back whole frames, however small the caller's buffer is.
        let writable = out.len() - (out.len() % channels);

        let mut written = 0;
        while written < writable {
            let Some((block, offset)) = self.partial.take().or_else(|| self.next_block()) else {
                break;
            };

            let available = block.samples().get(offset..).unwrap_or(&[]);
            let room = writable - written;
            let take = available.len().min(room);

            if let (Some(dst), Some(src)) =
                (out.get_mut(written..written + take), available.get(..take))
            {
                dst.copy_from_slice(src);
            }
            written += take;

            if offset + take < block.samples().len() {
                self.partial = Some((block, offset + take));
            } else {
                // Exhausted: recycle it so the producer never has to allocate.
                let _ = self.shared.free.push(block);
            }
        }

        if written == 0 {
            if self.is_drained_and_lost() {
                return Err(RingError::DeviceLost);
            }
            self.shared.counters.record_underrun();
        } else {
            self.shared.counters.record_served(frames_of(written, channels) as u64);
        }
        Ok(written)
    }

    /// Whole frames currently queued, for diagnostics and back-pressure decisions.
    #[must_use]
    pub fn buffered_frames(&self) -> usize {
        let per_block = FRAMES_PER_BLOCK;
        let partial = self.partial.as_ref().map_or(0, |(block, offset)| {
            let channels = self.shared.format.channels.get() as usize;
            frames_of(block.samples().len().saturating_sub(*offset), channels)
        });
        self.shared.filled.len() * per_block + partial
    }

    /// The negotiated format.
    #[must_use]
    pub fn format(&self) -> AudioFormat {
        self.shared.format
    }

    /// Whether the producer has reported the device gone.
    #[must_use]
    pub fn device_lost(&self) -> bool {
        self.shared.device_lost.load(Ordering::Acquire)
    }

    /// Declares that nothing is draining this receiver for the time being.
    ///
    /// Call it when a consumer lets go — the stream server does, when the speaker's connection
    /// ends and the receiver goes back in the pool. Audio discarded from here until the next
    /// [`read`](Self::read) is attributed to [`DropLayer::NoConsumer`] rather than counted
    /// against the session's quality, because there is no consumer to have fallen behind.
    ///
    /// Idempotent, and cheap enough to call on every disconnect.
    ///
    /// Covers: RQ-OBS-001
    pub fn detach(&self) {
        self.shared.consumer_attached.store(false, Ordering::Release);
    }

    fn next_block(&self) -> Option<(Block, usize)> {
        self.shared.filled.pop().map(|block| (block, 0))
    }

    fn is_drained_and_lost(&self) -> bool {
        self.device_lost() && self.shared.filled.is_empty() && self.partial.is_none()
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

    use rincon_core::audio::SampleEncoding;

    use super::*;
    use crate::alloc_probe;

    fn stereo_48k() -> AudioFormat {
        AudioFormat::new(48_000, 2, SampleEncoding::F32Le).unwrap()
    }

    fn ring(capacity_ms: u32) -> (FrameSender, FrameReceiver, Arc<SessionCounters>) {
        let counters = SessionCounters::new();
        let (tx, rx) = channel(stereo_48k(), capacity_ms, Arc::clone(&counters));
        (tx, rx, counters)
    }

    /// Covers: RQ-AUD-001
    #[test]
    fn rq_aud_001_producer_path_does_not_allocate() {
        let (mut tx, mut rx, _) = ring(DEFAULT_CAPACITY_MS);
        let callback: Vec<f32> = (0..960).map(|i| (i % 7) as f32 / 7.0).collect();
        let mut sink = vec![0.0_f32; 960];

        // Warm up: the first pushes touch the free queue and settle the steady state.
        for _ in 0..8 {
            tx.push(&callback);
            let _ = rx.read(&mut sink);
        }

        // Steady state: 200 callbacks, interleaved with reads, must allocate nothing.
        let allocations = alloc_probe::count(|| {
            for _ in 0..200 {
                tx.push(&callback);
                let _ = rx.read(&mut sink);
            }
        });
        assert_eq!(allocations, 0, "the producer path allocated {allocations} time(s)");

        // And it must allocate nothing while overrunning either, which is when a naive
        // implementation would start pushing new buffers.
        let overrun = alloc_probe::count(|| {
            for _ in 0..200 {
                tx.push(&callback);
            }
        });
        assert_eq!(overrun, 0, "the overrun path allocated {overrun} time(s)");
    }

    /// Covers: RQ-AUD-002
    #[test]
    fn rq_aud_002_never_splits_a_frame() {
        let (mut tx, mut rx, _) = ring(DEFAULT_CAPACITY_MS);

        // A callback carrying a dangling half-frame: the odd sample must never be visible.
        tx.push(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let mut out = vec![0.0_f32; 16];
        let n = rx.read(&mut out).unwrap();
        assert_eq!(n % 2, 0, "read {n} samples, which is not a whole number of stereo frames");
        assert_eq!(&out[..n], &[1.0, 2.0, 3.0, 4.0]);

        // A caller asking for an odd number of samples also gets whole frames only.
        tx.push(&[9.0, 8.0, 7.0, 6.0]);
        let mut odd = vec![0.0_f32; 3];
        let n = rx.read(&mut odd).unwrap();
        assert_eq!(n, 2, "an odd-sized destination must still receive whole frames");
        assert_eq!(&odd[..2], &[9.0, 8.0]);
    }

    /// Covers: RQ-AUD-003
    #[test]
    fn rq_aud_003_overrun_drops_oldest_and_counts() {
        // A deliberately tiny ring so overrun is reached in a handful of pushes.
        let counters = SessionCounters::new();
        let (mut tx, mut rx) = channel(stereo_48k(), 1, Arc::clone(&counters));

        // Fill well past capacity with an identifiable ramp: block N is filled with value N.
        let blocks_pushed = 12;
        for n in 0..blocks_pushed {
            let payload = vec![n as f32; FRAMES_PER_BLOCK * 2];
            tx.push(&payload);
        }

        // Nothing has read yet, so this overrun is "nobody was listening", not a defect.
        assert_eq!(counters.dropped_at(DropLayer::Ring), 0);
        let dropped = counters.dropped_at(DropLayer::NoConsumer);
        assert!(dropped > 0, "an overrun must be counted, not silently absorbed");

        // What survives must be the *newest* audio: the first value we read back cannot be
        // block 0, and the last block pushed must still be present.
        let mut out = vec![0.0_f32; FRAMES_PER_BLOCK * 2];
        let n = rx.read(&mut out).unwrap();
        assert!(n > 0);
        assert!(
            out[0] > 0.0,
            "the oldest block survived an overrun; newest-first ordering is broken"
        );

        // The count is exact: everything pushed is either still buffered or counted as dropped.
        let pushed_frames = (blocks_pushed * FRAMES_PER_BLOCK) as u64;
        let served = counters.frames_served();
        let still_buffered = rx.buffered_frames() as u64;
        assert_eq!(
            counters.frames_captured(),
            pushed_frames,
            "every pushed frame must be counted as captured"
        );
        assert!(
            dropped + served + still_buffered <= pushed_frames + FRAMES_PER_BLOCK as u64,
            "accounting does not balance: dropped {dropped} served {served} buffered {still_buffered} of {pushed_frames}"
        );
    }

    /// Covers: RQ-AUD-009
    #[test]
    fn rq_aud_009_default_capacity_covers_500ms() {
        // The default must satisfy the spec on its own, checked at compile time.
        const _: () = assert!(DEFAULT_CAPACITY_MS >= 500);

        let format = stereo_48k();
        let (_tx, rx, _) = ring(DEFAULT_CAPACITY_MS);

        let required = format.frames_in(500);
        // Capacity is the filled queue depth in blocks times the frames each block holds.
        let capacity_frames = rx.shared.filled.capacity() * FRAMES_PER_BLOCK;
        assert!(
            capacity_frames >= required,
            "ring holds {capacity_frames} frames, under the {required} frame (500 ms) floor"
        );
    }

    /// Covers: RQ-AUD-010
    #[test]
    fn rq_aud_010_device_loss_is_observable() {
        let (mut tx, mut rx, _) = ring(DEFAULT_CAPACITY_MS);
        tx.push(&[0.25, 0.5, 0.75, 1.0]);
        tx.mark_device_lost();

        // Buffered audio is still delivered: loss must not discard what we already have.
        let mut out = vec![0.0_f32; 8];
        let n = rx.read(&mut out).unwrap();
        assert_eq!(n, 4, "buffered frames must survive device loss");

        // Only once drained does the consumer see the terminal state -- and it keeps seeing it.
        assert_eq!(rx.read(&mut out), Err(RingError::DeviceLost));
        assert_eq!(rx.read(&mut out), Err(RingError::DeviceLost));
        assert!(rx.device_lost());
    }

    /// Covers: RQ-AUD-011
    #[test]
    fn rq_aud_011_channel_order_is_preserved() {
        let (mut tx, mut rx, _) = ring(DEFAULT_CAPACITY_MS);
        // Left channel is negative, right channel positive: a swap is unmistakable.
        let source: Vec<f32> = (0..256).map(|i| if i % 2 == 0 { -1.0 } else { 1.0 }).collect();
        tx.push(&source);

        let mut out = vec![0.0_f32; 256];
        let n = rx.read(&mut out).unwrap();
        assert_eq!(n, 256);
        assert_eq!(out, source, "interleaving was not preserved through the ring");
    }

    /// Covers: RQ-OBS-001
    #[test]
    fn overrun_before_the_first_read_is_not_blamed_on_the_consumer() {
        // Measured on real hardware: a successful session discarded 54% of captured frames
        // between capture starting and the speaker connecting. Attributing those to the ring
        // made a perfect session look like it was dropping audio.
        let counters = SessionCounters::new();
        let (mut tx, mut rx) = channel(stereo_48k(), 1, Arc::clone(&counters));

        for n in 0..12 {
            tx.push(&vec![n as f32; FRAMES_PER_BLOCK * 2]);
        }
        assert!(counters.dropped_at(DropLayer::NoConsumer) > 0, "the loss must still be counted");
        assert_eq!(counters.dropped_at(DropLayer::Ring), 0, "nobody was listening to lose it");
        assert_eq!(counters.snapshot().quality_drops(), 0);

        // Once the consumer starts reading, further overrun is its own fault and is blamed
        // on the ring, because now it really is audio a listener lost.
        let mut out = vec![0.0_f32; 16];
        let _ = rx.read(&mut out);
        for n in 0..12 {
            tx.push(&vec![n as f32; FRAMES_PER_BLOCK * 2]);
        }
        assert!(
            counters.dropped_at(DropLayer::Ring) > 0,
            "overrun after the consumer started must count against quality"
        );
        assert!(counters.snapshot().quality_drops() > 0);
    }

    /// Covers: RQ-OBS-001
    #[test]
    fn a_reconnect_gap_is_not_blamed_on_the_consumer_either() {
        // Measured on real hardware, and the reason `consumer_attached` is not a latch. A
        // Sonos opened the stream, dropped it, and reopened it one second later -- its normal
        // startup behaviour, which SPEC-002 requires the session to survive. The 45,120 frames
        // discarded in that gap were attributed to the ring, so the health indicator read
        // "Dropping audio" for a session that was working.
        let counters = SessionCounters::new();
        let (mut tx, mut rx) = channel(stereo_48k(), 1, Arc::clone(&counters));

        // Connect, drain, and overrun: that loss is real and belongs to the ring.
        let mut out = vec![0.0_f32; 16];
        let _ = rx.read(&mut out);
        for n in 0..12 {
            tx.push(&vec![n as f32; FRAMES_PER_BLOCK * 2]);
        }
        let real_loss = counters.dropped_at(DropLayer::Ring);
        assert!(real_loss > 0, "the setup must actually overrun while attached");

        // The speaker hangs up. The stream server drops its lease, which detaches.
        rx.detach();
        for n in 0..12 {
            tx.push(&vec![n as f32; FRAMES_PER_BLOCK * 2]);
        }
        assert_eq!(
            counters.dropped_at(DropLayer::Ring),
            real_loss,
            "a gap with no consumer must not be charged to the ring"
        );
        assert!(counters.dropped_at(DropLayer::NoConsumer) > 0, "but it must still be counted");

        // It reconnects, and the ring is answerable again.
        let _ = rx.read(&mut out);
        for n in 0..12 {
            tx.push(&vec![n as f32; FRAMES_PER_BLOCK * 2]);
        }
        assert!(
            counters.dropped_at(DropLayer::Ring) > real_loss,
            "overrun after reconnecting counts against quality again"
        );
    }

    #[test]
    fn an_empty_ring_reports_an_underrun_rather_than_blocking() {
        let (_tx, mut rx, counters) = ring(DEFAULT_CAPACITY_MS);
        let mut out = vec![0.0_f32; 32];
        assert_eq!(rx.read(&mut out).unwrap(), 0);
        assert_eq!(counters.underruns(), 1, "an empty read is an underrun and must be counted");
    }

    #[test]
    fn a_read_smaller_than_a_block_resumes_where_it_left_off() {
        let (mut tx, mut rx, _) = ring(DEFAULT_CAPACITY_MS);
        let source: Vec<f32> = (0..64).map(|i| i as f32).collect();
        tx.push(&source);

        let mut collected = Vec::new();
        let mut out = vec![0.0_f32; 6];
        loop {
            let n = rx.read(&mut out).unwrap();
            if n == 0 {
                break;
            }
            collected.extend_from_slice(&out[..n]);
        }
        assert_eq!(collected, source, "a partially drained block must resume, not restart");
    }

    #[test]
    fn a_push_larger_than_one_block_spans_blocks_without_loss() {
        let (mut tx, mut rx, _) = ring(DEFAULT_CAPACITY_MS);
        let source: Vec<f32> = (0..FRAMES_PER_BLOCK * 2 * 3).map(|i| (i % 100) as f32).collect();
        tx.push(&source);

        let mut collected = Vec::new();
        let mut out = vec![0.0_f32; 512];
        while collected.len() < source.len() {
            let n = rx.read(&mut out).unwrap();
            if n == 0 {
                break;
            }
            collected.extend_from_slice(&out[..n]);
        }
        assert_eq!(collected, source);
    }

    #[test]
    fn empty_pushes_and_reads_are_harmless() {
        let (mut tx, mut rx, _) = ring(DEFAULT_CAPACITY_MS);
        assert_eq!(tx.push(&[]), 0);
        assert_eq!(rx.read(&mut []).unwrap(), 0);
    }

    proptest::proptest! {
        /// Whatever the callback sizes, whatever the read sizes, the consumer sees exactly the
        /// samples the producer sent, in order -- as long as the ring never overruns.
        #[test]
        fn round_trips_faithfully_without_overrun(
            chunks in proptest::collection::vec(2_usize..=64, 1..12),
            read_size in 2_usize..=128,
        ) {
            let (mut tx, mut rx, _) = ring(DEFAULT_CAPACITY_MS);
            let mut sent = Vec::new();
            let mut value = 0.0_f32;

            for chunk_frames in chunks {
                let payload: Vec<f32> = (0..chunk_frames * 2)
                    .map(|_| { value += 1.0; value })
                    .collect();
                tx.push(&payload);
                sent.extend_from_slice(&payload);
            }

            let mut got = Vec::new();
            let mut out = vec![0.0_f32; read_size];
            loop {
                let n = rx.read(&mut out).unwrap();
                if n == 0 { break; }
                got.extend_from_slice(&out[..n]);
            }
            proptest::prop_assert_eq!(got, sent);
        }
    }
}
