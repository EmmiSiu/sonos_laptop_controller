//! Validated device identity.
//!
//! Every field here originates from an unauthenticated network response, so each one is a
//! newtype that validates and sanitises on construction. Once a [`Device`] exists, the rest of
//! the program may use it without re-checking anything — the invariant is carried by the type,
//! not by the discipline of the caller.

use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// A device's stable unique identifier, derived from its UPnP UDN (`uuid:RINCON_...`).
///
/// Used instead of an IP address wherever an identity is needed, because DHCP will eventually
/// move the address and because the IPC boundary must never accept a caller-supplied address
/// (see SPEC-006 `RQ-UI-001`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DeviceId(String);

impl DeviceId {
    /// Longest identifier we accept. Real UDNs are ~45 characters.
    pub const MAX_LEN: usize = 128;

    /// Constructs an identifier, rejecting empty, oversized, or non-printable values.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Invalid`] when the identifier violates any of those rules.
    ///
    /// Covers: RQ-CORE-006
    pub fn new(raw: impl Into<String>) -> Result<Self, CoreError> {
        let raw = raw.into();
        let trimmed = raw.trim();

        if trimmed.is_empty() {
            return Err(CoreError::invalid("device_id", "must not be empty"));
        }
        if trimmed.len() > Self::MAX_LEN {
            return Err(CoreError::invalid(
                "device_id",
                format!("{} bytes exceeds the {} byte limit", trimmed.len(), Self::MAX_LEN),
            ));
        }
        if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(CoreError::invalid(
                "device_id",
                "must not contain control or whitespace characters",
            ));
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// The identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DeviceId {
    type Error = CoreError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<DeviceId> for String {
    fn from(value: DeviceId) -> Self {
        value.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A human-readable name that will be rendered in a WebView.
///
/// Sanitised on construction: control characters are stripped, whitespace is collapsed, and the
/// result is truncated on a character boundary. The renderer still treats it as text
/// (SPEC-006 `RQ-UI-005`) — this is defence in depth, not the only defence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DeviceName(String);

impl DeviceName {
    /// Longest name we display. Longer names are truncated with an ellipsis.
    pub const MAX_LEN: usize = 64;

    /// Sanitises and truncates a name received from the network.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Invalid`] only when nothing printable remains.
    ///
    /// Covers: RQ-DISC-012, RQ-UI-011
    pub fn new(raw: impl AsRef<str>) -> Result<Self, CoreError> {
        let cleaned: String = raw
            .as_ref()
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        if cleaned.is_empty() {
            return Err(CoreError::invalid("device_name", "contains nothing printable"));
        }

        Ok(Self(truncate_on_char_boundary(&cleaned, Self::MAX_LEN)))
    }

    /// The sanitised name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DeviceName {
    type Error = CoreError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<DeviceName> for String {
    fn from(value: DeviceName) -> Self {
        value.0
    }
}

impl fmt::Display for DeviceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Truncates to at most `max` characters, appending `…` when anything was removed.
fn truncate_on_char_boundary(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A discovered Sonos player.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// Stable identity derived from the UDN.
    pub id: DeviceId,
    /// The room the user knows this speaker as, e.g. "Kitchen".
    pub room: DeviceName,
    /// Model string as reported by the device, e.g. "Sonos One".
    pub model: DeviceName,
    /// Current address. May change between scans.
    pub address: IpAddr,
    /// Port serving the description and control endpoints. Sonos uses 1400.
    pub port: u16,
    /// The local address that successfully reached this device during discovery.
    ///
    /// The stream server must bind here: on a laptop with Wi-Fi, Ethernet, a VPN, and Hyper-V
    /// virtual switches, binding to the wrong interface produces a URL the speaker cannot
    /// route back to, which presents as a firewall problem and wastes the user's afternoon.
    pub reached_via: Option<IpAddr>,
}

impl Device {
    /// Whether this device is currently believed to be a member of a group it does not lead.
    ///
    /// Populated by topology resolution in `rincon-control`; absent means "not yet known".
    #[must_use]
    pub fn describe(&self) -> String {
        format!("{} ({})", self.room, self.model)
    }
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}] at {}:{}", self.room, self.model, self.address, self.port)
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

    /// Covers: RQ-CORE-006
    #[test]
    fn rq_core_006_device_id_is_validated() {
        assert!(DeviceId::new("").is_err(), "empty id must be refused");
        assert!(DeviceId::new("   ").is_err(), "whitespace-only id must be refused");
        assert!(DeviceId::new("a".repeat(129)).is_err(), "oversized id must be refused");
        assert!(DeviceId::new("RINCON_949F3E\u{0}1400").is_err(), "NUL must be refused");
        assert!(DeviceId::new("has space").is_err(), "internal whitespace must be refused");

        let ok = DeviceId::new("  uuid:RINCON_949F3EC13E7601400  ").unwrap();
        assert_eq!(ok.as_str(), "uuid:RINCON_949F3EC13E7601400", "surrounding space is trimmed");
        assert_eq!(DeviceId::new("a".repeat(128)).unwrap().as_str().len(), 128, "boundary is ok");
    }

    /// Covers: RQ-UI-011
    #[test]
    fn rq_ui_011_names_are_sanitised_and_truncated() {
        let injected = DeviceName::new("Kitchen\u{0}\u{7}\n\tSpeaker").unwrap();
        assert_eq!(injected.as_str(), "Kitchen Speaker", "control chars become collapsed space");

        let long = DeviceName::new("x".repeat(200)).unwrap();
        assert_eq!(long.as_str().chars().count(), 64, "truncated to the display limit");
        assert!(long.as_str().ends_with('…'), "truncation is visible to the user");

        assert!(DeviceName::new("\u{0}\u{1}\u{2}").is_err(), "nothing printable is an error");
    }

    /// Covers: RQ-DISC-012
    #[test]
    fn rq_disc_012_sanitises_room_name() {
        // A hostile responder cannot smuggle markup or layout through a room name. The angle
        // brackets survive as text -- the renderer escapes them -- but the control characters
        // and line breaks that would let it fake UI structure do not.
        let hostile = DeviceName::new("Kitchen</div><script>alert(1)</script>").unwrap();
        assert!(!hostile.as_str().contains('\n'));
        assert!(!hostile.as_str().chars().any(char::is_control));

        let multiline = DeviceName::new("Living\r\n\r\nRoom").unwrap();
        assert_eq!(multiline.as_str(), "Living Room");
    }

    #[test]
    fn truncation_respects_multibyte_boundaries() {
        let emoji = DeviceName::new("🎵".repeat(100)).unwrap();
        assert_eq!(emoji.as_str().chars().count(), 64);
        // Would have panicked on a byte-index slice.
        assert!(emoji.as_str().is_char_boundary(emoji.as_str().len()));
    }

    #[test]
    fn device_ids_round_trip_through_serde_and_keep_validating() {
        let id = DeviceId::new("uuid:RINCON_TEST").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<DeviceId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<DeviceId>(r#""""#).is_err(), "invariant survives serde");
    }

    #[test]
    fn display_is_useful_in_logs() {
        let d = Device {
            id: DeviceId::new("uuid:RINCON_1").unwrap(),
            room: DeviceName::new("Kitchen").unwrap(),
            model: DeviceName::new("Sonos One").unwrap(),
            address: "192.168.1.45".parse().unwrap(),
            port: 1400,
            reached_via: Some("192.168.1.20".parse().unwrap()),
        };
        assert_eq!(d.to_string(), "Kitchen [Sonos One] at 192.168.1.45:1400");
        assert_eq!(d.describe(), "Kitchen (Sonos One)");
    }
}
