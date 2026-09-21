//! Parsing the UPnP device description document.
//!
//! A responder hands us a `LOCATION`; fetching it yields this XML, which is where the room
//! name the user recognises actually comes from. It is also the second untrusted document in
//! the pipeline, so everything in [SPEC-007](../../../specs/SPEC-007-security.md) applies:
//! no DOCTYPE, no entities, a hard size cap, and no path that can panic.
//!
//! # The nesting trap
//!
//! A Sonos description contains a top-level `<device>` **and** a `<deviceList>` of embedded
//! sub-devices (`MediaRenderer`, `MediaServer`), each with their own `<friendlyName>` and
//! `<UDN>`. A parser that simply takes "the first `<UDN>` it sees" appears to work and then
//! silently targets a sub-device, which the transport service rejects. This parser therefore
//! tracks depth and reads only the root device's fields.

use quick_xml::Reader;
use quick_xml::events::Event;
use rincon_core::device::{DeviceId, DeviceName};
use rincon_core::xml::{self, XmlGuardError};

/// Why a description document was unusable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DescriptionError {
    /// The document failed the pre-parse guard.
    #[error(transparent)]
    Guard(#[from] XmlGuardError),

    /// The XML was not well-formed.
    #[error("malformed XML: {0}")]
    Malformed(String),

    /// A field Rincon needs was absent or empty.
    #[error("description is missing `{0}`")]
    MissingField(&'static str),

    /// A field was present but failed its domain invariant.
    #[error("description field `{field}` is invalid: {reason}")]
    InvalidField {
        /// Which field.
        field: &'static str,
        /// Why it was refused.
        reason: String,
    },
}

/// The subset of a device description Rincon acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceDescription {
    /// Stable identity from `<UDN>`.
    pub udn: DeviceId,
    /// The room the user knows, from `<roomName>`.
    pub room: DeviceName,
    /// Model string, from `<modelName>` or `<modelDescription>`.
    pub model: DeviceName,
    /// The device type URN, used to confirm this really is a zone player.
    pub device_type: String,
}

impl DeviceDescription {
    /// Whether the document describes a Sonos zone player rather than some other UPnP device.
    #[must_use]
    pub fn is_zone_player(&self) -> bool {
        self.device_type.contains("ZonePlayer") || self.udn.as_str().contains("RINCON")
    }
}

/// Fields collected during the scan, before validation.
#[derive(Debug, Default)]
struct Raw {
    udn: Option<String>,
    room: Option<String>,
    model_name: Option<String>,
    model_description: Option<String>,
    friendly_name: Option<String>,
    device_type: Option<String>,
}

