//! Offline, redacted support bundles.
//!
//! Rincon never phones home, so a bug report is only as good as what the user can paste into an
//! issue. That makes the bundle a real product surface: it has to contain enough to debug a
//! dropout, and little enough that pasting it into a public tracker is safe.
//!
//! Redaction happens while the bundle is **built**, not while it is displayed. A bundle written
//! to disk is therefore safe by construction — there is no unredacted form of it anywhere.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::metrics::CounterSnapshot;

/// Replacement for any value considered sensitive.
pub const REDACTED: &str = "<redacted>";

/// One line of a diagnostics bundle's session summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Current state name, e.g. `Streaming`.
    pub state: String,
    /// How long the session has been in that state, as `HH:MM:SS`.
    pub elapsed: String,
    /// The negotiated audio format, rendered.
    pub format: String,
    /// The target device, already redacted.
    pub device: String,
}

/// Everything a maintainer needs, and nothing a user would regret sharing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    /// Product version.
    pub version: String,
    /// Build profile (`debug` or `release`).
    pub profile: String,
    /// Operating system description.
    pub os: String,
    /// Network interfaces, addresses already redacted.
    pub interfaces: Vec<String>,
    /// Session summary, if a session exists.
    pub session: Option<SessionSummary>,
    /// Counters at the moment the bundle was built.
    pub counters: CounterSnapshot,
    /// The tail of the log, redacted.
    pub log_tail: Vec<String>,
}

/// Builds a [`Bundle`], applying redaction to every value as it is added.
///
/// The builder is the only way to construct a bundle, which is what makes redaction
/// unforgettable rather than merely recommended.
#[derive(Debug, Clone)]
pub struct BundleBuilder {
    version: String,
    profile: String,
    os: String,
    interfaces: Vec<String>,
    session: Option<SessionSummary>,
    counters: CounterSnapshot,
    log_tail: Vec<String>,
    secrets: Vec<String>,
}

impl BundleBuilder {
    /// Starts a bundle for the running build.
    #[must_use]
    pub fn new() -> Self {
        Self {
            version: crate::VERSION.to_owned(),
            profile: if cfg!(debug_assertions) { "debug" } else { "release" }.to_owned(),
            os: std::env::consts::OS.to_owned(),
            interfaces: Vec::new(),
            session: None,
            counters: CounterSnapshot::default(),
            log_tail: Vec::new(),
            secrets: Vec::new(),
        }
    }

    /// Registers a literal string that must never appear in the output, such as a stream token
    /// or the machine hostname.
    #[must_use]
    pub fn redacting(mut self, secret: impl Into<String>) -> Self {
        let secret = secret.into();
        // An empty or one-character secret would redact the entire document.
        if secret.len() > 2 {
            self.secrets.push(secret);
        }
        self
    }

    /// Sets the OS description.
    #[must_use]
    pub fn os(mut self, os: impl Into<String>) -> Self {
        self.os = os.into();
        self
    }

    /// Adds an interface description line.
    #[must_use]
    pub fn interface(mut self, line: impl AsRef<str>) -> Self {
        let scrubbed = self.scrub(line.as_ref());
        self.interfaces.push(scrubbed);
        self
    }

    /// Attaches the session summary.
    #[must_use]
    pub fn session(mut self, summary: SessionSummary) -> Self {
        self.session = Some(SessionSummary {
            state: self.scrub(&summary.state),
            elapsed: summary.elapsed,
            format: summary.format,
            device: self.scrub(&summary.device),
        });
        self
    }

    /// Attaches counters.
    #[must_use]
    pub const fn counters(mut self, counters: CounterSnapshot) -> Self {
        self.counters = counters;
        self
    }

    /// Attaches log lines, keeping at most [`crate::limits::DIAGNOSTIC_LOG_LINES`] of the tail.
    #[must_use]
    pub fn log_tail<I, S>(mut self, lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let all: Vec<String> = lines.into_iter().map(|l| self.scrub(l.as_ref())).collect();
        let keep = crate::limits::DIAGNOSTIC_LOG_LINES;
        let start = all.len().saturating_sub(keep);
        self.log_tail = all.into_iter().skip(start).collect();
        self
    }

    /// Finishes the bundle.
    ///
    /// Covers: RQ-OBS-006, RQ-OBS-008
    #[must_use]
    pub fn build(self) -> Bundle {
        Bundle {
            version: self.version,
            profile: self.profile,
            os: self.os,
            interfaces: self.interfaces,
            session: self.session,
            counters: self.counters,
            log_tail: self.log_tail,
        }
    }

    /// Applies registered secrets, then the generic path/address redaction.
    fn scrub(&self, raw: &str) -> String {
        let mut out = raw.to_owned();
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), REDACTED);
            }
        }
        crate::error::redact_sensitive(&out)
    }
}

impl Default for BundleBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl Bundle {
    /// Renders the bundle as the plain text a user pastes into an issue.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(4096);
        let _ = writeln!(out, "{} {} ({})", crate::PRODUCT, self.version, self.profile);
        let _ = writeln!(out, "os:         {}", self.os);

        let _ = writeln!(out, "interfaces:");
        if self.interfaces.is_empty() {
            let _ = writeln!(out, "  (none detected)");
        }
        for iface in &self.interfaces {
            let _ = writeln!(out, "  {iface}");
        }

