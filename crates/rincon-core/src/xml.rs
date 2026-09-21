//! The guard every XML document passes before a parser sees it.
//!
//! Rincon parses XML from three untrusted sources: device descriptions, SOAP responses, and
//! zone-group topology. All three arrive from whoever answered on the LAN.
//!
//! The classic attacks against an XML parser — XXE file disclosure, SSRF via external entities,
//! and the billion-laughs expansion bomb — all require a `DOCTYPE` or an `ENTITY` declaration.
//! Rather than rely on each parser being configured safely (a property that silently regresses
//! when a dependency is upgraded), Rincon refuses any document that contains one at all.
//!
//! No legitimate UPnP document needs a DOCTYPE, so the false-positive rate is zero.

use crate::limits;

/// A document was refused before parsing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XmlGuardError {
    /// The document declared a DTD.
    #[error("XML document contains a DOCTYPE declaration; refusing to parse")]
    DoctypeDeclared,

    /// The document declared an entity.
    #[error("XML document contains an ENTITY declaration; refusing to parse")]
    EntityDeclared,

    /// The document exceeded the configured size budget.
    #[error("XML document is {actual} bytes, over the {limit} byte limit")]
    TooLarge {
        /// Size of the offending document.
        actual: usize,
        /// The configured ceiling.
        limit: usize,
    },
}

/// Refuses documents containing a DTD, an entity declaration, or more bytes than allowed.
///
/// The scan is a case-insensitive search for the literal markup openers. XML requires `<!` to be
/// immediately followed by the keyword with no intervening whitespace, so there is no variant
/// spelling for an attacker to hide behind.
///
/// # Errors
///
/// Returns [`XmlGuardError`] describing the first violation found.
///
/// Covers: RQ-DISC-007, RQ-SEC-002, RQ-CTL-013
pub fn reject_doctype(document: &str) -> Result<(), XmlGuardError> {
    reject_doctype_with_limit(document, limits::MAX_XML_BYTES)
}

/// [`reject_doctype`] with an explicit size budget, for callers with a tighter ceiling.
///
/// # Errors
///
/// Returns [`XmlGuardError`] describing the first violation found.
pub fn reject_doctype_with_limit(document: &str, limit: usize) -> Result<(), XmlGuardError> {
    if document.len() > limit {
        return Err(XmlGuardError::TooLarge { actual: document.len(), limit });
    }

    // Case-folding the whole document once beats scanning it three times case-insensitively,
    // and these documents are already capped at a few hundred KiB.
    let lowered = document.to_ascii_lowercase();
    if lowered.contains("<!doctype") {
        return Err(XmlGuardError::DoctypeDeclared);
    }
    if lowered.contains("<!entity") {
        return Err(XmlGuardError::EntityDeclared);
    }
    Ok(())
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

    const XXE: &str = r#"<?xml version="1.0"?>
<!DOCTYPE root [<!ENTITY xxe SYSTEM "file:///c:/windows/win.ini">]>
<root>&xxe;</root>"#;

    const BILLION_LAUGHS: &str = r#"<?xml version="1.0"?>
<!DOCTYPE lolz [
 <!ENTITY lol "lol">
 <!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">
]>
<lolz>&lol2;</lolz>"#;

    const LEGITIMATE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <device><roomName>Kitchen</roomName></device>
</root>"#;

    /// Covers: RQ-DISC-007, RQ-SEC-002, RQ-CTL-013
    #[test]
    fn rq_sec_002_doctype_is_rejected_everywhere() {
        assert_eq!(reject_doctype(XXE).unwrap_err(), XmlGuardError::DoctypeDeclared);
        assert_eq!(reject_doctype(BILLION_LAUGHS).unwrap_err(), XmlGuardError::DoctypeDeclared);
        reject_doctype(LEGITIMATE).expect("a real UPnP document must pass");
    }

    #[test]
    fn case_and_spacing_variants_do_not_evade_the_guard() {
        for evasion in [
            "<!doctype html>",
            "<!DocType x>",
            "<!DOCTYPE\n  root SYSTEM 'x'>",
            "  \n\t<!DOCTYPE root>",
            "<?xml version='1.0'?><!-- comment --><!DOCTYPE root>",
        ] {
            assert!(reject_doctype(evasion).is_err(), "must reject: {evasion}");
        }
    }

    #[test]
    fn a_bare_entity_declaration_is_refused_even_without_a_doctype() {
        assert_eq!(
            reject_doctype("<!ENTITY x 'y'><r/>").unwrap_err(),
            XmlGuardError::EntityDeclared
        );
    }

    #[test]
    fn oversized_documents_are_refused_before_scanning() {
        let big = "x".repeat(limits::MAX_XML_BYTES + 1);
        assert!(matches!(reject_doctype(&big), Err(XmlGuardError::TooLarge { .. })));
        // Exactly at the limit is allowed: the check is `>`, not `>=`.
        let exact = "x".repeat(limits::MAX_XML_BYTES);
        assert!(reject_doctype(&exact).is_ok());
    }

    #[test]
    fn predefined_entity_references_in_content_remain_legal() {
        // `&amp;` is a *reference*, not a declaration. Refusing it would break every room
        // name containing an ampersand, which is a real thing users do.
        let doc = "<root><roomName>Kitchen &amp; Dining</roomName></root>";
        reject_doctype(doc).expect("entity references must stay legal");
    }

    #[test]
    fn empty_input_is_not_an_error_here() {
        // Emptiness is the parser's problem to report, not the guard's; the guard only
        // answers "is this safe to hand to a parser".
        assert!(reject_doctype("").is_ok());
    }
}
