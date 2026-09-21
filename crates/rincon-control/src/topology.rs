//! Zone-group topology: finding the speaker that will actually obey a transport command.
//!
//! # Why this module exists
//!
//! If the user has grouped Kitchen with Dining in the Sonos app, one of them is the
//! **coordinator** and the other is a member. `Play` sent to a member is ignored — sometimes
//! silently, sometimes with a `UPnP 701`. To the user this is indistinguishable from "the app
//! is broken", and it is the single most likely way a first-time session fails on a real
//! household that uses grouping.
//!
//! So before any transport command, Rincon asks any player for the topology and retargets the
//! coordinator. Standalone speakers are their own coordinator, so the same code path covers
//! both cases with no special-casing at the call site.

use std::collections::BTreeMap;

use quick_xml::Reader;
use quick_xml::events::Event;
use rincon_core::xml::{self, XmlGuardError};

/// Why topology could not be determined.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TopologyError {
    /// The document was refused before parsing.
    #[error(transparent)]
    Guard(#[from] XmlGuardError),

    /// The XML was not well-formed.
    #[error("malformed topology document: {0}")]
    Malformed(String),
}

/// One speaker's place in the group structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Membership {
    /// The UUID of the coordinator that accepts transport commands for this speaker.
    pub coordinator: String,
    /// The zone name as the app shows it.
    pub zone_name: String,
}

/// The parsed group structure: every known member mapped to its coordinator.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Topology {
    members: BTreeMap<String, Membership>,
}

impl Topology {
    /// Resolves the coordinator for `uuid`.
    ///
    /// Returns `uuid` itself when the speaker is standalone or unknown: an unknown speaker is
    /// far more likely to be standalone than grouped, and targeting itself is the behaviour
    /// that degrades gracefully.
    ///
    /// Covers: RQ-CTL-008
    #[must_use]
    pub fn coordinator_of<'a>(&'a self, uuid: &'a str) -> &'a str {
        self.members.get(uuid).map_or(uuid, |member| member.coordinator.as_str())
    }

    /// Whether `uuid` leads its own group.
    #[must_use]
    pub fn is_coordinator(&self, uuid: &str) -> bool {
        self.coordinator_of(uuid) == uuid
    }

    /// Whether `uuid` is grouped under a different speaker.
    #[must_use]
    pub fn is_grouped_member(&self, uuid: &str) -> bool {
        self.members.contains_key(uuid) && !self.is_coordinator(uuid)
    }

    /// Every known member UUID.
    #[must_use]
    pub fn member_uuids(&self) -> Vec<&str> {
        self.members.keys().map(String::as_str).collect()
    }

    /// The zone name recorded for `uuid`, if the topology named it.
    #[must_use]
    pub fn zone_name_of(&self, uuid: &str) -> Option<&str> {
        self.members.get(uuid).map(|member| member.zone_name.as_str())
    }

    /// How many groups the household has.
    #[must_use]
    pub fn group_count(&self) -> usize {
        let mut coordinators: Vec<&str> =
            self.members.values().map(|m| m.coordinator.as_str()).collect();
        coordinators.sort_unstable();
        coordinators.dedup();
        coordinators.len()
    }
}

/// Parses a `ZoneGroupState` document.
///
/// The document is attribute-driven rather than element-driven, which is why this parser reads
/// attributes instead of text nodes like the others in the workspace.
///
/// # Errors
///
/// Returns [`TopologyError`] when the guard refuses the document or the XML is malformed.
///
/// Covers: RQ-CTL-008, RQ-CTL-013
pub fn parse(document: &str) -> Result<Topology, TopologyError> {
    xml::reject_doctype(document)?;

    let mut reader = Reader::from_str(document);
    reader.config_mut().trim_text(true);

    let mut topology = Topology::default();
    let mut current_coordinator: Option<String> = None;

    loop {
        let event = reader.read_event();
        match event {
            Ok(Event::Start(element) | Event::Empty(element)) => {
                let name = local_name(element.name().as_ref());
                let attributes = collect_attributes(&element);

                if name.eq_ignore_ascii_case("ZoneGroup") {
                    current_coordinator = attributes.get("Coordinator").cloned();
                } else if name.eq_ignore_ascii_case("ZoneGroupMember") {
                    let Some(uuid) = attributes.get("UUID").cloned() else { continue };
                    // A member outside any group is its own coordinator; that is the correct
                    // reading of a malformed document, not a reason to discard it.
                    let coordinator = current_coordinator.clone().unwrap_or_else(|| uuid.clone());
                    let zone_name =
                        attributes.get("ZoneName").cloned().unwrap_or_else(|| uuid.clone());
                    topology.members.insert(uuid, Membership { coordinator, zone_name });
                }
            }
            Ok(Event::End(element)) => {
                if local_name(element.name().as_ref()).eq_ignore_ascii_case("ZoneGroup") {
                    current_coordinator = None;
                }
            }
            Ok(Event::Eof) => break,
            Err(source) => return Err(TopologyError::Malformed(source.to_string())),
            Ok(_) => {}
        }
    }

    Ok(topology)
}

