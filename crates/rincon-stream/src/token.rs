//! The per-session token that makes the stream URL unguessable.
//!
//! # What this defends against, and what it does not
//!
//! The stream carries the user's system audio, and it is served from an open port on a home
//! network. The token is one of four independent layers (SPEC-002 §9) and answers exactly one
//! question: *can a LAN peer who has not seen the URL find it?*
//!
//! 128 bits of entropy means no. It does **not** hide the URL from someone who can read the
//! SOAP request on the wire — that is the attacker the peer allowlist and SPEC-007's honest
//! statement about Wi-Fi confidentiality are for.
//!
//! # Why comparison is constant-time
//!
//! A byte-by-byte `==` returns sooner for a wrong first byte than for a wrong last byte. Over
//! enough requests that difference is measurable on a LAN, and it turns a 128-bit guess into
//! 32 sequential 16-way guesses. `subtle::ConstantTimeEq` removes the signal.

use std::fmt;

use subtle::ConstantTimeEq;

/// Bytes of entropy in a token. 16 bytes = 128 bits, rendered as 32 hex characters.
pub const TOKEN_BYTES: usize = 16;

/// Characters in the rendered token.
pub const TOKEN_LEN: usize = TOKEN_BYTES * 2;

/// A session secret that never prints itself.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionToken {
    rendered: String,
}

impl SessionToken {
    /// Generates a fresh token from the OS entropy source.
    ///
    /// # Errors
    ///
    /// Returns [`getrandom::Error`] when the OS cannot provide entropy. There is no fallback
    /// to a weak source: a predictable token is worse than a failed session, because the
    /// failure is visible and the weakness is not.
    ///
    /// Covers: RQ-STRM-003
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0_u8; TOKEN_BYTES];
        getrandom::getrandom(&mut bytes)?;
        Ok(Self { rendered: hex(&bytes) })
    }

    /// Builds a token from a known string. Test-only in practice, but not gated, because the
    /// engine reconstructs a token when it re-reads a session URL.
    ///
    /// # Errors
    ///
    /// Returns `None` when `raw` is not exactly [`TOKEN_LEN`] lowercase hex characters.
    #[must_use]
    pub fn from_hex(raw: &str) -> Option<Self> {
        let valid = raw.len() == TOKEN_LEN
            && raw.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        valid.then(|| Self { rendered: raw.to_owned() })
    }

    /// The token as it appears in the URL.
    ///
    /// Named to be conspicuous at call sites: anything that exposes a secret should look
    /// deliberate in a diff.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.rendered
    }

    /// Constant-time equality against a candidate from a request path.
    ///
    /// Covers: RQ-STRM-005, RQ-SEC-005
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        // Comparing different lengths can never be constant-time, and the length is not the
        // secret, so it is checked first and openly.
        if candidate.len() != self.rendered.len() {
            return false;
        }
        self.rendered.as_bytes().ct_eq(candidate.as_bytes()).into()
    }
}

/// Lowercase hex, written out rather than pulled from a crate: sixteen bytes do not justify a
/// dependency, and the implementation is one line that a reviewer can check by eye.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Redacts. A token in a log file is a token in whatever the user pastes into an issue.
impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(<redacted>)")
    }
}

/// Redacts, for the same reason.
impl fmt::Display for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
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

    use std::collections::HashSet;

    use super::*;

    /// Covers: RQ-STRM-003
    #[test]
    fn rq_strm_003_token_has_128_bits_of_entropy() {
        let token = SessionToken::generate().unwrap();
        assert_eq!(token.expose().len(), TOKEN_LEN, "32 hex characters = 128 bits");
        assert_eq!(TOKEN_BYTES * 8, 128);
        assert!(token.expose().bytes().all(|b| b.is_ascii_hexdigit()));

        // Distinctness across a large sample. A generator with a fixed seed, a per-process
        // counter, or a truncated source would collide here; true 128-bit randomness will not
        // collide in 10,000 draws before the heat death of the sun.
        let mut seen = HashSet::new();
        for _ in 0..10_000 {
            assert!(
                seen.insert(SessionToken::generate().unwrap().expose().to_owned()),
                "two sessions generated the same token"
            );
        }

        // And the bits are spread: every hex digit should appear across the sample. A broken
        // source that only ever emitted, say, ASCII digits would pass the checks above.
        let corpus: String = (0..200)
            .map(|_| SessionToken::generate().unwrap().expose().to_owned())
            .collect();
        for digit in "0123456789abcdef".chars() {
            assert!(corpus.contains(digit), "hex digit `{digit}` never appeared");
        }
    }

    /// Covers: RQ-STRM-005
    #[test]
    fn rq_strm_005_comparison_is_constant_time() {
        let token = SessionToken::from_hex("9f2c1a7b4e8d3f60a1b2c3d4e5f60718").unwrap();

        assert!(token.matches("9f2c1a7b4e8d3f60a1b2c3d4e5f60718"));
        assert!(!token.matches("0f2c1a7b4e8d3f60a1b2c3d4e5f60718"), "wrong first character");
        assert!(!token.matches("9f2c1a7b4e8d3f60a1b2c3d4e5f60719"), "wrong last character");
        assert!(!token.matches(""), "empty candidate");
        assert!(!token.matches("9f2c1a7b"), "truncated candidate");
        assert!(!token.matches(&format!("{}extra", token.expose())), "extended candidate");

        // The structural guarantee: comparison of equal-length inputs goes through
        // `subtle::ConstantTimeEq`, which has no early return. A timing measurement in a unit
        // test would be flaky on a shared CI runner, so the property is enforced by using the
        // primitive rather than by timing it -- and this test pins the behaviour that
        // primitive gives us, including the length pre-check that is deliberately not secret.
        let shorter = SessionToken::from_hex("9f2c1a7b4e8d3f60a1b2c3d4e5f60718").unwrap();
        assert!(shorter.matches(token.expose()));
    }

    /// Covers: RQ-SEC-005, RQ-OBS-004
    #[test]
    fn rq_sec_005_token_is_redacted_in_debug() {
        let token = SessionToken::from_hex("9f2c1a7b4e8d3f60a1b2c3d4e5f60718").unwrap();
        let secret = token.expose().to_owned();

        for rendered in [format!("{token:?}"), format!("{token}"), format!("{:?}", Some(&token))] {
            assert!(!rendered.contains(&secret), "the token leaked: {rendered}");
            assert!(rendered.contains("redacted"), "no redaction marker in: {rendered}");
        }
        // Only the conspicuously named accessor exposes it.
        assert_eq!(token.expose(), secret);
    }

    #[test]
    fn malformed_tokens_are_refused_at_construction() {
        for bad in [
            "",
            "short",
            "9f2c1a7b4e8d3f60a1b2c3d4e5f6071",    // 31 characters
            "9f2c1a7b4e8d3f60a1b2c3d4e5f607188",  // 33 characters
            "9F2C1A7B4E8D3F60A1B2C3D4E5F60718",   // uppercase
            "9f2c1a7b4e8d3f60a1b2c3d4e5f6071g",   // non-hex
            "../../etc/passwd................",   // traversal attempt of the right length
        ] {
            assert!(SessionToken::from_hex(bad).is_none(), "`{bad}` must be refused");
        }
    }

    #[test]
    fn hex_rendering_is_correct_and_lowercase() {
        assert_eq!(hex(&[0x00, 0x0f, 0xf0, 0xff]), "000ff0ff");
        assert_eq!(hex(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(hex(&[]), "");
    }
}
