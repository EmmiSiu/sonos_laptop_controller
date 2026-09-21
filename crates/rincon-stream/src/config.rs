//! Server configuration, with the dangerous defaults made impossible rather than discouraged.
//!
//! The one setting that matters most is the bind address. `0.0.0.0` is the reflexive choice and
//! it is wrong here: it offers the user's system audio to every interface the machine has,
//! including a VPN tunnel into a corporate network and whatever a hotel Wi-Fi puts us on.
//!
//! Binding to a specific LAN address is not merely safer, it is also *more likely to work* —
//! the speaker has to route back to whatever address it is given, and the interface discovery
//! reached it on is the one address known to be routable from there.
//!
//! So the wildcard is available, and it takes a deliberate call to a conspicuously named
//! method to get it.

use std::net::{IpAddr, SocketAddr};

use rincon_core::audio::AudioFormat;

/// How the audio is framed on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Framing {
    /// A WAV header followed by raw PCM, served until the socket closes. The default.
    #[default]
    EndlessWav,
    /// HTTP chunked transfer encoding with no declared length.
    ///
    /// Kept for the firmware that may one day enforce the WAV `data` size. Not the default
    /// until that firmware is shown to exist; see SPEC-002 §4.
    Chunked,
}

/// Why a configuration was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The bind address was the wildcard without the explicit opt-in.
    #[error(
        "refusing to bind {0}: a wildcard bind exposes system audio on every interface. \
         Call `allow_wildcard_bind()` if that is genuinely intended."
    )]
    WildcardBindNotAllowed(IpAddr),

    /// No peer was allowlisted, which would make the stream unreachable or unguarded.
    #[error("at least one allowed peer is required")]
    NoAllowedPeers,

    /// The connection cap was zero, which would refuse the speaker itself.
    #[error("max_connections must be at least 1")]
    ZeroConnections,
}

/// Everything the stream server needs to start.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// The interface and port to listen on. Port `0` picks an ephemeral port.
    pub bind: SocketAddr,
    /// Addresses permitted to fetch the stream: the target speaker, plus loopback for
    /// diagnostics.
    pub allowed_peers: Vec<IpAddr>,
    /// The format the audio is served in.
    pub format: AudioFormat,
    /// Wire framing.
    pub framing: Framing,
    /// Concurrent connections permitted.
    pub max_connections: usize,
    /// The `data` chunk size written into the WAV header.
    ///
    /// Defaults to the largest a 32-bit RIFF field can express. It is configurable for two
    /// reasons: a test needs to prove the server keeps serving past it (`RQ-STRM-015`), and a
    /// firmware that one day *does* enforce the field would need a different value here rather
    /// than a different server.
    pub declared_data_bytes: u32,
    /// Whether a wildcard bind was explicitly authorised.
    wildcard_allowed: bool,
}

impl StreamConfig {
    /// A configuration bound to `bind`, serving `format` to `peer`.
    #[must_use]
    pub fn new(bind: SocketAddr, peer: IpAddr, format: AudioFormat) -> Self {
        Self {
            bind,
            // Loopback is allowlisted alongside the speaker so `just mvt-stream` can point VLC
            // at the URL without weakening the rule for anything else.
            allowed_peers: vec![peer, IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)],
            format,
            framing: Framing::default(),
            max_connections: 1,
            declared_data_bytes: crate::wav::MAX_DATA_BYTES,
            wildcard_allowed: false,
        }
    }

    /// Permits binding to a wildcard address.
    ///
    /// Deliberately verbose at the call site. There is exactly one legitimate caller — a
    /// diagnostic mode for a user whose interface enumeration is broken — and it should be
    /// obvious in a diff.
    #[must_use]
    pub const fn allow_wildcard_bind(mut self) -> Self {
        self.wildcard_allowed = true;
        self
    }

    /// Sets the concurrent connection cap.
    #[must_use]
    pub const fn with_max_connections(mut self, max: usize) -> Self {
        self.max_connections = max;
        self
    }

    /// Sets the wire framing.
    #[must_use]
    pub const fn with_framing(mut self, framing: Framing) -> Self {
        self.framing = framing;
        self
    }

    /// Overrides the `data` chunk size written into the WAV header.
    #[must_use]
    pub const fn with_declared_data_bytes(mut self, bytes: u32) -> Self {
        self.declared_data_bytes = bytes;
        self
    }

    /// Adds another permitted peer.
    #[must_use]
    pub fn allowing(mut self, peer: IpAddr) -> Self {
        if !self.allowed_peers.contains(&peer) {
            self.allowed_peers.push(peer);
        }
        self
    }

    /// Checks the configuration before a socket is opened.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] describing the first rule violated.
    ///
    /// Covers: RQ-STRM-007
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.bind.ip().is_unspecified() && !self.wildcard_allowed {
            return Err(ConfigError::WildcardBindNotAllowed(self.bind.ip()));
        }
        if self.allowed_peers.is_empty() {
            return Err(ConfigError::NoAllowedPeers);
        }
        if self.max_connections == 0 {
            return Err(ConfigError::ZeroConnections);
        }
        Ok(())
    }

    /// Whether `peer` may fetch the stream.
    ///
    /// Covers: RQ-STRM-004
    #[must_use]
    pub fn permits(&self, peer: IpAddr) -> bool {
        self.allowed_peers.iter().any(|allowed| same_host(*allowed, peer))
    }
}