fn collect_attributes(element: &quick_xml::events::BytesStart<'_>) -> BTreeMap<String, String> {
    element
        .attributes()
        .filter_map(Result::ok)
        .filter_map(|attribute| {
            let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
            let value = attribute.unescape_value().ok()?.into_owned();
            Some((key, value))
        })
        .collect()
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

    /// A household with Kitchen+Dining grouped under Kitchen, and a standalone Study.
    const GROUPED: &str = r#"<ZoneGroupState><ZoneGroups>
  <ZoneGroup Coordinator="RINCON_KITCHEN" ID="RINCON_KITCHEN:2">
    <ZoneGroupMember UUID="RINCON_KITCHEN" ZoneName="Kitchen" Location="http://192.168.1.45:1400/xml/device_description.xml"/>
    <ZoneGroupMember UUID="RINCON_DINING" ZoneName="Dining Room" Location="http://192.168.1.46:1400/xml/device_description.xml"/>
  </ZoneGroup>
  <ZoneGroup Coordinator="RINCON_STUDY" ID="RINCON_STUDY:7">
    <ZoneGroupMember UUID="RINCON_STUDY" ZoneName="Study" Location="http://192.168.1.51:1400/xml/device_description.xml"/>
  </ZoneGroup>
</ZoneGroups></ZoneGroupState>"#;

    /// Covers: RQ-CTL-008
    #[test]
    fn rq_ctl_008_resolves_group_coordinator() {
        let topology = parse(GROUPED).unwrap();

        // The grouped member retargets its coordinator -- the whole point of this module.
        assert_eq!(topology.coordinator_of("RINCON_DINING"), "RINCON_KITCHEN");
        assert!(topology.is_grouped_member("RINCON_DINING"));
        assert!(!topology.is_coordinator("RINCON_DINING"));

        // The coordinator resolves to itself.
        assert_eq!(topology.coordinator_of("RINCON_KITCHEN"), "RINCON_KITCHEN");
        assert!(topology.is_coordinator("RINCON_KITCHEN"));

        // A standalone speaker is its own coordinator, with no special case at the call site.
        assert_eq!(topology.coordinator_of("RINCON_STUDY"), "RINCON_STUDY");
        assert!(topology.is_coordinator("RINCON_STUDY"));
        assert!(!topology.is_grouped_member("RINCON_STUDY"));

        assert_eq!(topology.group_count(), 2);
    }

    #[test]
    fn an_unknown_speaker_targets_itself_rather_than_failing() {
        // Degrading to "send the command to the speaker the user picked" is always better than
        // refusing to play because a topology query was stale.
        let topology = parse(GROUPED).unwrap();
        assert_eq!(topology.coordinator_of("RINCON_NEVER_SEEN"), "RINCON_NEVER_SEEN");
        assert!(topology.is_coordinator("RINCON_NEVER_SEEN"));
        assert!(!topology.is_grouped_member("RINCON_NEVER_SEEN"));
    }

    #[test]
    fn zone_names_are_recorded_for_the_interface() {
        let topology = parse(GROUPED).unwrap();
        assert_eq!(topology.zone_name_of("RINCON_DINING"), Some("Dining Room"));
        assert_eq!(topology.zone_name_of("RINCON_ABSENT"), None);
        assert_eq!(topology.member_uuids().len(), 3);
    }

    /// Covers: RQ-CTL-013
    #[test]
    fn rq_ctl_013_rejects_doctype() {
        let hostile = "<!DOCTYPE z [<!ENTITY e SYSTEM 'file:///etc/passwd'>]><ZoneGroupState/>";
        assert!(matches!(parse(hostile), Err(TopologyError::Guard(XmlGuardError::DoctypeDeclared))));
    }

    #[test]
    fn an_empty_topology_is_valid_and_targets_everything_at_itself() {
        let topology = parse("<ZoneGroupState><ZoneGroups/></ZoneGroupState>").unwrap();
        assert_eq!(topology.coordinator_of("RINCON_X"), "RINCON_X");
        assert_eq!(topology.group_count(), 0);
    }

    #[test]
    fn malformed_topology_never_panics() {
        for document in [
            "",
            "<ZoneGroupState>",
            "<ZoneGroup Coordinator=/>",
            "<ZoneGroupMember UUID=\"x\"/>",
            "<ZoneGroups><ZoneGroup><ZoneGroupMember/></ZoneGroup></ZoneGroups>",
            "\u{0}\u{1}",
        ] {
            let _ = parse(document);
        }
    }

    #[test]
    fn a_member_outside_any_group_coordinates_itself() {
        // Defensive reading of a malformed document: better than dropping the member.
        let orphan = "<ZoneGroups><ZoneGroupMember UUID=\"RINCON_LONE\" ZoneName=\"Lone\"/></ZoneGroups>";
        let topology = parse(orphan).unwrap();
        assert_eq!(topology.coordinator_of("RINCON_LONE"), "RINCON_LONE");
    }

    #[test]
    fn escaped_zone_names_are_decoded() {
        let doc = r#"<ZoneGroups><ZoneGroup Coordinator="RINCON_A">
            <ZoneGroupMember UUID="RINCON_A" ZoneName="Kitchen &amp; Dining"/>
        </ZoneGroup></ZoneGroups>"#;
        assert_eq!(parse(doc).unwrap().zone_name_of("RINCON_A"), Some("Kitchen & Dining"));
    }
}
