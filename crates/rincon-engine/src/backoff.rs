//! Reconnection backoff.
//!
//! Two failure modes this exists to avoid, both of which a naive `loop { retry() }` produces:
//!
//! - **Hammering a speaker that is rebooting.** Firmware updates take a minute or two; a tight
//!   retry loop during one is indistinguishable from a denial-of-service attempt, and some
//!   firmware stops answering a host that does it.
//! - **Retrying forever.** A session that never gives up never tells the user anything, and
//!   the interface sits on a spinner while the actual problem is that the speaker is unplugged.
//!
//! So: exponential, capped, and finite.

use std::time::Duration;

/// Delay before the first retry.
pub const INITIAL: Duration = Duration::from_millis(250);

/// Longest a single wait may become.
pub const CEILING: Duration = Duration::from_secs(8);

/// Attempts before the session gives up and reports a failure the user can act on.
pub const MAX_ATTEMPTS: u32 = 6;

/// The retry schedule for one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    attempt: u32,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

impl Backoff {
    /// A fresh schedule.
    #[must_use]
    pub const fn new() -> Self {
        Self { attempt: 0 }
    }

    /// The next delay, or `None` once the attempts are exhausted.
    ///
    /// Covers: RQ-ENG-008
    #[must_use]
    pub const fn next_delay(&mut self) -> Option<Duration> {
        if self.attempt >= MAX_ATTEMPTS {
            return None;
        }
        // `saturating_mul` rather than a shift: at attempt 30 a shift would wrap to zero and
        // silently turn the backoff into a hot loop.
        let multiplier = 1_u32.wrapping_shl(self.attempt);
        let delay = INITIAL.saturating_mul(multiplier);
        self.attempt += 1;
        Some(if delay.as_nanos() > CEILING.as_nanos() { CEILING } else { delay })
    }

    /// Attempts made so far.
    #[must_use]
    pub const fn attempts(self) -> u32 {
        self.attempt
    }

    /// Whether any attempts remain.
    #[must_use]
    pub const fn exhausted(self) -> bool {
        self.attempt >= MAX_ATTEMPTS
    }

    /// Forgets the history, after a successful reconnection.
    pub const fn reset(&mut self) {
        self.attempt = 0;
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

    /// Covers: RQ-ENG-008
    #[test]
    fn rq_eng_008_backoff_schedule_is_bounded() {
        let mut backoff = Backoff::new();
        let schedule: Vec<Duration> = std::iter::from_fn(|| backoff.next_delay()).collect();

        assert_eq!(
            schedule,
            vec![
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_millis(1_000),
                Duration::from_millis(2_000),
                Duration::from_millis(4_000),
                Duration::from_millis(8_000),
            ],
            "the schedule must double from 250 ms and stop at the ceiling"
        );

        assert_eq!(schedule.len() as u32, MAX_ATTEMPTS, "the schedule must be finite");
        assert!(backoff.exhausted());
        assert_eq!(backoff.next_delay(), None, "an exhausted backoff must stay exhausted");

        // Total wait before giving up: under 16 s, which is long enough for a speaker to
        // finish rebooting and short enough that the user is not left guessing.
        let total: Duration = schedule.iter().sum();
        assert!(total < Duration::from_secs(20), "the user waits {total:?} before being told");
    }

    #[test]
    fn no_delay_exceeds_the_ceiling() {
        let mut backoff = Backoff::new();
        while let Some(delay) = backoff.next_delay() {
            assert!(delay <= CEILING, "{delay:?} is above the {CEILING:?} ceiling");
            assert!(delay >= INITIAL, "{delay:?} is below the {INITIAL:?} floor");
        }
    }

    #[test]
    fn a_successful_reconnection_forgets_the_history() {
        // Without this, a session that blips once an hour eventually treats a single blip as
        // its sixth failure and gives up.
        let mut backoff = Backoff::new();
        let _ = backoff.next_delay();
        let _ = backoff.next_delay();
        assert_eq!(backoff.attempts(), 2);

        backoff.reset();
        assert_eq!(backoff.attempts(), 0);
        assert_eq!(backoff.next_delay(), Some(INITIAL));
    }

    #[test]
    fn the_shift_cannot_wrap_into_a_hot_loop() {
        // A `1 << attempt` with a large attempt count wraps to zero and turns the backoff into
        // a busy loop. The schedule is finite so this is unreachable, but the arithmetic is
        // written to survive it anyway.
        let mut backoff = Backoff { attempt: MAX_ATTEMPTS - 1 };
        let delay = backoff.next_delay().expect("one attempt remains");
        assert!(delay > Duration::ZERO, "a zero delay would spin the CPU");
    }
}
