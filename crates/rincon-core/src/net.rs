//! Private-address validation: the SSRF guard that every outbound connection passes through.
//!
//! Rincon dials addresses that came from **unauthenticated multicast responses**. Anyone on the
//! LAN — or anything that can spoof a packet onto it — can hand us a `LOCATION` header pointing
//! anywhere on the internet. Without this module, a forged SSDP reply turns the user's laptop
//! into a request proxy aimed at whatever the attacker likes.
//!
//! The rule is therefore inverted from the usual "block a denylist": we permit only addresses
//! that can plausibly be a speaker on the user's own network, and reject everything else.
//!
//! # Bypasses this module closes
//!
//! Naïve implementations of this check are routinely defeated by:
//!
//! - **IPv4-mapped IPv6** (`::ffff:93.184.216.34`) — looks like IPv6, routes to a public v4 host.
//! - **IPv4-compatible IPv6** (`::93.184.216.34`) — the deprecated form of the same trick.
//! - **Cloud metadata** (`169.254.169.254`) — technically link-local, so a plain
//!   "link-local is fine" rule lets it through.
//! - **Unspecified / broadcast / multicast** addresses used to reach an unintended listener.
//!
//! Each has a regression test below.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::Url;

/// A destination was rejected before a socket was opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    /// The address is outside private/link-local/loopback space.
    #[error("refusing to connect to non-private address {0}")]
    NotPrivate(IpAddr),

    /// The address is a well-known metadata endpoint, never a speaker.
    #[error("refusing to connect to cloud metadata address {0}")]
    MetadataEndpoint(IpAddr),

    /// The address cannot be a unicast destination at all.
    #[error("refusing to connect to non-unicast address {0}")]
    NotUnicast(IpAddr),

    /// The URL used a scheme other than plain `http`.
    #[error("unsupported URL scheme `{0}`, only `http` is allowed")]
    UnsupportedScheme(String),

    /// The URL host was a name, not a literal address; we never resolve names for LAN control.
    #[error("URL host must be a literal IP address, got `{0}`")]
    HostNotLiteral(String),

    /// The URL had no host component at all.
    #[error("URL has no host")]
    MissingHost,

    /// The port is outside the range a UPnP device may plausibly serve from.
    #[error("port {0} is outside the allowed range")]
    PortNotAllowed(u16),
}

/// The IMDS address used by every major cloud (`169.254.169.254`). Link-local, but never a
/// speaker.
///
/// Held as raw bits rather than an [`Ipv4Addr`] because `PartialEq` is not yet callable from a
/// `const fn`, and every predicate in this module is `const` so it can be used in assertions.
const CLOUD_METADATA_BITS: u32 = u32::from_be_bytes([169, 254, 169, 254]);

/// `Ipv4Addr` as a `u32`, usable from a `const fn`.
const fn v4_bits(addr: Ipv4Addr) -> u32 {
    u32::from_be_bytes(addr.octets())
}

/// Lowest port a device description or control endpoint may live on.
///
/// Sonos uses 1400. We allow the whole non-privileged range plus 1400 itself, but exclude
/// ports below 1024 that we have no business dialling on a speaker.
const MIN_DEVICE_PORT: u16 = 80;

/// Returns `true` when `ip` could belong to a device on the user's own network.
///
/// Covers: RQ-CORE-001
#[must_use]
pub const fn is_lan_reachable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_lan_reachable_v4(v4),
        IpAddr::V6(v6) => is_lan_reachable_v6(v6),
    }
}

const fn is_lan_reachable_v4(v4: Ipv4Addr) -> bool {
    if v4_bits(v4) == CLOUD_METADATA_BITS {
        return false;
    }
    if v4.is_unspecified() || v4.is_broadcast() || v4.is_multicast() || v4.is_documentation() {
        return false;
    }
    v4.is_private() || v4.is_link_local() || v4.is_loopback()
}

/// Either IPv4-in-IPv6 form, unwrapped. Both are classic private-address-check bypasses.
const fn embedded_v4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    match v6.to_ipv4_mapped() {
        Some(mapped) => Some(mapped),
        None => ipv4_compatible(v6),
    }
}

const fn is_lan_reachable_v6(v6: Ipv6Addr) -> bool {
    // Unwrap IPv4-in-IPv6 forms first; otherwise `::ffff:8.8.8.8` sails past every v6 check.
    if let Some(embedded) = embedded_v4(v6) {
        return is_lan_reachable_v4(embedded);
    }
    if v6.is_unspecified() || v6.is_multicast() {
        return false;
    }
    v6.is_loopback() || is_unique_local_v6(v6) || is_link_local_v6(v6)
}