/// Compares two addresses, unwrapping IPv4-mapped IPv6 first.
///
/// A dual-stack listener reports an IPv4 peer as `::ffff:192.168.1.45`. Without this, the
/// allowlist silently rejects the very speaker it was built for — a failure that presents as a
/// firewall problem and is miserable to diagnose.
fn same_host(a: IpAddr, b: IpAddr) -> bool {
    fn normalise(ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            v4 @ IpAddr::V4(_) => v4,
        }
    }
    normalise(a) == normalise(b)
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

    fn format() -> AudioFormat {
        AudioFormat::new(44_100, 2, SampleEncoding::S16Le).unwrap()
    }

    fn config(bind: &str) -> StreamConfig {
        StreamConfig::new(bind.parse().unwrap(), "192.168.1.45".parse().unwrap(), format())
    }

    /// Covers: RQ-STRM-007
    #[test]
    fn rq_strm_007_wildcard_bind_requires_opt_in() {
        let wildcard = config("0.0.0.0:0");
        assert!(
            matches!(wildcard.validate(), Err(ConfigError::WildcardBindNotAllowed(_))),
            "the wildcard must be refused by default"
        );

        // IPv6's wildcard is the same mistake spelled differently.
        assert!(matches!(config("[::]:0").validate(), Err(ConfigError::WildcardBindNotAllowed(_))));

        // A specific interface is always fine.
        config("192.168.1.20:0").validate().unwrap();
        config("127.0.0.1:0").validate().unwrap();

        // And the opt-in works, because a diagnostic mode legitimately needs it.
        config("0.0.0.0:0").allow_wildcard_bind().validate().unwrap();
    }

    /// Covers: RQ-STRM-004
    #[test]
    fn rq_strm_004_only_allowlisted_peers_are_permitted() {
        let config = config("192.168.1.20:0");

        assert!(config.permits("192.168.1.45".parse().unwrap()), "the target speaker");
        assert!(config.permits("127.0.0.1".parse().unwrap()), "loopback, for diagnostics");

        assert!(!config.permits("192.168.1.99".parse().unwrap()), "another host on the LAN");
        assert!(!config.permits("10.0.0.1".parse().unwrap()), "another subnet");
        assert!(!config.permits("8.8.8.8".parse().unwrap()), "the internet");
    }

    #[test]
    fn a_dual_stack_listener_still_recognises_its_own_speaker() {
        // The bug this prevents: a dual-stack socket reports the speaker as
        // `::ffff:192.168.1.45`, the allowlist holds `192.168.1.45`, and a naive comparison
        // rejects it. The symptom is "the speaker never connects" -- indistinguishable from a
        // firewall block, and far harder to find.
        let config = config("192.168.1.20:0");
        assert!(config.permits("::ffff:192.168.1.45".parse().unwrap()));
        assert!(!config.permits("::ffff:192.168.1.99".parse().unwrap()));
    }

    #[test]
    fn an_empty_allowlist_or_zero_cap_is_refused() {
        let mut empty = config("192.168.1.20:0");
        empty.allowed_peers.clear();
        assert_eq!(empty.validate().unwrap_err(), ConfigError::NoAllowedPeers);

        let zero = config("192.168.1.20:0").with_max_connections(0);
        assert_eq!(zero.validate().unwrap_err(), ConfigError::ZeroConnections);
    }

    #[test]
    fn additional_peers_can_be_allowed_without_duplication() {
        let config = config("192.168.1.20:0")
            .allowing("192.168.1.46".parse().unwrap())
            .allowing("192.168.1.46".parse().unwrap());
        assert_eq!(config.allowed_peers.len(), 3, "duplicates must not accumulate");
        assert!(config.permits("192.168.1.46".parse().unwrap()));
    }

    #[test]
    fn the_defaults_are_the_conservative_ones() {
        let config = config("192.168.1.20:0");
        assert_eq!(config.max_connections, 1, "one speaker per session");
        assert_eq!(config.framing, Framing::EndlessWav);
        assert_eq!(config.declared_data_bytes, crate::wav::MAX_DATA_BYTES);
        assert!(!config.wildcard_allowed, "the wildcard is never on by default");
    }
}
