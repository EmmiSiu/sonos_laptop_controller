//! The 44-byte RIFF header that makes an endless PCM stream look like a file.
//!
//! # The endless-file trick, and its real limit
//!
//! A Sonos player fetches a URL and expects something it can decode. The most widely accepted
//! thing is a WAV file — so Rincon sends a WAV header and then never stops sending samples.
//!
//! The `data` chunk size is a **32-bit** field. It cannot express an unbounded stream, and it
//! cannot even express a full day: at 44.1 kHz stereo 16-bit, 24 hours is 15.2 GB, while the
//! largest value the field can hold is 4.29 GB — about **6 h 45 m** of audio.
//!
//! So the header declares the maximum the format allows and the server keeps serving past it.
//! Every WAV-over-HTTP streamer works this way, because players stream until the socket closes
//! rather than counting bytes against the header. If a firmware is ever found that does enforce
//! the declared length, [`crate::config::Framing::Chunked`] exists for exactly that case.
//!
//! This limitation is stated here rather than hidden because a six-hour session that stops
//! without explanation would be a genuinely mysterious bug, and the next person to hit it
//! should find the answer in the module that causes it.

use rincon_core::audio::AudioFormat;

/// Bytes in a canonical PCM WAV header.
pub const HEADER_LEN: usize = 44;

/// The largest `data` chunk a 32-bit RIFF field can describe, leaving room for the header.
///
/// Using `u32::MAX` itself would make the RIFF chunk size overflow; subtracting the header
/// keeps both fields expressible.
pub const MAX_DATA_BYTES: u32 = u32::MAX - HEADER_LEN as u32;

/// Builds the header for `format`, declaring `data_bytes` of audio.
///
/// Covers: RQ-STRM-001
#[must_use]
pub fn header(format: AudioFormat, data_bytes: u32) -> [u8; HEADER_LEN] {
    let mut out = [0_u8; HEADER_LEN];
    let mut cursor = 0;

    let mut put = |bytes: &[u8]| {
        if let Some(slot) = out.get_mut(cursor..cursor + bytes.len()) {
            slot.copy_from_slice(bytes);
        }
        cursor += bytes.len();
    };

    let channels = format.channels.get();
    let block_align = u16::try_from(format.bytes_per_frame()).unwrap_or(4);

    put(b"RIFF");
    // The RIFF chunk covers everything after this field: 36 bytes of header plus the data.
    put(&data_bytes.saturating_add(36).to_le_bytes());
    put(b"WAVE");

    put(b"fmt ");
    put(&16_u32.to_le_bytes()); // PCM `fmt ` chunks are always 16 bytes
    put(&format.encoding.wave_format_tag().to_le_bytes());
    put(&channels.to_le_bytes());
    put(&format.sample_rate.hz().to_le_bytes());
    put(&format.byte_rate().to_le_bytes());
    put(&block_align.to_le_bytes());
    put(&format.encoding.bits_per_sample().to_le_bytes());

    put(b"data");
    put(&data_bytes.to_le_bytes());

    out
}

/// Builds the header for an endless stream.
///
/// Covers: RQ-STRM-001, RQ-STRM-002
#[must_use]
pub fn endless_header(format: AudioFormat) -> [u8; HEADER_LEN] {
    header(format, MAX_DATA_BYTES)
}

