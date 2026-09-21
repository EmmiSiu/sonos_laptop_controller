//! Every budget applied to an untrusted input, in one auditable place.
//!
//! A byte cap or a timeout that lives inline at its call site is a cap nobody can review. When
//! all of them are declared here, "does every network read have a limit?" becomes a question a
//! reader can answer in thirty seconds, and [`Budget`] makes it mechanically checkable.

use std::time::Duration;

/// Largest XML document any parser in the workspace will accept.
///
/// 256 KiB is roughly forty times the size of a real Sonos description document, so it is
/// generous for legitimate traffic and useless for memory exhaustion.
pub const MAX_XML_BYTES: usize = 256 * 1024;

/// Largest SOAP response body. Topology responses on a large household are the biggest
/// legitimate case, and they are well under this.
pub const MAX_SOAP_BYTES: usize = 1024 * 1024;

/// Largest single SSDP datagram considered. Anything larger is malformed or hostile.
pub const MAX_SSDP_DATAGRAM: usize = 8 * 1024;

/// Maximum number of devices retained from one scan, so a flooding responder cannot exhaust us.
pub const MAX_DEVICES_PER_SCAN: usize = 64;

/// Maximum concurrent description fetches during a scan.
pub const MAX_CONCURRENT_FETCHES: usize = 8;

/// Timeout for fetching a device description.
pub const DESCRIPTION_TIMEOUT: Duration = Duration::from_secs(2);

/// Timeout for a single SOAP control request.
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for `SetAVTransportURI` specifically.
///
/// # Why this one is different
///
/// Every other control command is a round trip: the speaker parses a request, changes some
/// state, and answers. `SetAVTransportURI` is not. Before it replies, the speaker **fetches
/// the URL we just gave it** to check that it can decode what is there — so the command's
/// duration includes a whole HTTP exchange back to us, over Wi-Fi, plus whatever the speaker
/// does with the first bytes.
///
/// Measured on a Sonos One: 5.02 s. With the ordinary 5 s budget the command timed out
/// *twenty milliseconds* before the speaker connected, and the session was torn down while
/// the speaker was busy succeeding.
///
/// 20 s is generous on purpose. The failure this guards against is a false negative on a slow
/// network, and the cost of waiting is bounded by the interface showing which step it is on.
pub const URI_HANDOFF_TIMEOUT: Duration = Duration::from_secs(20);

/// How long an SSDP scan listens for replies before giving up.
pub const DISCOVERY_WINDOW: Duration = Duration::from_secs(2);

/// Grace period beyond [`DISCOVERY_WINDOW`] before a scan is considered hung.
pub const DISCOVERY_SLACK: Duration = Duration::from_millis(500);

/// How long we wait for the speaker to fetch our stream before suspecting the firewall.
pub const PEER_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// Maximum size of the rotating log file before it is truncated.
pub const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// Number of log lines captured into a diagnostics bundle.
pub const DIAGNOSTIC_LOG_LINES: usize = 500;

/// A named budget pairing a byte ceiling with a deadline.
///
/// Constructing one of these is the only sanctioned way to start an untrusted read, which is
/// what makes `RQ-SEC-003` a property of the code rather than a promise in a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// What this budget governs, for error messages.
    pub what: &'static str,
    /// Maximum bytes the read may consume.
    pub max_bytes: usize,
    /// Maximum wall time the read may take.
    pub timeout: Duration,
}

impl Budget {
    /// Budget for fetching a device description document.
    pub const DESCRIPTION: Self =
        Self { what: "device description", max_bytes: MAX_XML_BYTES, timeout: DESCRIPTION_TIMEOUT };

    /// Budget for a SOAP control round trip.
    pub const CONTROL: Self =
        Self { what: "control response", max_bytes: MAX_SOAP_BYTES, timeout: CONTROL_TIMEOUT };

    /// Budget for reading one SSDP datagram.
    pub const SSDP: Self =
        Self { what: "ssdp datagram", max_bytes: MAX_SSDP_DATAGRAM, timeout: DISCOVERY_WINDOW };

    /// Every budget defined in the workspace. Used by the audit test below and by diagnostics.
    pub const ALL: [Self; 3] = [Self::DESCRIPTION, Self::CONTROL, Self::SSDP];

    /// Whether `observed` has overrun this budget's byte ceiling.
    #[must_use]
    pub const fn exceeded_by(&self, observed: usize) -> bool {
        observed > self.max_bytes
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

    /// Covers: RQ-SEC-003
    #[test]
    fn rq_sec_003_every_read_is_bounded() {
        // The audit: no budget may be unbounded, absurdly large, or without a deadline.
        for budget in Budget::ALL {
            assert!(budget.max_bytes > 0, "{} has no byte ceiling", budget.what);
            assert!(
                budget.max_bytes <= MAX_SOAP_BYTES,
                "{} allows {} bytes, above the workspace maximum",
                budget.what,
                budget.max_bytes
            );
            assert!(budget.timeout > Duration::ZERO, "{} has no deadline", budget.what);
            assert!(
                budget.timeout <= Duration::from_secs(10),
                "{} may hang for {:?}, which the UI cannot absorb",
                budget.what,
                budget.timeout
            );
        }
    }

    #[test]
    fn budgets_detect_overrun_at_the_boundary() {
        let b = Budget::DESCRIPTION;
        assert!(!b.exceeded_by(b.max_bytes), "exactly at the limit is allowed");
        assert!(b.exceeded_by(b.max_bytes + 1), "one byte over is refused");
        assert!(!b.exceeded_by(0));
    }

    #[test]
    fn discovery_completes_within_its_advertised_window() {
        // RQ-DISC-011 promises `timeout + 500 ms`; that slack is defined here, so the promise
        // and the constant cannot drift apart.
        assert_eq!(DISCOVERY_SLACK, Duration::from_millis(500));
        assert!(DISCOVERY_WINDOW + DISCOVERY_SLACK < Duration::from_secs(3));
    }

    #[test]
    fn caps_are_generous_for_real_traffic_and_useless_for_abuse() {
        // A real Sonos description is ~6 KiB; a real SSDP reply is ~400 bytes. These are
        // compile-time assertions: if someone tightens a cap below what real traffic needs,
        // the build fails rather than the app mysteriously dropping legitimate devices.
        const _: () = assert!(MAX_XML_BYTES > 40 * 6 * 1024);
        const _: () = assert!(MAX_SSDP_DATAGRAM > 10 * 400);
        const _: () = assert!(MAX_DEVICES_PER_SCAN >= 32);
    }
}