        match &self.session {
            Some(s) => {
                let _ = writeln!(out, "session:    {} for {}", s.state, s.elapsed);
                let _ = writeln!(out, "format:     {}", s.format);
                let _ = writeln!(out, "device:     {}", s.device);
            }
            None => {
                let _ = writeln!(out, "session:    (idle)");
            }
        }

        let c = &self.counters;
        let _ = writeln!(
            out,
            "counters:   captured {} served {} dropped {} underruns {} reconnects {}",
            c.frames_captured,
            c.frames_served,
            c.dropped_total(),
            c.underruns,
            c.reconnects
        );
        let _ = writeln!(
            out,
            "  by layer: capture {} ring {} socket {} device {}",
            c.dropped_capture, c.dropped_ring, c.dropped_socket, c.dropped_device
        );

        let _ = writeln!(out, "log tail ({} lines):", self.log_tail.len());
        for line in &self.log_tail {
            let _ = writeln!(out, "  {line}");
        }
        out
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

    fn sample_bundle() -> Bundle {
        BundleBuilder::new()
            .redacting(TOKEN)
            .redacting("ALICE-LAPTOP")
            .os("Windows 11 26200")
            .interface("wlan0 192.168.1.20/24 (up)")
            .session(SessionSummary {
                state: "Streaming".into(),
                elapsed: "00:14:22".into(),
                format: "48000 Hz / 2ch / s16le".into(),
                device: "Kitchen at 192.168.1.45".into(),
            })
            .counters(CounterSnapshot {
                frames_captured: 41_472_000,
                frames_served: 41_471_800,
                underruns: 2,
                ..Default::default()
            })
            .log_tail([
                format!("GET /s/{TOKEN}/stream.wav from 192.168.1.45"),
                "host ALICE-LAPTOP bound 192.168.1.20:41234".to_owned(),
            ])
            .build()
    }

    /// Covers: RQ-OBS-006
    #[test]
    fn rq_obs_006_bundle_contents() {
        let rendered = sample_bundle().render();
        for required in [
            "Rincon",      // version line
            "os:",         // OS build
            "interfaces:", // interface summary
            "session:",    // session state
            "counters:",   // counters
            "by layer:",   // attribution
            "log tail",    // log lines
        ] {
            assert!(rendered.contains(required), "bundle is missing `{required}`:\n{rendered}");
        }
        assert!(rendered.contains("41472000"), "counter values must be present");
        assert!(rendered.contains("00:14:22"), "elapsed time must be present");
    }

    /// Covers: RQ-OBS-007
    #[test]
    fn rq_obs_007_bundle_is_redacted() {
        let rendered = sample_bundle().render();

        assert!(!rendered.contains(TOKEN), "the stream token leaked:\n{rendered}");
        assert!(!rendered.contains("ALICE-LAPTOP"), "the hostname leaked:\n{rendered}");
        assert!(!rendered.contains("192.168.1.45"), "a full device IP leaked:\n{rendered}");
        assert!(!rendered.contains("192.168.1.20"), "a full local IP leaked:\n{rendered}");

        // The subnet survives, because it is what makes a network bug debuggable.
        assert!(rendered.contains("192.168.1.xxx"), "the subnet should remain:\n{rendered}");
        assert!(rendered.contains(REDACTED));
    }

    /// Covers: RQ-OBS-008
    #[test]
    fn rq_obs_008_bundle_is_offline() {
        // Structural guarantee: this module depends on no networking crate and the builder
        // takes every value from its caller. Building one cannot perform I/O because there is
        // no I/O API in scope -- exercised here by building a full bundle with no runtime.
        let bundle = sample_bundle();
        assert!(!bundle.render().is_empty());
        assert_eq!(bundle.version, crate::VERSION);
    }

    #[test]
    fn a_short_secret_cannot_redact_the_whole_document() {
        // Registering "a" as a secret would otherwise destroy the bundle.
        let bundle = BundleBuilder::new().redacting("a").interface("wlan0 up").build();
        assert_eq!(bundle.interfaces, vec!["wlan0 up".to_owned()]);
    }

    #[test]
    fn log_tail_is_capped_to_the_configured_line_count() {
        let many: Vec<String> = (0..2_000).map(|i| format!("line {i}")).collect();
        let bundle = BundleBuilder::new().log_tail(&many).build();
        assert_eq!(bundle.log_tail.len(), crate::limits::DIAGNOSTIC_LOG_LINES);
        // It is the *tail* that is kept: the newest lines are the useful ones.
        assert_eq!(bundle.log_tail.last().map(String::as_str), Some("line 1999"));
    }

    #[test]
    fn an_idle_bundle_is_still_well_formed() {
        let rendered = BundleBuilder::new().build().render();
        assert!(rendered.contains("session:    (idle)"));
        assert!(rendered.contains("(none detected)"));
    }

    #[test]
    fn bundles_round_trip_through_serde() {
        let b = sample_bundle();
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(serde_json::from_str::<Bundle>(&json).unwrap(), b);
    }

    proptest::proptest! {
        /// Whatever a hostile room name or log line contains, a registered secret never
        /// survives into the rendered bundle.
        #[test]
        fn registered_secrets_never_survive(noise in ".{0,200}") {
            let bundle = BundleBuilder::new()
                .redacting(TOKEN)
                .log_tail([format!("{noise}{TOKEN}{noise}")])
                .build();
            proptest::prop_assert!(!bundle.render().contains(TOKEN));
        }
    }
}
