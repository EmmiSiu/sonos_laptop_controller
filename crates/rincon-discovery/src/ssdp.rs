//! SSDP message construction and response parsing.
//!
//! SSDP is HTTP-shaped text over UDP multicast. "HTTP-shaped" is doing a lot of work in that
//! sentence: real devices send `\n` instead of `\r\n`, arbitrary header casing, duplicate
//! headers, and trailing garbage. A strict parser finds nothing on a real home network.
//!
//! At the same time, **every byte here is unauthenticated**. Any host on the LAN can answer,
//! and a hostile one will send exactly the input a lenient parser handles badly. So the rule
//! is: lenient about shape, ruthless about size and content.

use std::collections::BTreeMap;

use rincon_core::limits;

/// The multicast group SSDP discovery uses.
pub const MULTICAST_ADDR: &str = "239.255.255.250";

/// The SSDP port.
pub const MULTICAST_PORT: u16 = 1900;

/// The search target that matches a Sonos player.
pub const ZONE_PLAYER_ST: &str = "urn:schemas-upnp-org:device:ZonePlayer:1";

/// Maximum headers retained from one response, so a hostile responder cannot exhaust memory
/// with a datagram made entirely of distinct header lines.
const MAX_HEADERS: usize = 32;

/// Why a datagram was not usable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SsdpError {
    /// The datagram was larger than [`limits::MAX_SSDP_DATAGRAM`].
    #[error("datagram of {actual} bytes exceeds the {limit} byte limit")]
    TooLarge {
        /// Observed size.
        actual: usize,
        /// Configured ceiling.
        limit: usize,
    },

    /// The payload was not valid UTF-8.
    #[error("datagram is not valid UTF-8")]
    NotUtf8,

    /// The first line was not an HTTP-shaped status or request line.
    #[error("datagram does not start with an HTTP status line")]
    NotHttpShaped,

    /// A header required to act on the response was absent.
    #[error("response is missing the `{0}` header")]
    MissingHeader(&'static str),
}

/// Builds an `M-SEARCH` request for `target`.
///
/// `mx` is the maximum seconds a responder may wait before replying; keeping it low keeps
/// scans fast, at the cost of more simultaneous replies.
#[must_use]
pub fn msearch(target: &str, mx: u8) -> Vec<u8> {
    // CRLF is mandatory in the request even though we tolerate LF in responses: we are the
    // one party in this exchange who can afford to be correct.
    format!(
        "M-SEARCH * HTTP/1.1\r\n\
         HOST: {MULTICAST_ADDR}:{MULTICAST_PORT}\r\n\
         MAN: \"ssdp:discover\"\r\n\
         MX: {mx}\r\n\
         ST: {target}\r\n\
         \r\n"
    )
    .into_bytes()
}

/// A parsed SSDP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsdpResponse {
    /// Where the device description document lives.
    pub location: String,
    /// The search target the device matched.
    pub search_target: String,
    /// Unique service name; carries the UDN.
    pub usn: String,
    /// Server banner, when present. Useful for diagnostics only.
    pub server: Option<String>,
}

impl SsdpResponse {
    /// Extracts the UDN from the USN, which has the form `uuid:RINCON_xxx::urn:...`.
    #[must_use]
    pub fn udn(&self) -> &str {
        self.usn.split("::").next().unwrap_or(&self.usn)
    }

    /// Whether this response came from something claiming to be a Sonos player.
    #[must_use]
    pub fn is_zone_player(&self) -> bool {
        self.search_target == ZONE_PLAYER_ST || self.usn.contains("ZonePlayer")
    }
}

/// Parses a datagram into headers, tolerating the line endings and casing real devices use.
///
/// # Errors
///
/// Returns [`SsdpError`] when the datagram is oversized, not UTF-8, or not HTTP-shaped.
///
/// Covers: RQ-DISC-002
pub fn parse_headers(datagram: &[u8]) -> Result<BTreeMap<String, String>, SsdpError> {
    if datagram.len() > limits::MAX_SSDP_DATAGRAM {
        return Err(SsdpError::TooLarge {
            actual: datagram.len(),
            limit: limits::MAX_SSDP_DATAGRAM,
        });
    }

    let text = std::str::from_utf8(datagram).map_err(|_| SsdpError::NotUtf8)?;
    let mut lines = text.lines();

    let status = lines.next().unwrap_or_default().trim();
    // Accept both a response (`HTTP/1.1 200 OK`) and a NOTIFY request line, since some
    // firmware announces rather than replies.
    let http_shaped = status.starts_with("HTTP/1.")
        || status.starts_with("NOTIFY")
        || status.starts_with("M-SEARCH");
    if !http_shaped {
        return Err(SsdpError::NotHttpShaped);
    }

    let mut headers = BTreeMap::new();
    for line in lines {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            // A blank line ends the header block; anything after it is a body we do not want.
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            // A line without a colon is noise from a sloppy implementation, not a reason to
            // discard an otherwise usable response.
            continue;
        };
        if headers.len() >= MAX_HEADERS {
            break;
        }
        headers.insert(name.trim().to_ascii_uppercase(), value.trim().to_owned());
    }

    Ok(headers)
}