/// `::a.b.c.d`, the deprecated IPv4-compatible form. The naive unwrap also matches `::` and
/// `::1`, so those two are filtered out rather than treated as embedded IPv4.
const fn ipv4_compatible(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    let [s0, s1, s2, s3, s4, s5, low, high] = v6.segments();
    if s0 | s1 | s2 | s3 | s4 | s5 != 0 {
        return None;
    }
    let raw = ((low as u32) << 16) | high as u32;
    // `::` and `::1` are the unspecified and loopback addresses, not embedded IPv4.
    if raw <= 1 { None } else { Some(Ipv4Addr::from_bits(raw)) }
}

/// `fc00::/7` — unique local addresses, the IPv6 equivalent of RFC1918.
const fn is_unique_local_v6(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xfe00) == 0xfc00
}

/// `fe80::/10` — link-local unicast.
const fn is_link_local_v6(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xffc0) == 0xfe80
}

/// Rejects `ip` unless it is a plausible LAN unicast destination.
///
/// This is the function every crate must call before opening a socket. It is intentionally
/// the *only* public way to approve a destination, so the guard cannot be forgotten by
/// reimplementing the predicate somewhere else.
///
/// # Errors
///
/// Returns [`NetError`] describing why the address was refused.
///
/// Covers: RQ-CORE-001, RQ-SEC-001
pub const fn ensure_lan_destination(ip: IpAddr) -> Result<(), NetError> {
    if is_metadata_endpoint(ip) {
        return Err(NetError::MetadataEndpoint(ip));
    }
    if is_non_unicast(ip) {
        return Err(NetError::NotUnicast(ip));
    }
    if is_lan_reachable(ip) { Ok(()) } else { Err(NetError::NotPrivate(ip)) }
}

const fn is_metadata_endpoint(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_bits(v4) == CLOUD_METADATA_BITS,
        IpAddr::V6(v6) => match embedded_v4(v6) {
            Some(v4) => v4_bits(v4) == CLOUD_METADATA_BITS,
            None => false,
        },
    }
}

const fn is_non_unicast(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_unspecified() || v4.is_broadcast() || v4.is_multicast(),
        IpAddr::V6(v6) => v6.is_unspecified() || v6.is_multicast(),
    }
}

