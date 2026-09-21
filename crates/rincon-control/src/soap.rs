//! SOAP envelope construction and response parsing.
//!
//! # The injection problem
//!
//! Every UPnP action interpolates values into an XML document that we then send to a device on
//! the user's network. One of those values is a URL we built; another is metadata containing a
//! title. If interpolation is not escaped, a value containing `</u:SetAVTransportURI>` closes
//! the element early and the remainder of the string becomes *markup* — the attacker writes the
//! rest of our request.
//!
//! This module therefore has exactly one rule, and it is enforced by the type system rather
//! than by discipline: **no caller ever writes XML.** Callers supply `(name, value)` pairs and
//! [`envelope`] does the escaping. There is no public function that accepts pre-built markup.
//!
//! The property tests below generate hostile values rather than checking a fixed list, because
//! a fixed list only ever proves the cases someone already thought of.

use std::borrow::Cow;
use std::collections::BTreeMap;

use quick_xml::Reader;
use quick_xml::events::Event;
use rincon_core::xml::{self, XmlGuardError};

/// A UPnP service Rincon talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Service {
    /// Transport control: what is playing and whether it is playing.
    AvTransport,
    /// Volume and mute.
    RenderingControl,
    /// Group membership, used to find the coordinator.
    ZoneGroupTopology,
}

impl Service {
    /// The service type URN, which appears in both the envelope and the `SOAPACTION` header.
    #[must_use]
    pub const fn urn(self) -> &'static str {
        match self {
            Self::AvTransport => "urn:schemas-upnp-org:service:AVTransport:1",
            Self::RenderingControl => "urn:schemas-upnp-org:service:RenderingControl:1",
            Self::ZoneGroupTopology => "urn:schemas-upnp-org:service:ZoneGroupTopology:1",
        }
    }

    /// The control endpoint path on the device.
    #[must_use]
    pub const fn control_path(self) -> &'static str {
        match self {
            Self::AvTransport => "/MediaRenderer/AVTransport/Control",
            Self::RenderingControl => "/MediaRenderer/RenderingControl/Control",
            Self::ZoneGroupTopology => "/ZoneGroupTopology/Control",
        }
    }
}

/// Why a SOAP exchange failed at the protocol level.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SoapError {
    /// The device returned a SOAP fault.
    #[error("device returned UPnP error {code}: {description}")]
    Fault {
        /// The UPnP error code, e.g. 701.
        code: u16,
        /// The device's description of it.
        description: String,
    },

    /// The response was refused before parsing.
    #[error(transparent)]
    Guard(#[from] XmlGuardError),

    /// The response was not well-formed XML.
    #[error("malformed SOAP response: {0}")]
    Malformed(String),

    /// The response did not contain the expected action result.
    #[error("SOAP response has no `{0}` element")]
    MissingElement(String),
}

/// Escapes a value for inclusion in XML character data or an attribute.
///
/// All five predefined entities are escaped, including `'` and `"`, so the same function is
/// correct in both contexts. Borrowing when nothing needs escaping keeps the common path free
/// of an allocation.
#[must_use]
pub fn escape(value: &str) -> Cow<'_, str> {
    if !value.bytes().any(|b| matches!(b, b'&' | b'<' | b'>' | b'"' | b'\'')) {
        return Cow::Borrowed(value);
    }
    let mut out = String::with_capacity(value.len() + 16);
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    Cow::Owned(out)
}

/// Builds a complete SOAP envelope for `action` on `service`.
///
/// Arguments are `(element name, value)`. Values are escaped; names are not interpolated from
/// untrusted input anywhere in the workspace and are `&'static str` at every call site.
///
/// Covers: RQ-CTL-001, RQ-CTL-002, RQ-CTL-003
#[must_use]
pub fn envelope(service: Service, action: &str, args: &[(&str, &str)]) -> String {
    let urn = service.urn();
    let mut body = String::with_capacity(512);

    body.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>");
    body.push_str(
        "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">",
    );
    body.push_str("<s:Body>");
    body.push_str("<u:");
    body.push_str(action);
    body.push_str(" xmlns:u=\"");
    body.push_str(urn);
    body.push_str("\">");

    for (name, value) in args {
        body.push('<');
        body.push_str(name);
        body.push('>');
        body.push_str(&escape(value));
        body.push_str("</");
        body.push_str(name);
        body.push('>');
    }

    body.push_str("</u:");
    body.push_str(action);
    body.push('>');
    body.push_str("</s:Body></s:Envelope>");
    body
}