/// How long the declared `data` chunk lasts at `format`, in seconds.
///
/// Exposed so the limit is a number the tests and the documentation share rather than two
/// numbers that can drift apart.
#[must_use]
pub fn declared_duration_secs(format: AudioFormat) -> u64 {
    u64::from(MAX_DATA_BYTES) / u64::from(format.byte_rate())
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

    fn cd_stereo() -> AudioFormat {
        AudioFormat::new(44_100, 2, SampleEncoding::S16Le).unwrap()
    }

    /// The exact 44 bytes of a 44.1 kHz stereo 16-bit header declaring 1000 bytes of audio.
    ///
    /// Committed as a golden fixture: a header is the one thing in this project where a single
    /// wrong byte produces silence with no error anywhere, so it is compared literally rather
    /// than field by field.
    const GOLDEN_CD_1000: [u8; HEADER_LEN] = [
        b'R', b'I', b'F', b'F', // RIFF
        0xE4, 0x04, 0x00, 0x00, // chunk size = 1000 + 36 = 1036
        b'W', b'A', b'V', b'E', // WAVE
        b'f', b'm', b't', b' ', // fmt
        0x10, 0x00, 0x00, 0x00, // subchunk size = 16
        0x01, 0x00, // format tag = 1 (integer PCM)
        0x02, 0x00, // channels = 2
        0x44, 0xAC, 0x00, 0x00, // sample rate = 44100
        0x10, 0xB1, 0x02, 0x00, // byte rate = 176400
        0x04, 0x00, // block align = 4
        0x10, 0x00, // bits per sample = 16
        b'd', b'a', b't', b'a', // data
        0xE8, 0x03, 0x00, 0x00, // data size = 1000
    ];

    /// Covers: RQ-STRM-001
    #[test]
    fn rq_strm_001_header_matches_golden() {
        assert_eq!(header(cd_stereo(), 1000), GOLDEN_CD_1000);
    }

    /// Covers: RQ-STRM-002
    #[test]
    fn rq_strm_002_data_chunk_is_the_largest_the_format_allows() {
        // The honest statement of the limit. A 32-bit `data` field cannot describe 24 hours of
        // CD-quality stereo -- no WAV header can -- so the requirement is that we declare the
        // maximum expressible value and keep serving past it (RQ-STRM-015).
        let head = endless_header(cd_stereo());
        let declared = u32::from_le_bytes([head[40], head[41], head[42], head[43]]);
        assert_eq!(declared, MAX_DATA_BYTES);
        assert_eq!(declared, u32::MAX - 44);

        // The RIFF size must not wrap when the data size is at its maximum.
        let riff = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        assert!(riff >= declared, "RIFF chunk size wrapped: {riff} < {declared}");

        // And the number the docs quote must be the number the code produces.
        let hours = declared_duration_secs(cd_stereo()) / 3600;
        assert_eq!(hours, 6, "the declared chunk lasts {hours} h, not the documented 6 h 45 m");
    }

    #[test]
    fn the_header_is_correct_for_the_other_formats_we_negotiate() {
        // 48 kHz stereo, the Windows default.
        let studio = AudioFormat::new(48_000, 2, SampleEncoding::S16Le).unwrap();
        let head = header(studio, 0);
        assert_eq!(u32::from_le_bytes([head[24], head[25], head[26], head[27]]), 48_000);
        assert_eq!(u32::from_le_bytes([head[28], head[29], head[30], head[31]]), 192_000);
        assert_eq!(u16::from_le_bytes([head[32], head[33]]), 4, "block align");

        // Mono, in case an endpoint reports it.
        let mono = AudioFormat::new(44_100, 1, SampleEncoding::S16Le).unwrap();
        let head = header(mono, 0);
        assert_eq!(u16::from_le_bytes([head[22], head[23]]), 1, "channels");
        assert_eq!(u16::from_le_bytes([head[32], head[33]]), 2, "block align");
    }

    #[test]
    fn a_float_format_declares_the_ieee_tag_not_the_pcm_one() {
        // Sending f32 samples under format tag 1 is a classic way to produce loud noise.
        let float = AudioFormat::new(48_000, 2, SampleEncoding::F32Le).unwrap();
        let head = header(float, 0);
        assert_eq!(u16::from_le_bytes([head[20], head[21]]), 3, "IEEE float tag");
        assert_eq!(u16::from_le_bytes([head[34], head[35]]), 32, "bits per sample");
    }

    #[test]
    fn every_header_is_exactly_forty_four_bytes() {
        for encoding in [SampleEncoding::S16Le, SampleEncoding::F32Le] {
            for rate in [8_000, 44_100, 48_000, 96_000] {
                for channels in 1..=8 {
                    let format = AudioFormat::new(rate, channels, encoding).unwrap();
                    assert_eq!(header(format, 0).len(), HEADER_LEN);
                    assert_eq!(endless_header(format).len(), HEADER_LEN);
                }
            }
        }
    }

    #[test]
    fn the_header_is_self_consistent_with_the_format_it_describes() {
        let format = cd_stereo();
        let head = header(format, 12_345);
        let channels = u16::from_le_bytes([head[22], head[23]]);
        let rate = u32::from_le_bytes([head[24], head[25], head[26], head[27]]);
        let byte_rate = u32::from_le_bytes([head[28], head[29], head[30], head[31]]);
        let block_align = u16::from_le_bytes([head[32], head[33]]);
        let bits = u16::from_le_bytes([head[34], head[35]]);

        // The three redundant fields WAV carries must agree, or decoders disagree about tempo.
        assert_eq!(u32::from(block_align), u32::from(channels) * u32::from(bits) / 8);
        assert_eq!(byte_rate, rate * u32::from(block_align));
    }
}
