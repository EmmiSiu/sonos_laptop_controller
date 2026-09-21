//! DIDL-Lite metadata: an XML document that travels inside an XML document.
//!
//! `SetAVTransportURI` takes a `CurrentURIMetaData` argument whose value is itself a complete
//! DIDL-Lite XML document. Because it is an argument *value*, it must be escaped when placed
//! into the SOAP envelope — so every `<` in the metadata becomes `&lt;`, and every `&` inside
//! a title that was already escaped becomes `&amp;amp;`.
//!
//! That double escaping is the part implementations get wrong, and getting it wrong produces
//! a speaker that accepts the command and then plays nothing, with no error. Hence
//! [`RQ-CTL-004`](../../../specs/SPEC-004-control.md) and a golden fixture.
//!
//! The escaping itself is not done here: this module produces the raw DIDL document, and
//! [`crate::soap::envelope`] escapes it exactly once as it interpolates it. One escape applied
//! in one place beats two half-escapes applied in two.

use crate::soap::escape;

/// The Sonos content-directory token that marks a stream as coming from a line-in-like source.
///
/// This exact value is what makes a player accept an arbitrary HTTP stream URI rather than
/// expecting a music-service account behind it.
const CDUDN: &str = "SA_RINCON65031_";

/// Metadata shown on the speaker and in the Sonos app while Rincon is streaming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackMeta {
    /// What the Sonos app displays as the title.
    pub title: String,
    /// What it displays as the artist or source line.
    pub creator: String,
}

impl Default for TrackMeta {
    fn default() -> Self {
        Self { title: "Computer Audio".to_owned(), creator: "Rincon".to_owned() }
    }
}

impl TrackMeta {
    /// Metadata with a custom title, keeping the default source line.
    #[must_use]
    pub fn titled(title: impl Into<String>) -> Self {
        Self { title: title.into(), ..Self::default() }
    }

    /// Renders the raw DIDL-Lite document.
    ///
    /// The result is **unescaped XML**. It is escaped exactly once, by the envelope builder,
    /// when it becomes the value of `CurrentURIMetaData`.
    ///
    /// Covers: RQ-CTL-004
    #[must_use]
    pub fn to_didl(&self) -> String {
        format!(
            concat!(
                r#"<DIDL-Lite xmlns:dc="http://purl.org/dc/elements/1.1/" "#,
                r#"xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/" "#,
                r#"xmlns:r="urn:schemas-rinconnetworks-com:metadata-1-0/" "#,
                r#"xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/">"#,
                r#"<item id="-1" parentID="-1" restricted="true">"#,
                "<dc:title>{title}</dc:title>",
                "<dc:creator>{creator}</dc:creator>",
                "<upnp:class>object.item.audioItem.audioBroadcast</upnp:class>",
                r#"<desc id="cdudn" nameSpace="urn:schemas-rinconnetworks-com:metadata-1-0/">"#,
                "{cdudn}</desc>",
                "</item></DIDL-Lite>",
            ),
            // Escaped once here, because these values sit inside the DIDL document. The
            // envelope escapes the whole document again on the way out, which is the second
            // level the spec calls for.
            title = escape(&self.title),
            creator = escape(&self.creator),
            cdudn = CDUDN,
        )
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

    use crate::soap::{Service, envelope};

    use super::*;

    /// Covers: RQ-CTL-004
    #[test]
    fn rq_ctl_004_didl_is_double_escaped() {
        let meta = TrackMeta::default();
        let didl = meta.to_didl();

        // Level one: the DIDL document itself is real, unescaped XML.
        assert!(didl.starts_with("<DIDL-Lite "), "the DIDL document must be raw XML");
        assert!(didl.contains("<dc:title>Computer Audio</dc:title>"));
        assert!(didl.contains(CDUDN), "the content-directory token is what makes Sonos accept us");

        // Level two: inside the envelope it must appear fully escaped, with no raw `<`.
        let built = envelope(
            Service::AvTransport,
            "SetAVTransportURI",
            &[
                ("InstanceID", "0"),
                ("CurrentURI", "http://192.168.1.20:41234/s/tok/stream.wav"),
                ("CurrentURIMetaData", &didl),
            ],
        );

        assert!(built.contains("&lt;DIDL-Lite "), "the DIDL must be escaped inside the envelope");
        assert!(
            !built.contains("<DIDL-Lite "),
            "raw DIDL markup inside the envelope would break the request: {built}"
        );
        // The metadata element itself is still a single, well-formed element.
        assert_eq!(built.matches("<CurrentURIMetaData>").count(), 1);
        assert_eq!(built.matches("</CurrentURIMetaData>").count(), 1);
    }

    #[test]
    fn an_ampersand_in_the_title_survives_both_levels() {
        // `&` -> `&amp;` in the DIDL -> `&amp;amp;` in the envelope. Getting this wrong gives a
        // speaker that accepts the command and silently plays nothing.
        let meta = TrackMeta::titled("Rock & Roll");
        let didl = meta.to_didl();
        assert!(didl.contains("<dc:title>Rock &amp; Roll</dc:title>"));

        let built = envelope(Service::AvTransport, "X", &[("CurrentURIMetaData", &didl)]);
        assert!(built.contains("Rock &amp;amp; Roll"), "second escape missing in: {built}");
    }

    #[test]
    fn a_hostile_title_cannot_break_out_of_the_didl() {
        let meta = TrackMeta::titled("</dc:title></item><item id=\"evil\">");
        let didl = meta.to_didl();
        assert_eq!(didl.matches("<dc:title>").count(), 1);
        assert_eq!(didl.matches("</dc:title>").count(), 1);
        assert_eq!(didl.matches("<item ").count(), 1, "a second item element was injected");
    }

    #[test]
    fn the_default_metadata_is_what_a_user_would_want_to_see() {
        let meta = TrackMeta::default();
        assert_eq!(meta.title, "Computer Audio");
        assert_eq!(meta.creator, "Rincon");
        assert!(meta.to_didl().contains("object.item.audioItem.audioBroadcast"));
    }

    proptest::proptest! {
        /// No title, however hostile, adds or removes an element in the DIDL document.
        #[test]
        fn didl_structure_survives_any_title(title in ".{0,150}") {
            let didl = TrackMeta::titled(title).to_didl();
            proptest::prop_assert_eq!(didl.matches("<item ").count(), 1);
            proptest::prop_assert_eq!(didl.matches("</item>").count(), 1);
            proptest::prop_assert_eq!(didl.matches("<DIDL-Lite ").count(), 1);
            proptest::prop_assert_eq!(didl.matches("</DIDL-Lite>").count(), 1);
            proptest::prop_assert_eq!(didl.matches("<upnp:class>").count(), 1);
        }
    }
}