/// The exact `SOAPACTION` header value for an action.
///
/// Covers: RQ-CTL-005
#[must_use]
pub fn action_header(service: Service, action: &str) -> String {
    format!("\"{}#{action}\"", service.urn())
}

/// Parses a SOAP response body into its output arguments.
///
/// Recognises a `Fault` body and converts it into [`SoapError::Fault`] rather than returning
/// an empty success — the failure mode that would otherwise present as "the command worked but
/// nothing happened".
///
/// # Errors
///
/// Returns [`SoapError`] for a fault, a guard refusal, or malformed XML.
///
/// Covers: RQ-CTL-006, RQ-CTL-013
pub fn parse_response(body: &str) -> Result<BTreeMap<String, String>, SoapError> {
    xml::reject_doctype(body)?;

    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);

    let mut values: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut saw_fault = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let name = local_name(element.name().as_ref());
                if name.eq_ignore_ascii_case("Fault") {
                    saw_fault = true;
                }
                current = Some(name);
            }
            Ok(Event::End(_)) => current = None,
            Ok(Event::Text(text)) => {
                let Some(name) = current.clone() else { continue };
                let value = text
                    .unescape()
                    .map_err(|source| SoapError::Malformed(source.to_string()))?
                    .into_owned();
                values.entry(name).or_insert(value);
            }
            Ok(Event::Eof) => break,
            Err(source) => return Err(SoapError::Malformed(source.to_string())),
            Ok(_) => {}
        }
    }

    if saw_fault {
        return Err(fault_from(&values));
    }
    Ok(values)
}

/// Builds a typed fault from the elements of a `Fault` body.
fn fault_from(values: &BTreeMap<String, String>) -> SoapError {
    let code = values
        .get("errorCode")
        .and_then(|raw| raw.trim().parse::<u16>().ok())
        .unwrap_or(0);
    let description = values
        .get("errorDescription")
        .or_else(|| values.get("faultstring"))
        .cloned()
        .unwrap_or_else(|| "no description".to_owned());
    SoapError::Fault { code, description }
}