/// Parses a device description document.
///
/// # Errors
///
/// Returns [`DescriptionError`] when the guard refuses the document, the XML is malformed, or
/// a required field is absent or invalid.
///
/// Covers: RQ-DISC-007, RQ-DISC-008, RQ-DISC-012
pub fn parse(document: &str) -> Result<DeviceDescription, DescriptionError> {
    // The guard runs first, always. A DOCTYPE never reaches the XML reader.
    xml::reject_doctype(document)?;

    let mut reader = Reader::from_str(document);
    // Deliberately *not* `trim_text(true)`. An entity reference splits a value across several
    // Text events, and per-fragment trimming would eat the interior spaces: `Kitchen &amp;
    // Dining` would arrive as `Kitchen&Dining`. The accumulated value is trimmed once, when
    // the element closes.

    let mut raw = Raw::default();
    // Element path depth. The root device's fields sit at depth 3:
    // root(1) > device(2) > roomName(3).
    let mut depth = 0_usize;
    let mut current: Option<String> = None;
    // Once we enter a `deviceList`, everything below belongs to a sub-device.
    let mut in_sub_device = 0_usize;
    // Text is accumulated rather than taken from the first fragment, because an entity
    // reference splits a value into several events: `Kitchen &amp; Dining` arrives as
    // Text("Kitchen ") + GeneralRef("amp") + Text(" Dining"). Keeping only the first piece
    // would silently truncate every name containing an ampersand.
    let mut buffer = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                depth += 1;
                let name = local_name(element.name().as_ref());
                if name.eq_ignore_ascii_case("deviceList") {
                    in_sub_device += 1;
                }
                current = Some(name);
                buffer.clear();
            }
            Ok(Event::End(element)) => {
                let name = local_name(element.name().as_ref());
                // Commit while `depth` still refers to the element that is closing.
                if let Some(field) = current.take() {
                    if in_sub_device == 0 && depth <= 3 {
                        store(&mut raw, &field, buffer.trim().to_owned());
                    }
                }
                buffer.clear();
                if name.eq_ignore_ascii_case("deviceList") {
                    in_sub_device = in_sub_device.saturating_sub(1);
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Text(text)) => {
                let decoded = text
                    .xml10_content()
                    .map_err(|source| DescriptionError::Malformed(source.to_string()))?;
                buffer.push_str(&decoded);
            }
            Ok(Event::GeneralRef(reference)) => {
                // The event carries the reference's *name*, not its value. Resolving it is
                // what stops `Kitchen &amp; Dining` arriving as `Kitchen amp Dining`.
                if let Ok(name) = reference.decode() {
                    if let Some(resolved) = xml::resolve_reference(&name) {
                        buffer.push_str(&resolved);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(source) => return Err(DescriptionError::Malformed(source.to_string())),
            Ok(_) => {}
        }
    }

    finish(raw)
}

/// Strips any namespace prefix and decodes the tag name, lossily — a tag name that is not
/// UTF-8 is malformed anyway, and we only ever compare it against ASCII literals.
fn local_name(raw: &[u8]) -> String {
    let bytes = raw.rsplit(|&b| b == b':').next().unwrap_or(raw);
    String::from_utf8_lossy(bytes).into_owned()
}

/// Records the first non-empty value seen for a field. First wins, so a sub-device that
/// slipped past the depth check still cannot overwrite the root device's identity.
fn store(raw: &mut Raw, field: &str, value: String) {
    if value.trim().is_empty() {
        return;
    }
    let slot = if field.eq_ignore_ascii_case("UDN") {
        &mut raw.udn
    } else if field.eq_ignore_ascii_case("roomName") {
        &mut raw.room
    } else if field.eq_ignore_ascii_case("modelName") {
        &mut raw.model_name
    } else if field.eq_ignore_ascii_case("modelDescription") {
        &mut raw.model_description
    } else if field.eq_ignore_ascii_case("friendlyName") {
        &mut raw.friendly_name
    } else if field.eq_ignore_ascii_case("deviceType") {
        &mut raw.device_type
    } else {
        return;
    };
    if slot.is_none() {
        *slot = Some(value);
    }
}

fn finish(raw: Raw) -> Result<DeviceDescription, DescriptionError> {
    let udn_raw = raw.udn.ok_or(DescriptionError::MissingField("UDN"))?;
    let udn = DeviceId::new(udn_raw).map_err(|source| DescriptionError::InvalidField {
        field: "UDN",
        reason: source.to_string(),
    })?;

    // A device with no `roomName` is unusual but not fatal: the friendly name is what the
    // Sonos app shows for such a device, so it is the right thing to fall back to.
    let room_raw = raw
        .room
        .or_else(|| raw.friendly_name.clone())
        .ok_or(DescriptionError::MissingField("roomName"))?;
    let room = DeviceName::new(room_raw).map_err(|source| DescriptionError::InvalidField {
        field: "roomName",
        reason: source.to_string(),
    })?;

    let model_raw = raw
        .model_name
        .or(raw.model_description)
        .or(raw.friendly_name)
        .unwrap_or_else(|| "Unknown model".to_owned());
    let model = DeviceName::new(model_raw).map_err(|source| DescriptionError::InvalidField {
        field: "modelName",
        reason: source.to_string(),
    })?;

    Ok(DeviceDescription { udn, room, model, device_type: raw.device_type.unwrap_or_default() })
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

    /// A description shaped the way a real Sonos One answers, including the embedded
    /// sub-devices that trip up naive parsers.
    const REAL: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:ZonePlayer:1</deviceType>
    <friendlyName>192.168.1.45 - Sonos One</friendlyName>
    <manufacturer>Sonos, Inc.</manufacturer>
    <modelNumber>S13</modelNumber>
    <modelDescription>Sonos One</modelDescription>
    <modelName>Sonos One</modelName>
    <UDN>uuid:RINCON_949F3EC13E7601400</UDN>
    <roomName>Kitchen</roomName>
    <deviceList>
      <device>
        <deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
        <friendlyName>192.168.1.45 - MediaRenderer</friendlyName>
        <modelName>Sonos MediaRenderer</modelName>
        <UDN>uuid:RINCON_949F3EC13E7601400_MR</UDN>
        <roomName>WRONG ROOM</roomName>
      </device>
    </deviceList>
  </device>
</root>"#;

    #[test]
    fn a_real_description_parses_to_the_root_device() {
        let parsed = parse(REAL).unwrap();
        assert_eq!(parsed.udn.as_str(), "uuid:RINCON_949F3EC13E7601400");
        assert_eq!(parsed.room.as_str(), "Kitchen");
        assert_eq!(parsed.model.as_str(), "Sonos One");
        assert!(parsed.is_zone_player());
    }

    #[test]
    fn embedded_sub_devices_cannot_hijack_the_identity() {
        // This is the nesting trap described in the module docs: the `MediaRenderer` UDN and
        // room name must never win, or transport commands go to a sub-device that ignores them.
        let parsed = parse(REAL).unwrap();
        assert_ne!(parsed.udn.as_str(), "uuid:RINCON_949F3EC13E7601400_MR");
        assert_ne!(parsed.room.as_str(), "WRONG ROOM");
    }

    /// Covers: RQ-DISC-007
    #[test]
    fn rq_disc_007_rejects_doctype() {
        let xxe = r#"<?xml version="1.0"?>
<!DOCTYPE root [<!ENTITY xxe SYSTEM "file:///c:/windows/win.ini">]>
<root><device><UDN>uuid:x</UDN><roomName>&xxe;</roomName></device></root>"#;
        assert!(matches!(parse(xxe), Err(DescriptionError::Guard(XmlGuardError::DoctypeDeclared))));

        let bomb = "<!ENTITY lol 'lol'><root/>";
        assert!(matches!(parse(bomb), Err(DescriptionError::Guard(XmlGuardError::EntityDeclared))));
    }

    /// Covers: RQ-DISC-008
    #[test]
    fn rq_disc_008_malformed_input_never_panics() {
        let hostile = [
            "",
            "<",
            "<root>",
            "</root>",
            "<root><device></root>",
            "<root><device><UDN></UDN></device></root>",
            "<<<<<>>>>>",
            "<root>\u{0}<device/></root>",
            "not xml at all",
            "<root><device><UDN>uuid:x</UDN><roomName>\u{0}\u{1}</roomName></device></root>",
        ];
        for input in hostile {
            // The property is that this returns; any `Err` is an acceptable outcome.
            let _ = parse(input);
        }
    }

    /// Covers: RQ-DISC-012
    #[test]
    fn rq_disc_012_sanitises_room_name() {
        // XML entities are decoded (so the user sees `Kitchen & Dining`, not the raw entity),
        // and the result is then sanitised before it can reach the renderer.
        let doc = r"<root><device><deviceType>urn:schemas-upnp-org:device:ZonePlayer:1</deviceType>
            <UDN>uuid:RINCON_A</UDN>
            <roomName>Kitchen &amp; Dining</roomName>
            <modelName>Sonos One</modelName></device></root>";
        assert_eq!(parse(doc).unwrap().room.as_str(), "Kitchen & Dining");

        // A crafted name cannot carry control characters or line breaks into the interface.
        let hostile = "<root><device><UDN>uuid:RINCON_B</UDN>\
            <roomName>Evil&#10;&#13;&#9;Room</roomName>\
            <modelName>X</modelName></device></root>";
        let room = parse(hostile).unwrap().room;
        assert_eq!(room.as_str(), "Evil Room");
        assert!(!room.as_str().chars().any(char::is_control));
    }

    #[test]
    fn a_missing_udn_is_a_typed_error_not_a_default() {
        let doc = "<root><device><roomName>Kitchen</roomName></device></root>";
        assert_eq!(parse(doc).unwrap_err(), DescriptionError::MissingField("UDN"));
    }

    #[test]
    fn friendly_name_substitutes_for_a_missing_room_name() {
        let doc = "<root><device><UDN>uuid:RINCON_C</UDN>\
            <friendlyName>Study Speaker</friendlyName></device></root>";
        let parsed = parse(doc).unwrap();
        assert_eq!(parsed.room.as_str(), "Study Speaker");
        assert_eq!(parsed.model.as_str(), "Study Speaker", "model falls back too");
    }

    #[test]
    fn namespace_prefixes_do_not_hide_fields() {
        let doc = "<u:root xmlns:u='urn:x'><u:device><u:UDN>uuid:RINCON_D</u:UDN>\
            <u:roomName>Den</u:roomName><u:modelName>Play:1</u:modelName></u:device></u:root>";
        let parsed = parse(doc).unwrap();
        assert_eq!(parsed.udn.as_str(), "uuid:RINCON_D");
        assert_eq!(parsed.room.as_str(), "Den");
    }

    #[test]
    fn an_oversized_document_is_refused_by_the_guard() {
        let huge = format!("<root>{}</root>", "x".repeat(rincon_core::limits::MAX_XML_BYTES));
        assert!(matches!(
            parse(&huge),
            Err(DescriptionError::Guard(XmlGuardError::TooLarge { .. }))
        ));
    }

    #[test]
    fn a_non_sonos_upnp_device_is_parsed_but_identified_as_such() {
        // A printer or a router will answer a broad SSDP probe. Parsing must succeed so the
        // caller can filter, rather than erroring and losing the whole scan.
        let doc = "<root><device><deviceType>urn:schemas-upnp-org:device:Printer:1</deviceType>\
            <UDN>uuid:12345678-1234-1234-1234-123456789abc</UDN>\
            <friendlyName>Office Printer</friendlyName></device></root>";
        let parsed = parse(doc).unwrap();
        assert!(!parsed.is_zone_player(), "a printer must not be offered as a speaker");
    }

    proptest::proptest! {
        /// Arbitrary text never panics the parser.
        #[test]
        fn parsing_arbitrary_text_is_total(raw in ".{0,2000}") {
            let _ = parse(&raw);
        }

        /// Whatever a hostile responder puts in a room name, what comes out is printable,
        /// single-line, and bounded.
        #[test]
        fn room_names_are_always_renderable(name in "[^<>&]{1,300}") {
            let doc = format!(
                "<root><device><UDN>uuid:RINCON_P</UDN><roomName>{name}</roomName></device></root>"
            );
            if let Ok(parsed) = parse(&doc) {
                let room = parsed.room.as_str();
                proptest::prop_assert!(!room.chars().any(char::is_control));
                proptest::prop_assert!(room.chars().count() <= 64);
            }
        }
    }
}
