//! The control error vocabulary.
//!
//! A UPnP device answers failures with a numeric code. Collapsing those into one
//! `ControlError::Failed(u16)` would technically be typed and practically useless: the
//! interface has to say something specific, and "the speaker returned error 714" is not
//! something a user can act on.
//!
//! So the codes Rincon can actually encounter each get a variant, a user-facing sentence, and
//! a retry classification. Unknown codes keep their number, because inventing a friendly
//! message for a code we have never seen would be worse than admitting we do not know.

use rincon_core::error::UserFacing;
use rincon_core::net::NetError;

use crate::soap::SoapError;
use crate::topology::TopologyError;

/// UPnP: the device cannot make the requested transition right now.
pub const UPNP_TRANSITION_NOT_AVAILABLE: u16 = 701;

/// UPnP: the device refused the media's MIME type.
pub const UPNP_ILLEGAL_MIME_TYPE: u16 = 714;

/// UPnP: the resource could not be found by the device.
pub const UPNP_RESOURCE_NOT_FOUND: u16 = 716;

/// UPnP: the action is not implemented by this service.
pub const UPNP_INVALID_ACTION: u16 = 401;

/// Why a control command did not take effect.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    /// The speaker is busy with another source and will not switch right now.
    #[error("the speaker cannot change what it is playing right now (UPnP 701)")]
    TransitionNotAvailable,

    /// The speaker rejected our audio format.
    #[error("the speaker rejected the audio format (UPnP 714)")]
    FormatRejected,

    /// The speaker could not fetch the URL we gave it.
    #[error("the speaker could not reach the stream URL (UPnP 716)")]
    StreamUnreachable,

    /// The speaker does not implement this action.
    #[error("the speaker does not support this command (UPnP 401)")]
    UnsupportedAction,

    /// Some other UPnP fault.
    #[error("the speaker returned UPnP error {code}: {description}")]
    Upnp {
        /// The raw code.
        code: u16,
        /// The device's description.
        description: String,
    },

    /// The device did not answer in time or refused the connection.
    #[error("the speaker is unreachable: {detail}")]
    Unreachable {
        /// Transport-level detail, for the log.
        detail: String,
    },

    /// The response was too large, malformed, or refused by a guard.
    #[error("the speaker sent an unusable response: {0}")]
    BadResponse(String),

    /// The target failed the private-address guard.
    #[error(transparent)]
    Refused(#[from] NetError),

    /// A response was missing a value the caller needs.
    #[error("the speaker's response did not include `{0}`")]
    MissingValue(&'static str),
}

impl ControlError {
    /// Maps a raw UPnP fault onto the most specific variant available.
    ///
    /// Covers: RQ-CTL-007
    #[must_use]
    pub fn from_upnp(code: u16, description: String) -> Self {
        match code {
            UPNP_TRANSITION_NOT_AVAILABLE => Self::TransitionNotAvailable,
            UPNP_ILLEGAL_MIME_TYPE => Self::FormatRejected,
            UPNP_RESOURCE_NOT_FOUND => Self::StreamUnreachable,
            UPNP_INVALID_ACTION => Self::UnsupportedAction,
            other => Self::Upnp { code: other, description },
        }
    }

    /// Whether retrying the same command could plausibly succeed.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            // Retryable for two different reasons, which the comments record even though the
            // answer is the same: the first pair can change on their own (the user stops the
            // other source, the network recovers), while the second pair is worth one attempt
            // with an alternate framing before the engine gives up.
            Self::TransitionNotAvailable
            | Self::Unreachable { .. }
            | Self::FormatRejected
            | Self::StreamUnreachable => true,
            // These will not change on a retry.
            Self::UnsupportedAction
            | Self::Upnp { .. }
            | Self::BadResponse(_)
            | Self::Refused(_)
            | Self::MissingValue(_) => false,
        }
    }

    /// What the user should be told, and what they can do about it.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::TransitionNotAvailable => "Stop what the speaker is playing, then try again.",
            Self::FormatRejected => "This speaker refused the audio format. Report this with a diagnostics bundle.",
            Self::StreamUnreachable => "The speaker could not reach your computer. Check the Windows Firewall rule.",
            Self::UnsupportedAction => "This device does not support being streamed to.",
            Self::Upnp { .. } => "The speaker refused the command. Try again, or restart the speaker.",
            Self::Unreachable { .. } => "The speaker is not responding. Rescan the network.",
            Self::BadResponse(_) | Self::MissingValue(_) => "The speaker sent something unexpected. Report this with a diagnostics bundle.",
            Self::Refused(_) => "That address is not on your local network.",
        }
    }
}

