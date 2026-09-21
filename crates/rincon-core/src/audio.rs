//! Negotiated audio stream parameters.
//!
//! These types exist so that a sample rate can never be confused with a channel count, and so
//! that an impossible format (0 Hz, 0 channels) is unrepresentable rather than merely unlikely.
//! Every downstream calculation — WAV headers, ring capacity, byte rates — derives from
//! [`AudioFormat`], so getting the invariants right here removes a whole class of arithmetic
//! bugs everywhere else.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// Samples per second, guaranteed non-zero and within a plausible audio range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct SampleRate(u32);

impl SampleRate {
    /// The lowest rate any consumer-grade endpoint reports.
    pub const MIN: u32 = 8_000;
    /// Above this, something has misreported its format.
    pub const MAX: u32 = 768_000;

    /// CD rate; the most common Windows shared-mode default after 48 kHz.
    pub const CD: Self = Self(44_100);
    /// The Windows default for most endpoints.
    pub const STUDIO: Self = Self(48_000);

    /// Constructs a rate, rejecting values outside `MIN..=MAX`.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Invalid`] when the rate is implausible.
    pub fn new(hz: u32) -> Result<Self, CoreError> {
        if (Self::MIN..=Self::MAX).contains(&hz) {
            Ok(Self(hz))
        } else {
            Err(CoreError::invalid("sample_rate", format!("{hz} Hz is outside 8000..=768000")))
        }
    }

    /// The rate in hertz.
    #[must_use]
    pub const fn hz(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for SampleRate {
    type Error = CoreError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SampleRate> for u32 {
    fn from(value: SampleRate) -> Self {
        value.0
    }
}

impl fmt::Display for SampleRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz", self.0)
    }
}

/// Interleaved channel count, guaranteed in `1..=8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct ChannelCount(u16);

impl ChannelCount {
    /// Single channel.
    pub const MONO: Self = Self(1);
    /// Two interleaved channels, the format Sonos expects.
    pub const STEREO: Self = Self(2);
    /// Upper bound; beyond this the endpoint is not something we stream.
    pub const MAX: u16 = 8;

    /// Constructs a channel count, rejecting `0` and anything above [`Self::MAX`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Invalid`] when the count is out of range.
    pub fn new(count: u16) -> Result<Self, CoreError> {
        if (1..=Self::MAX).contains(&count) {
            Ok(Self(count))
        } else {
            Err(CoreError::invalid("channels", format!("{count} is outside 1..=8")))
        }
    }

    /// The channel count.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// The channel count widened for byte-rate arithmetic.
    ///
    /// `u32::from` is not callable in a `const fn`, and the value is bounded by
    /// [`Self::MAX`] = 8, so the widening cast cannot lose information.
    #[must_use]
    pub const fn get_u32(self) -> u32 {
        self.0 as u32
    }
}

impl TryFrom<u16> for ChannelCount {
    type Error = CoreError;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ChannelCount> for u16 {
    fn from(value: ChannelCount) -> Self {
        value.0
    }
}

/// How a single sample is laid out in memory or on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleEncoding {
    /// 32-bit float, the format WASAPI hands us in shared mode.
    F32Le,
    /// 16-bit signed little-endian, the format we put on the wire (L16 PCM).
    S16Le,
}

impl SampleEncoding {
    /// Bytes occupied by one sample of this encoding.
    #[must_use]
    pub const fn bytes_per_sample(self) -> u32 {
        match self {
            Self::F32Le => 4,
            Self::S16Le => 2,
        }
    }

    /// Bit depth as written into a WAV header.
    #[must_use]
    pub const fn bits_per_sample(self) -> u16 {
        match self {
            Self::F32Le => 32,
            Self::S16Le => 16,
        }
    }

    /// The WAVE format tag: 1 for integer PCM, 3 for IEEE float.
    #[must_use]
    pub const fn wave_format_tag(self) -> u16 {
        match self {
            Self::F32Le => 3,
            Self::S16Le => 1,
        }
    }
}

impl fmt::Display for SampleEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::F32Le => f.write_str("f32le"),
            Self::S16Le => f.write_str("s16le"),
        }
    }
}

/// A fully specified audio stream format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioFormat {
    /// Samples per second, per channel.
    pub sample_rate: SampleRate,
    /// Number of interleaved channels.
    pub channels: ChannelCount,
    /// Layout of a single sample.
    pub encoding: SampleEncoding,
}

impl AudioFormat {
    /// The format Rincon puts on the wire by default: CD-quality L16 stereo.
    pub const WIRE_DEFAULT: Self = Self {
        sample_rate: SampleRate::CD,
        channels: ChannelCount::STEREO,
        encoding: SampleEncoding::S16Le,
    };