fn local_name(raw: &[u8]) -> String {
    let bytes = raw.rsplit(|&b| b == b':').next().unwrap_or(raw);
    String::from_utf8_lossy(bytes).into_owned()
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

    /// The exact bytes a real Sonos player accepts for `Play`. Committed as a golden fixture:
    /// if a refactor changes one character of this, the test fails rather than the speaker.
    const GOLDEN_PLAY: &str = concat!(
        r#"<?xml version="1.0" encoding="utf-8"?>"#,
        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" "#,
        r#"s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">"#,
        r"<s:Body>",
        r#"<u:Play xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">"#,
        r"<InstanceID>0</InstanceID><Speed>1</Speed>",
        r"</u:Play>",
        r"</s:Body></s:Envelope>",
    );

    /// Covers: RQ-CTL-001
    #[test]
    fn rq_ctl_001_envelope_matches_golden() {
        let built =
            envelope(Service::AvTransport, "Play", &[("InstanceID", "0"), ("Speed", "1")]);
        assert_eq!(built, GOLDEN_PLAY);
    }

    /// Covers: RQ-CTL-005
    #[test]
    fn rq_ctl_005_soapaction_header_is_exact() {
        assert_eq!(
            action_header(Service::AvTransport, "SetAVTransportURI"),
            "\"urn:schemas-upnp-org:service:AVTransport:1#SetAVTransportURI\""
        );
        assert_eq!(
            action_header(Service::RenderingControl, "SetVolume"),
            "\"urn:schemas-upnp-org:service:RenderingControl:1#SetVolume\""
        );
        // The quotes are mandatory: an unquoted SOAPACTION is rejected by some firmware.
        let header = action_header(Service::ZoneGroupTopology, "GetZoneGroupState");
        assert!(header.starts_with('"') && header.ends_with('"'));
    }

    /// Covers: RQ-CTL-002
    #[test]
    fn rq_ctl_002_all_values_are_escaped() {
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("a & b"), "a &amp; b");
        assert_eq!(escape("<tag>"), "&lt;tag&gt;");
        assert_eq!(escape(r#"say "hi""#), "say &quot;hi&quot;");
        assert_eq!(escape("it's"), "it&apos;s");
        // All five predefined entities, in one value.
        assert_eq!(escape(r#"&<>"'"#), "&amp;&lt;&gt;&quot;&apos;");
    }

    /// Covers: RQ-CTL-003
    #[test]
    fn rq_ctl_003_injection_cannot_escape_element() {
        // The attack: a value that closes our element and appends markup of its own.
        let attack = "http://x/</CurrentURI><Speed>666</Speed><CurrentURI>";
        let built = envelope(Service::AvTransport, "SetAVTransportURI", &[("CurrentURI", attack)]);

        // Exactly one opening and one closing tag for the element we created.
        assert_eq!(built.matches("<CurrentURI>").count(), 1, "in: {built}");
        assert_eq!(built.matches("</CurrentURI>").count(), 1, "in: {built}");
        // The injected element never becomes markup.
        assert!(!built.contains("<Speed>"), "injected element became markup: {built}");
        assert!(built.contains("&lt;Speed&gt;666&lt;/Speed&gt;"), "in: {built}");

        // And the envelope itself cannot be terminated early.
        let terminator = envelope(
            Service::AvTransport,
            "Play",
            &[("InstanceID", "</s:Body></s:Envelope><evil/>")],
        );
        assert_eq!(terminator.matches("</s:Envelope>").count(), 1);
        assert!(!terminator.contains("<evil/>"));
    }

    /// Covers: RQ-CTL-006
    #[test]
    fn rq_ctl_006_parses_fault_into_typed_error() {
        // A real Sonos fault for "the speaker is doing something else".
        let fault = r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <s:Fault>
      <faultcode>s:Client</faultcode>
      <faultstring>UPnPError</faultstring>
      <detail>
        <UPnPError xmlns="urn:schemas-upnp-org:control-1-0">
          <errorCode>701</errorCode>
          <errorDescription>Transition not available</errorDescription>
        </UPnPError>
      </detail>
    </s:Fault>
  </s:Body>
</s:Envelope>"#;

        match parse_response(fault) {
            Err(SoapError::Fault { code, description }) => {
                assert_eq!(code, 701);
                assert_eq!(description, "Transition not available");
            }
            other => panic!("a fault must never read as success: {other:?}"),
        }
    }

    #[test]
    fn a_fault_without_an_error_code_is_still_a_fault() {
        // The dangerous failure is treating this as success, not getting the code wrong.
        let vague = "<s:Envelope xmlns:s='http://schemas.xmlsoap.org/soap/envelope/'><s:Body>\
            <s:Fault><faultstring>Something broke</faultstring></s:Fault></s:Body></s:Envelope>";
        match parse_response(vague) {
            Err(SoapError::Fault { code, description }) => {
                assert_eq!(code, 0);
                assert_eq!(description, "Something broke");
            }
            other => panic!("expected a fault, got {other:?}"),
        }
    }

    #[test]
    fn a_success_response_yields_its_output_arguments() {
        let ok = "<s:Envelope xmlns:s='http://schemas.xmlsoap.org/soap/envelope/'><s:Body>\
            <u:GetVolumeResponse xmlns:u='urn:schemas-upnp-org:service:RenderingControl:1'>\
            <CurrentVolume>34</CurrentVolume></u:GetVolumeResponse></s:Body></s:Envelope>";
        let values = parse_response(ok).unwrap();
        assert_eq!(values.get("CurrentVolume").map(String::as_str), Some("34"));
    }

    /// Covers: RQ-CTL-013
    #[test]
    fn rq_ctl_013_rejects_doctype() {
        let hostile = "<!DOCTYPE x [<!ENTITY e SYSTEM 'file:///etc/passwd'>]><s:Envelope/>";
        assert!(matches!(
            parse_response(hostile),
            Err(SoapError::Guard(XmlGuardError::DoctypeDeclared))
        ));
    }

    #[test]
    fn malformed_responses_never_panic() {
        for body in ["", "<", "<s:Envelope>", "</s:Body>", "\u{0}", "not xml", "<a><b></a>"] {
            let _ = parse_response(body);
        }
    }

    #[test]
    fn service_endpoints_match_the_paths_sonos_serves() {
        assert_eq!(Service::AvTransport.control_path(), "/MediaRenderer/AVTransport/Control");
        assert_eq!(
            Service::RenderingControl.control_path(),
            "/MediaRenderer/RenderingControl/Control"
        );
        assert_eq!(Service::ZoneGroupTopology.control_path(), "/ZoneGroupTopology/Control");
    }

    proptest::proptest! {
        /// Whatever a value contains, the envelope keeps exactly the structure we built: one
        /// body, one action element, one element per argument. This is the real statement of
        /// "injection is impossible", as opposed to "the cases I thought of are handled".
        #[test]
        fn structure_survives_any_argument_value(value in ".{0,200}") {
            let built = envelope(Service::AvTransport, "SetAVTransportURI", &[
                ("InstanceID", "0"),
                ("CurrentURI", &value),
                ("CurrentURIMetaData", &value),
            ]);

            proptest::prop_assert_eq!(built.matches("<s:Body>").count(), 1);
            proptest::prop_assert_eq!(built.matches("</s:Body>").count(), 1);
            proptest::prop_assert_eq!(built.matches("<u:SetAVTransportURI ").count(), 1);
            proptest::prop_assert_eq!(built.matches("</u:SetAVTransportURI>").count(), 1);
            proptest::prop_assert_eq!(built.matches("<CurrentURI>").count(), 1);
            proptest::prop_assert_eq!(built.matches("</CurrentURI>").count(), 1);
        }

        /// An escaped value contains none of the five raw metacharacters.
        #[test]
        fn escaping_leaves_no_raw_metacharacters(value in ".{0,200}") {
            let escaped = escape(&value);
            for forbidden in ['<', '>', '"', '\''] {
                proptest::prop_assert!(
                    !escaped.contains(forbidden),
                    "`{}` survived escaping of {:?}", forbidden, value
                );
            }
            // `&` may appear, but only as the start of an entity we produced.
            for (index, _) in escaped.match_indices('&') {
                let tail = escaped.get(index..).unwrap_or("");
                proptest::prop_assert!(
                    ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;"]
                        .iter()
                        .any(|entity| tail.starts_with(entity)),
                    "a bare ampersand survived in {:?}", escaped
                );
            }
        }

        /// Round-tripping through a parser returns the original value -- the property that
        /// actually matters to a device, as opposed to "the bytes look escaped".
        ///
        /// The alphabet deliberately contains every XML metacharacter but no control
        /// characters. Those are excluded because XML *specifies* that a parser normalises
        /// line endings and trims insignificant whitespace, and because `str::trim` and
        /// `quick-xml` disagree about which exotic code points count as whitespace. Asserting
        /// that a vertical tab survives verbatim would be testing a claim that is false in
        /// both directions.
        #[test]
        fn escaped_values_round_trip_through_a_parser(
            value in "[a-zA-Z0-9&<>\"'/:._=?-]{1,80}"
        ) {
            let built = envelope(Service::AvTransport, "Probe", &[("Value", &value)]);
            let parsed = parse_response(&built).map_err(|error| {
                proptest::test_runner::TestCaseError::fail(format!("unparseable: {error}"))
            })?;
            proptest::prop_assert_eq!(parsed.get("Value").map(String::as_str), Some(&*value));
        }
    }
}