impl UserFacing for ControlError {
    fn retryable(&self) -> bool {
        Self::retryable(self)
    }
}

impl From<SoapError> for ControlError {
    fn from(source: SoapError) -> Self {
        match source {
            SoapError::Fault { code, description } => Self::from_upnp(code, description),
            other => Self::BadResponse(other.to_string()),
        }
    }
}

impl From<TopologyError> for ControlError {
    fn from(source: TopologyError) -> Self {
        Self::BadResponse(source.to_string())
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

    /// Covers: RQ-CTL-007
    #[test]
    fn rq_ctl_007_maps_known_upnp_codes() {
        assert_eq!(
            ControlError::from_upnp(701, "Transition not available".into()),
            ControlError::TransitionNotAvailable
        );
        assert_eq!(
            ControlError::from_upnp(714, "Illegal MIME-Type".into()),
            ControlError::FormatRejected
        );
        assert_eq!(
            ControlError::from_upnp(716, "Resource not found".into()),
            ControlError::StreamUnreachable
        );
        assert_eq!(
            ControlError::from_upnp(401, "Invalid Action".into()),
            ControlError::UnsupportedAction
        );

        // The two codes the spec singles out must be *distinct*, because they mean entirely
        // different things to the user: one is "stop Spotify", the other is "file a bug".
        assert_ne!(
            ControlError::from_upnp(701, String::new()),
            ControlError::from_upnp(714, String::new())
        );
        assert_ne!(
            ControlError::TransitionNotAvailable.remediation(),
            ControlError::FormatRejected.remediation()
        );
    }

    #[test]
    fn an_unknown_code_keeps_its_number_rather_than_inventing_a_message() {
        let unknown = ControlError::from_upnp(799, "Something specific".into());
        assert_eq!(
            unknown,
            ControlError::Upnp { code: 799, description: "Something specific".into() }
        );
        assert!(unknown.to_string().contains("799"), "the code must survive for a bug report");
    }

    #[test]
    fn every_variant_offers_an_action_the_user_can_take() {
        let variants = [
            ControlError::TransitionNotAvailable,
            ControlError::FormatRejected,
            ControlError::StreamUnreachable,
            ControlError::UnsupportedAction,
            ControlError::Upnp { code: 500, description: "x".into() },
            ControlError::Unreachable { detail: "timeout".into() },
            ControlError::BadResponse("garbage".into()),
            ControlError::MissingValue("CurrentVolume"),
        ];
        for variant in variants {
            let advice = variant.remediation();
            assert!(!advice.is_empty(), "{variant:?} has no remediation");
            assert!(advice.ends_with('.'), "{variant:?} remediation is not a sentence");
            // The message the user sees must never be the raw debug form.
            assert!(!advice.contains("ControlError"));
        }
    }

    #[test]
    fn retry_classification_matches_what_can_actually_change() {
        assert!(ControlError::TransitionNotAvailable.retryable(), "the user can stop Spotify");
        assert!(ControlError::Unreachable { detail: String::new() }.retryable());
        assert!(!ControlError::UnsupportedAction.retryable(), "a printer will not become a speaker");
        assert!(!ControlError::BadResponse(String::new()).retryable());
    }

    #[test]
    fn a_soap_fault_converts_into_the_specific_variant() {
        let fault = SoapError::Fault { code: 701, description: "Transition not available".into() };
        assert_eq!(ControlError::from(fault), ControlError::TransitionNotAvailable);

        let malformed = SoapError::Malformed("bad xml".into());
        assert!(matches!(ControlError::from(malformed), ControlError::BadResponse(_)));
    }

    #[test]
    fn user_messages_carry_no_internal_addresses_or_paths() {
        // `UserFacing::user_message` applies the core redaction, so a detail string that
        // picked up an address on the way through cannot reach a dialog.
        let err = ControlError::Unreachable { detail: "connect to 192.168.1.45 failed".into() };
        assert!(!err.user_message().contains("192.168.1.45"), "{}", err.user_message());
        assert!(err.user_message().contains("192.168.1.xxx"));
    }
}