    /// Builds a format from raw parts.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Invalid`] when the rate or channel count is out of range.
    pub fn new(rate_hz: u32, channels: u16, encoding: SampleEncoding) -> Result<Self, CoreError> {
        Ok(Self {
            sample_rate: SampleRate::new(rate_hz)?,
            channels: ChannelCount::new(channels)?,
            encoding,
        })
    }

    /// Bytes occupied by one frame (one sample for every channel).
    #[must_use]
    pub const fn bytes_per_frame(self) -> usize {
        self.bytes_per_frame_u32() as usize
    }

    /// [`Self::bytes_per_frame`] as a `u32`, for wire-format arithmetic.
    ///
    /// Bounded by `4 bytes x 8 channels = 32`, so no caller need worry about overflow.
    #[must_use]
    pub const fn bytes_per_frame_u32(self) -> u32 {
        self.encoding.bytes_per_sample() * self.channels.get_u32()
    }

    /// Bytes per second on the wire. Used for WAV headers and bandwidth budgeting.
    #[must_use]
    pub const fn byte_rate(self) -> u32 {
        self.sample_rate.hz() * self.bytes_per_frame_u32()
    }

    /// Frames in `millis` milliseconds, rounded up so a buffer is never a fraction short.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "bounded by MAX sample rate (768 kHz) times millis; callers pass session-scale values"
    )]
    pub const fn frames_in(self, millis: u32) -> usize {
        let numerator = self.sample_rate.hz() as u64 * millis as u64;
        // Ceiling division: a partial millisecond must still be a whole frame of headroom.
        numerator.div_ceil(1000) as usize
    }

    /// The same format re-expressed in the wire encoding, preserving rate and channels.
    #[must_use]
    pub const fn as_wire(self) -> Self {
        Self { encoding: SampleEncoding::S16Le, ..self }
    }
}

impl fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} / {}ch / {}", self.sample_rate, self.channels.get(), self.encoding)
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

    #[test]
    fn implausible_formats_cannot_be_constructed() {
        assert!(SampleRate::new(0).is_err());
        assert!(SampleRate::new(7_999).is_err());
        assert!(SampleRate::new(768_001).is_err());
        assert!(ChannelCount::new(0).is_err());
        assert!(ChannelCount::new(9).is_err());
        assert!(AudioFormat::new(48_000, 2, SampleEncoding::F32Le).is_ok());
    }

    #[test]
    fn byte_arithmetic_matches_the_wav_spec() {
        let f = AudioFormat::new(44_100, 2, SampleEncoding::S16Le).unwrap();
        assert_eq!(f.bytes_per_frame(), 4);
        assert_eq!(f.byte_rate(), 176_400, "CD stereo is 176.4 kB/s");

        let f32_stereo = AudioFormat::new(48_000, 2, SampleEncoding::F32Le).unwrap();
        assert_eq!(f32_stereo.bytes_per_frame(), 8);
        assert_eq!(f32_stereo.byte_rate(), 384_000);
    }

    #[test]
    fn frames_in_rounds_up_so_buffers_are_never_short() {
        let f = AudioFormat::new(44_100, 2, SampleEncoding::S16Le).unwrap();
        // 44_100 * 1 / 1000 = 44.1 frames -> must be 45, not 44.
        assert_eq!(f.frames_in(1), 45);
        assert_eq!(f.frames_in(1000), 44_100);
        assert_eq!(f.frames_in(0), 0);
    }

    #[test]
    fn as_wire_preserves_rate_and_channels() {
        let captured = AudioFormat::new(48_000, 2, SampleEncoding::F32Le).unwrap();
        let wire = captured.as_wire();
        assert_eq!(wire.sample_rate, captured.sample_rate);
        assert_eq!(wire.channels, captured.channels);
        assert_eq!(wire.encoding, SampleEncoding::S16Le);
    }

    #[test]
    fn wave_tags_distinguish_float_from_integer_pcm() {
        assert_eq!(SampleEncoding::S16Le.wave_format_tag(), 1);
        assert_eq!(SampleEncoding::F32Le.wave_format_tag(), 3);
        assert_eq!(SampleEncoding::S16Le.bits_per_sample(), 16);
        assert_eq!(SampleEncoding::F32Le.bits_per_sample(), 32);
    }

    #[test]
    fn formats_round_trip_through_serde() {
        let f = AudioFormat::new(48_000, 2, SampleEncoding::F32Le).unwrap();
        let json = serde_json::to_string(&f).unwrap();
        let back: AudioFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }

    #[test]
    fn serde_rejects_out_of_range_values_at_the_boundary() {
        // The invariant must survive deserialisation, not just the constructor.
        let bad = r#"{"sample_rate":0,"channels":2,"encoding":"f32_le"}"#;
        assert!(serde_json::from_str::<AudioFormat>(bad).is_err());
    }
}