/// Parses a datagram into a usable [`SsdpResponse`].
///
/// # Errors
///
/// Returns [`SsdpError`] when the datagram is unusable or lacks `LOCATION`, `ST`, or `USN`.
///
/// Covers: RQ-DISC-001, RQ-DISC-002, RQ-DISC-008
pub fn parse_response(datagram: &[u8]) -> Result<SsdpResponse, SsdpError> {
    let headers = parse_headers(datagram)?;

    let take = |name: &'static str| -> Result<String, SsdpError> {
        headers
            .get(name)
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or(SsdpError::MissingHeader(name))
    };

    // `NT` is the NOTIFY spelling of `ST`; accepting both means announcements work too.
    // The error names `ST` either way: "missing NT" would be a confusing thing to report
    // about a response that simply never carried a search target.
    let search_target =
        take("ST").or_else(|_| take("NT")).map_err(|_| SsdpError::MissingHeader("ST"))?;

    Ok(SsdpResponse {
        location: take("LOCATION")?,
        search_target,
        usn: take("USN")?,
        server: headers.get("SERVER").cloned(),
    })
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

    /// A response shaped the way a real Sonos One answers.
    const REAL: &[u8] = b"HTTP/1.1 200 OK\r\n\
CACHE-CONTROL: max-age = 1800\r\n\
EXT:\r\n\
LOCATION: http://192.168.1.45:1400/xml/device_description.xml\r\n\
SERVER: Linux UPnP/1.0 Sonos/70.3-35220\r\n\
ST: urn:schemas-upnp-org:device:ZonePlayer:1\r\n\
USN: uuid:RINCON_949F3EC13E7601400::urn:schemas-upnp-org:device:ZonePlayer:1\r\n\
\r\n";

    #[test]
    fn a_real_response_parses_completely() {
        let parsed = parse_response(REAL).unwrap();
        assert_eq!(parsed.location, "http://192.168.1.45:1400/xml/device_description.xml");
        assert_eq!(parsed.search_target, ZONE_PLAYER_ST);
        assert_eq!(parsed.udn(), "uuid:RINCON_949F3EC13E7601400");
        assert!(parsed.is_zone_player());
        assert!(parsed.server.as_deref().unwrap().contains("Sonos"));
    }

    /// Covers: RQ-DISC-001
    #[test]
    fn rq_disc_001_discards_incomplete_responses() {
        let cases: [(&str, &[u8], &'static str); 3] = [
            ("no LOCATION", b"HTTP/1.1 200 OK\r\nST: urn:x\r\nUSN: uuid:a\r\n\r\n", "LOCATION"),
            (
                "no ST or NT",
                b"HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.1:1400/x\r\nUSN: uuid:a\r\n\r\n",
                "ST",
            ),
            (
                "no USN",
                b"HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.1:1400/x\r\nST: urn:x\r\n\r\n",
                "USN",
            ),
        ];
        for (name, datagram, missing) in cases {
            match parse_response(datagram) {
                Err(SsdpError::MissingHeader(header)) => assert_eq!(header, missing, "{name}"),
                other => panic!("{name}: expected a missing-header error, got {other:?}"),
            }
        }

        // An empty header value counts as missing, not as an empty location we would then dial.
        let blank = b"HTTP/1.1 200 OK\r\nLOCATION:\r\nST: urn:x\r\nUSN: uuid:a\r\n\r\n";
        assert!(matches!(parse_response(blank), Err(SsdpError::MissingHeader("LOCATION"))));
    }

    /// Covers: RQ-DISC-002
    #[test]
    fn rq_disc_002_header_parsing_is_lenient() {
        // Bare LF, lowercase names, extra spaces, a junk line, and a trailing body: every
        // one of these appears in the wild, and a strict parser finds nothing on a real LAN.
        let sloppy = b"HTTP/1.1 200 OK\n\
location:   http://192.168.1.45:1400/xml/device_description.xml  \n\
St:urn:schemas-upnp-org:device:ZonePlayer:1\n\
this line has no colon\n\
usn: uuid:RINCON_TEST::urn:schemas-upnp-org:device:ZonePlayer:1\n\
\n\
trailing body that must be ignored\n";

        let parsed = parse_response(sloppy).unwrap();
        assert_eq!(parsed.location, "http://192.168.1.45:1400/xml/device_description.xml");
        assert_eq!(parsed.search_target, ZONE_PLAYER_ST);
        assert_eq!(parsed.udn(), "uuid:RINCON_TEST");
    }

    #[test]
    fn notify_announcements_are_accepted_through_the_nt_header() {
        let notify = b"NOTIFY * HTTP/1.1\r\n\
HOST: 239.255.255.250:1900\r\n\
LOCATION: http://192.168.1.51:1400/xml/device_description.xml\r\n\
NT: urn:schemas-upnp-org:device:ZonePlayer:1\r\n\
USN: uuid:RINCON_BEAM::urn:schemas-upnp-org:device:ZonePlayer:1\r\n\
NTS: ssdp:alive\r\n\r\n";
        let parsed = parse_response(notify).unwrap();
        assert_eq!(parsed.udn(), "uuid:RINCON_BEAM");
    }

    /// Covers: RQ-DISC-008
    #[test]
    fn rq_disc_008_malformed_input_never_panics() {
        // The adversarial corpus. None of these may panic; all must produce `Err` or a
        // harmless parse.
        let hostile: Vec<Vec<u8>> = vec![
            vec![],
            vec![0x00; 16],
            vec![0xFF, 0xFE, 0xFD],
            b"HTTP/1.1 200 OK".to_vec(),
            b"HTTP/1.1 200 OK\r\n".to_vec(),
            b"\r\n\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\n:::::\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nLOCATION\r\n".to_vec(),
            // A header line made of a single very long token.
            [b"HTTP/1.1 200 OK\r\nX: ".to_vec(), vec![b'a'; 4000]].concat(),
            // Many distinct headers, to exercise the retention cap.
            {
                let mut d = b"HTTP/1.1 200 OK\r\n".to_vec();
                for i in 0..500 {
                    d.extend_from_slice(format!("H{i}: v\r\n").as_bytes());
                }
                d
            },
            // Valid UTF-8 multibyte in a header value.
            "HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.1:1400/\r\nST: x\r\nUSN: café\r\n\r\n"
                .as_bytes()
                .to_vec(),
        ];

        for (index, datagram) in hostile.iter().enumerate() {
            // The assertion is simply that this returns rather than unwinding.
            let _ = parse_response(datagram);
            let _ = parse_headers(datagram);
            assert!(datagram.len() < 100_000, "corpus entry {index} is implausibly large");
        }
    }

    #[test]
    fn oversized_datagrams_are_refused_before_parsing() {
        let huge = vec![b'x'; limits::MAX_SSDP_DATAGRAM + 1];
        assert!(matches!(parse_response(&huge), Err(SsdpError::TooLarge { .. })));
    }

    #[test]
    fn invalid_utf8_is_refused_rather_than_lossily_decoded() {
        let mut bad = b"HTTP/1.1 200 OK\r\nST: ".to_vec();
        bad.extend_from_slice(&[0xC3, 0x28]); // invalid two-byte sequence
        assert_eq!(parse_response(&bad).unwrap_err(), SsdpError::NotUtf8);
    }

    #[test]
    fn non_http_payloads_are_rejected_immediately() {
        assert_eq!(parse_response(b"GARBAGE\r\n\r\n").unwrap_err(), SsdpError::NotHttpShaped);
        assert_eq!(parse_response(b"{\"json\": true}").unwrap_err(), SsdpError::NotHttpShaped);
    }

    #[test]
    fn the_msearch_request_is_wire_correct() {
        let request = String::from_utf8(msearch(ZONE_PLAYER_ST, 1)).unwrap();
        assert!(request.starts_with("M-SEARCH * HTTP/1.1\r\n"));
        assert!(request.contains("HOST: 239.255.255.250:1900\r\n"));
        assert!(request.contains("MAN: \"ssdp:discover\"\r\n"), "MAN must be quoted per the spec");
        assert!(request.contains("MX: 1\r\n"));
        assert!(request.contains(&format!("ST: {ZONE_PLAYER_ST}\r\n")));
        assert!(request.ends_with("\r\n\r\n"), "the header block must be terminated");
        assert!(
            !request.contains(
                "

"
            ),
            "a bare LF pair would truncate the request"
        );
    }

    #[test]
    fn a_usn_without_a_separator_still_yields_a_udn() {
        let response = SsdpResponse {
            location: "http://192.168.1.1:1400/x".into(),
            search_target: ZONE_PLAYER_ST.into(),
            usn: "uuid:RINCON_BARE".into(),
            server: None,
        };
        assert_eq!(response.udn(), "uuid:RINCON_BARE");
    }

    proptest::proptest! {
        /// Arbitrary bytes never panic the parser. This is the property a fuzz target
        /// explores more deeply overnight; keeping a cheap version here means a regression
        /// fails the PR rather than waiting for the nightly run.
        #[test]
        fn parsing_arbitrary_bytes_is_total(
            raw in proptest::collection::vec(proptest::num::u8::ANY, 0..2048)
        ) {
            let _ = parse_response(&raw);
        }

        /// Any header name casing yields the same result.
        #[test]
        fn header_lookup_is_case_insensitive(upper in proptest::bool::ANY) {
            let (loc, st, usn) = if upper {
                ("LOCATION", "ST", "USN")
            } else {
                ("location", "st", "usn")
            };
            let datagram = format!(
                "HTTP/1.1 200 OK\r\n{loc}: http://192.168.1.9:1400/x\r\n{st}: t\r\n{usn}: u\r\n\r\n"
            );
            let parsed = parse_response(datagram.as_bytes()).unwrap();
            proptest::prop_assert_eq!(parsed.location, "http://192.168.1.9:1400/x");
        }
    }
}
