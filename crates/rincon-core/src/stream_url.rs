//! The URL handed to a speaker, with the session token kept out of logs.
//!
//! This type lives in `rincon-core` rather than in `rincon-stream` because two crates need it
//! and neither should depend on the other: `rincon-stream` produces it, `rincon-control` sends
//! it. Shared vocabulary belongs in the shared crate.
//!
//! Its `Debug` implementation redacts the token. That is not decoration: `tracing` renders
//! `Debug` for structured fields, so a single `debug!(?url)` anywhere in the workspace would
//! otherwise write the session secret to a file the user may attach to a public issue.

use std::fmt;

use serde::{Serialize, Serializer};

use crate::diagnostics::REDACTED;

/// A `http://<addr>:<port>/s/<token>/stream.wav` URL.
///
/// Construct through [`StreamUrl::new`], which records where the token sits so that redaction
/// is a property of the value rather than something each log site must remember.
///
/// # Serialisation
///
/// Deliberately **not** `Deserialize`, and `Serialize` emits the *redacted* string.
///
/// The derived implementation would have emitted the token. That matters more than it looks:
/// the session state is serialised across the IPC boundary into a WebView, so a derived
/// `Serialize` would hand the stream secret to a renderer that also displays device names
/// supplied by unauthenticated devices on the LAN. Nothing needs to deserialise a stream URL —
/// it is always constructed from a server that has actually bound — so the trait is simply
/// absent rather than implemented incorrectly.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct StreamUrl {
    full: String,
    /// Byte range of the token inside `full`.
    token_span: (usize, usize),
}

impl StreamUrl {
    /// Builds the URL for a bound server.
    #[must_use]
    pub fn new(host: std::net::IpAddr, port: u16, token: &str) -> Self {
        // An IPv6 literal needs brackets in an authority component.
        let authority = match host {
            std::net::IpAddr::V4(v4) => format!("{v4}:{port}"),
            std::net::IpAddr::V6(v6) => format!("[{v6}]:{port}"),
        };
        let prefix = format!("http://{authority}/s/");
        let start = prefix.len();
        let full = format!("{prefix}{token}/stream.wav");
        Self { full, token_span: (start, start + token.len()) }
    }

    /// The complete URL, token included. Use this only where the token must actually travel.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.full
    }

    /// The URL with the token replaced, safe for logs, diagnostics, and the interface.
    #[must_use]
    pub fn redacted(&self) -> String {
        let (start, end) = self.token_span;
        match (self.full.get(..start), self.full.get(end..)) {
            (Some(head), Some(tail)) => format!("{head}{REDACTED}{tail}"),
            // Unreachable while the span comes from the constructor, but a wrong span must
            // fail closed -- towards redaction -- not open.
            _ => format!("http://{REDACTED}/stream.wav"),
        }
    }

    /// The token itself, for the server's constant-time comparison.
    #[must_use]
    pub fn token(&self) -> &str {
        let (start, end) = self.token_span;
        self.full.get(start..end).unwrap_or_default()
    }
}

/// Serialises to the redacted form, so the token cannot cross the IPC boundary.
///
/// Covers: RQ-SEC-005, RQ-UI-003
impl Serialize for StreamUrl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.redacted())
    }
}

/// Redacts, so an accidental `{:?}` in a `tracing` field cannot leak the session secret.
impl fmt::Debug for StreamUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted())
    }
}

/// Also redacts: `Display` reaches the interface, which is exactly where the token must not be.
///
/// Both formatting traits deliberately produce the identical redacted string. A type where one
/// is safe and the other is not is a trap for the next person who logs it.
impl fmt::Display for StreamUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted())
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

    const TOKEN: &str = "9f2c1a7b4e8d3f60a1b2c3d4e5f60718";

    fn url() -> StreamUrl {
        StreamUrl::new("192.168.1.20".parse().unwrap(), 41_234, TOKEN)
    }

    #[test]
    fn the_url_is_what_a_speaker_will_fetch() {
        assert_eq!(url().as_str(), format!("http://192.168.1.20:41234/s/{TOKEN}/stream.wav"));
        assert_eq!(url().token(), TOKEN);
    }

    /// Covers: RQ-SEC-005, RQ-OBS-004
    #[test]
    fn rq_sec_005_token_is_redacted_in_debug() {
        let url = url();
        // The three ways a value reaches a log or a screen.
        let debug = format!("{url:?}");
        let display = format!("{url}");
        let structured = format!("{:?}", vec![&url]);

        for rendered in [&debug, &display, &structured] {
            assert!(!rendered.contains(TOKEN), "the token leaked: {rendered}");
            assert!(rendered.contains(REDACTED), "no redaction marker in: {rendered}");
        }
        // Only the explicit accessor exposes it.
        assert!(url.as_str().contains(TOKEN));
    }

    #[test]
    fn ipv6_literals_are_bracketed_so_the_authority_parses() {
        let v6 = StreamUrl::new("fe80::1".parse().unwrap(), 8080, TOKEN);
        assert!(v6.as_str().starts_with("http://[fe80::1]:8080/s/"), "got {}", v6.as_str());
        assert_eq!(v6.token(), TOKEN);
    }

    /// Stands in for the session state, which carries a `StreamUrl` as a field.
    #[derive(serde::Serialize)]
    struct Session {
        url: StreamUrl,
    }

    /// Covers: RQ-SEC-005
    #[test]
    fn rq_sec_005_the_token_cannot_cross_the_ipc_boundary() {
        // The session state is serialised into a WebView. A derived `Serialize` here would
        // have shipped the stream secret to a renderer that also displays strings supplied by
        // unauthenticated devices on the LAN.
        let json = serde_json::to_string(&url()).expect("a stream URL must serialise");
        assert!(!json.contains(TOKEN), "the token reached the wire: {json}");
        assert!(json.contains(REDACTED), "no redaction marker in: {json}");
        assert!(json.contains("192.168.1.20:41234"), "the address is still useful: {json}");

        // Nested inside a structure, as the session state carries it.
        let nested = serde_json::to_string(&Session { url: url() }).expect("must serialise");
        assert!(!nested.contains(TOKEN), "the token leaked when nested: {nested}");
    }

    #[test]
    fn the_redacted_form_keeps_everything_useful_for_debugging() {
        let redacted = url().redacted();
        assert!(redacted.contains("192.168.1.20:41234"), "the address must survive");
        assert!(redacted.ends_with("/stream.wav"), "the path shape must survive");
    }
}