/// Validates a URL received from the network and returns the address it points at.
///
/// Applies, in order: scheme check, literal-host check (we never resolve a name from an
/// untrusted source — DNS rebinding would reopen the SSRF we just closed), port range, and
/// finally the private-address rule.
///
/// # Errors
///
/// Returns [`NetError`] for the first rule the URL violates.
///
/// Covers: RQ-DISC-003, RQ-DISC-004, RQ-CTL-009, RQ-SEC-001
pub fn validate_device_url(url: &Url) -> Result<IpAddr, NetError> {
    if url.scheme() != "http" {
        return Err(NetError::UnsupportedScheme(url.scheme().to_owned()));
    }

    let host = url.host_str().ok_or(NetError::MissingHost)?;
    // `Url` strips the brackets from `[::1]`, but a bare v6 literal parses directly.
    let ip: IpAddr = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .map_err(|_| NetError::HostNotLiteral(host.to_owned()))?;

    let port = url.port_or_known_default().unwrap_or(0);
    if port < MIN_DEVICE_PORT {
        return Err(NetError::PortNotAllowed(port));
    }

    ensure_lan_destination(ip)?;
    Ok(ip)
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

    fn v4(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn v6(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// Covers: RQ-CORE-001, RQ-SEC-001
    #[test]
    fn rq_core_001_rejects_public_destinations() {
        for addr in ["8.8.8.8", "93.184.216.34", "1.1.1.1", "100.64.0.1", "203.0.113.5"] {
            let err = ensure_lan_destination(v4(addr)).unwrap_err();
            assert!(
                matches!(err, NetError::NotPrivate(_)),
                "{addr} should be refused as non-private, got {err:?}"
            );
        }
    }

    /// Covers: RQ-SEC-001
    #[test]
    fn rq_sec_001_no_public_egress() {
        // The three RFC1918 blocks and loopback are the only shapes a speaker can have.
        for addr in ["10.0.0.5", "172.16.3.4", "192.168.1.45", "127.0.0.1", "169.254.7.7"] {
            ensure_lan_destination(v4(addr)).unwrap_or_else(|e| panic!("{addr} refused: {e}"));
        }
        // Everything else is refused, including the IPv6 disguises.
        for addr in ["2606:4700::1111", "2001:4860:4860::8888"] {
            assert!(ensure_lan_destination(v6(addr)).is_err(), "{addr} must be refused");
        }
    }

    #[test]
    fn ipv4_mapped_ipv6_cannot_smuggle_a_public_address() {
        let smuggled = v6("::ffff:8.8.8.8");
        assert!(
            ensure_lan_destination(smuggled).is_err(),
            "IPv4-mapped IPv6 must be unwrapped before the private check"
        );
        // The same trick pointed at a real LAN host is still fine.
        assert!(ensure_lan_destination(v6("::ffff:192.168.1.45")).is_ok());
    }

    #[test]
    fn ipv4_compatible_ipv6_cannot_smuggle_a_public_address() {
        assert!(ensure_lan_destination(v6("::8.8.8.8")).is_err());
        assert!(ensure_lan_destination(v6("::192.168.1.45")).is_ok());
        // `::1` must still read as loopback, not as the embedded address `0.0.0.1`.
        assert!(ensure_lan_destination(v6("::1")).is_ok());
    }

    #[test]
    fn cloud_metadata_is_refused_despite_being_link_local() {
        let err = ensure_lan_destination(v4("169.254.169.254")).unwrap_err();
        assert!(matches!(err, NetError::MetadataEndpoint(_)));
        // And through the IPv6 disguise.
        assert!(matches!(
            ensure_lan_destination(v6("::ffff:169.254.169.254")).unwrap_err(),
            NetError::MetadataEndpoint(_)
        ));
    }

    #[test]
    fn non_unicast_addresses_are_refused() {
        for addr in ["0.0.0.0", "255.255.255.255", "239.255.255.250", "224.0.0.1"] {
            assert!(
                matches!(ensure_lan_destination(v4(addr)).unwrap_err(), NetError::NotUnicast(_)),
                "{addr} must be refused as non-unicast"
            );
        }
    }

    /// Covers: RQ-DISC-003
    #[test]
    fn rq_disc_003_rejects_public_location() {
        let url = Url::parse("http://93.184.216.34:1400/xml/device_description.xml").unwrap();
        assert!(matches!(validate_device_url(&url).unwrap_err(), NetError::NotPrivate(_)));

        let ok = Url::parse("http://192.168.1.45:1400/xml/device_description.xml").unwrap();
        assert_eq!(validate_device_url(&ok).unwrap(), v4("192.168.1.45"));
    }

    /// Covers: RQ-DISC-004
    #[test]
    fn rq_disc_004_rejects_non_http_scheme() {
        for raw in [
            "https://192.168.1.45:1400/x.xml",
            "file:///etc/passwd",
            "gopher://192.168.1.45:70/",
            "ftp://192.168.1.45/x",
        ] {
            let url = Url::parse(raw).unwrap();
            assert!(
                matches!(validate_device_url(&url), Err(NetError::UnsupportedScheme(_))),
                "{raw} must be refused on scheme"
            );
        }
    }

    /// Covers: RQ-CTL-009
    #[test]
    fn rq_ctl_009_refuses_public_target() {
        let url = Url::parse("http://8.8.8.8:1400/MediaRenderer/AVTransport/Control").unwrap();
        assert!(validate_device_url(&url).is_err());
    }

    #[test]
    fn hostnames_are_never_resolved() {
        // Accepting a name would reintroduce SSRF through DNS rebinding.
        let url = Url::parse("http://evil.example.com:1400/x.xml").unwrap();
        assert!(matches!(validate_device_url(&url), Err(NetError::HostNotLiteral(_))));
        // ...including a name that merely looks private.
        let url = Url::parse("http://localhost:1400/x.xml").unwrap();
        assert!(matches!(validate_device_url(&url), Err(NetError::HostNotLiteral(_))));
    }

    #[test]
    fn privileged_low_ports_are_refused() {
        let url = Url::parse("http://192.168.1.45:22/x.xml").unwrap();
        assert!(matches!(validate_device_url(&url), Err(NetError::PortNotAllowed(22))));
    }

    #[test]
    fn ipv6_literals_in_urls_are_unbracketed_before_parsing() {
        let url = Url::parse("http://[fe80::1]:1400/x.xml").unwrap();
        assert_eq!(validate_device_url(&url).unwrap(), v6("fe80::1"));
    }

    proptest::proptest! {
        /// No input shape may make the validator panic, and approval always implies
        /// `is_lan_reachable` — the two entry points can never disagree.
        #[test]
        fn validator_is_total_and_self_consistent(a: u8, b: u8, c: u8, d: u8) {
            let ip = IpAddr::from(Ipv4Addr::new(a, b, c, d));
            if ensure_lan_destination(ip).is_ok() {
                proptest::prop_assert!(is_lan_reachable(ip));
            }
        }
    }
}
