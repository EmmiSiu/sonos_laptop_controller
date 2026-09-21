//! Sample-format conversion, with the edge cases that produce audible artefacts spelled out.
//!
//! WASAPI hands us `f32` in shared mode; Sonos wants L16 PCM on the wire. That conversion looks
//! like one multiply, and three things make it not one multiply:
//!
//! - **Clipping.** A float sample may exceed `±1.0` — a loudness-war master or a plugin with
//!   gain will do it routinely. Multiplying and casting wraps, turning a loud peak into a
//!   full-scale sample of the *opposite* sign. That is the loudest, ugliest possible artefact.
//! - **Asymmetry.** Two's complement has one more negative value than positive. A single scale
//!   factor either cannot reach `-32768` or overflows at `+1.0`.
//! - **Non-finite input.** NaN from an upstream bug must become silence, not a random 16-bit
//!   pattern that arrives as a click.

/// Positive full scale for signed 16-bit audio.
const POSITIVE_SCALE: f32 = 32_767.0;

/// Negative full scale, one greater in magnitude than the positive side.
const NEGATIVE_SCALE: f32 = 32_768.0;

/// Converts one `f32` sample to `i16`, clamping and handling non-finite input.
///
/// The two scale factors are deliberate: `+1.0` maps to `i16::MAX` and `-1.0` to `i16::MIN`,
/// so both rails are reachable and neither overflows.
///
/// Covers: RQ-AUD-004, RQ-AUD-005, RQ-AUD-006
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the input is clamped to +/-1.0 and scaled below i16::MAX; float-to-int `as` also \
              saturates rather than wrapping, so this cannot produce a wrong sign"
)]
pub fn f32_to_i16(sample: f32) -> i16 {
    if !sample.is_finite() {
        return 0;
    }
    let clamped = sample.clamp(-1.0, 1.0);
    let scaled = if clamped >= 0.0 { clamped * POSITIVE_SCALE } else { clamped * NEGATIVE_SCALE };
    scaled.round() as i16
}

/// Converts a slice of `f32` samples into little-endian `i16` bytes appended to `out`.
///
/// Writing directly into a byte buffer skips an intermediate `Vec<i16>`, which matters because
/// this runs once per socket write for the life of the session.
pub fn f32_to_i16_le_bytes(samples: &[f32], out: &mut Vec<u8>) {
    out.reserve(samples.len() * 2);
    for &sample in samples {
        out.extend_from_slice(&f32_to_i16(sample).to_le_bytes());
    }
}

/// Peak absolute amplitude of a buffer, in `0.0..=1.0`, for the level meter.
///
/// Non-finite samples are ignored rather than poisoning the peak with NaN.
#[must_use]
pub fn peak_amplitude(samples: &[f32]) -> f32 {
    samples
        .iter()
        .copied()
        .filter(|s| s.is_finite())
        .fold(0.0_f32, |peak, s| peak.max(s.abs()))
        .min(1.0)
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

    /// Covers: RQ-AUD-005
    #[test]
    fn rq_aud_005_endpoints_are_exact() {
        assert_eq!(f32_to_i16(1.0), i16::MAX, "+1.0 must reach positive full scale");
        assert_eq!(f32_to_i16(-1.0), i16::MIN, "-1.0 must reach negative full scale");
        assert_eq!(f32_to_i16(0.0), 0, "silence must stay silent");
        assert_eq!(f32_to_i16(-0.0), 0, "negative zero is still silence");

        // Half scale, both signs, to catch a one-off in the scale factors.
        assert_eq!(f32_to_i16(0.5), 16_384);
        assert_eq!(f32_to_i16(-0.5), -16_384);
    }

    /// Covers: RQ-AUD-004
    #[test]
    fn rq_aud_004_clamps_never_wraps() {
        // The failure this guards against: a wrap turns a loud positive peak into a loud
        // *negative* sample, which is the worst-sounding artefact a converter can produce.
        for hot in [1.0001_f32, 1.5, 2.0, 10.0, 1e30, f32::MAX] {
            assert_eq!(f32_to_i16(hot), i16::MAX, "{hot} must clamp to positive full scale");
        }
        for cold in [-1.0001_f32, -1.5, -2.0, -10.0, -1e30, f32::MIN] {
            assert_eq!(f32_to_i16(cold), i16::MIN, "{cold} must clamp to negative full scale");
        }
    }

    /// Covers: RQ-AUD-006
    #[test]
    fn rq_aud_006_non_finite_becomes_silence() {
        assert_eq!(f32_to_i16(f32::NAN), 0);
        assert_eq!(f32_to_i16(-f32::NAN), 0);
        assert_eq!(f32_to_i16(f32::INFINITY), 0);
        assert_eq!(f32_to_i16(f32::NEG_INFINITY), 0);
    }

    #[test]
    fn little_endian_byte_order_matches_the_wire_format() {
        let mut out = Vec::new();
        f32_to_i16_le_bytes(&[1.0, -1.0, 0.0], &mut out);
        assert_eq!(out, vec![0xFF, 0x7F, 0x00, 0x80, 0x00, 0x00]);
    }

    #[test]
    fn byte_conversion_produces_two_bytes_per_sample() {
        let samples = vec![0.1_f32; 1000];
        let mut out = Vec::new();
        f32_to_i16_le_bytes(&samples, &mut out);
        assert_eq!(out.len(), 2000);
    }

    #[test]
    fn peak_amplitude_ignores_garbage_and_saturates() {
        assert_eq!(peak_amplitude(&[]), 0.0);
        assert_eq!(peak_amplitude(&[0.2, -0.7, 0.3]), 0.7);
        assert_eq!(peak_amplitude(&[f32::NAN, 0.4]), 0.4, "NaN must not poison the meter");
        assert_eq!(peak_amplitude(&[5.0]), 1.0, "the meter cannot read above full scale");
    }

    proptest::proptest! {
        /// No input, however hostile, produces a sample of the wrong sign or an out-of-range
        /// value. This is the property that "clamps, never wraps" actually means.
        #[test]
        fn conversion_is_total_and_sign_preserving(raw in proptest::num::f32::ANY) {
            let converted = f32_to_i16(raw);

            if raw.is_nan() || raw.is_infinite() {
                proptest::prop_assert_eq!(converted, 0);
            } else if raw > 0.0 {
                proptest::prop_assert!(converted >= 0, "{} became {}", raw, converted);
            } else if raw < 0.0 {
                proptest::prop_assert!(converted <= 0, "{} became {}", raw, converted);
            }
        }

        /// Conversion is monotonic: a louder input never produces a quieter sample.
        #[test]
        fn conversion_is_monotonic(a in -2.0_f32..2.0, b in -2.0_f32..2.0) {
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            proptest::prop_assert!(f32_to_i16(lo) <= f32_to_i16(hi));
        }

        /// Round-tripping back to float stays within one quantisation step.
        ///
        /// The inverse has to use the same asymmetric scales as the forward conversion: a
        /// single divisor is off by one LSB near full scale, which is exactly the kind of
        /// silent half-bit error this property exists to catch.
        #[test]
        fn round_trip_error_is_below_one_lsb(raw in -1.0_f32..=1.0) {
            let converted = f32_to_i16(raw);
            let scale = if converted >= 0 { POSITIVE_SCALE } else { NEGATIVE_SCALE };
            let back = f32::from(converted) / scale;
            proptest::prop_assert!(
                (back - raw).abs() <= 1.0 / POSITIVE_SCALE,
                "{} round-tripped to {}", raw, back
            );
        }
    }
}
