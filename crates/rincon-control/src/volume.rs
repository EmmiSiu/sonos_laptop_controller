//! Volume as a type that cannot hold an invalid value.
//!
//! A `u8` volume is out of range for 155 of its 256 values, and every function that accepts one
//! has to decide what to do about that. A newtype that clamps at construction moves the decision
//! to one place and removes the question from every call site — including the IPC boundary,
//! where the value arrives from a WebView.

use std::fmt;
use std::str::FromStr;

use rincon_core::error::CoreError;
use serde::{Deserialize, Serialize};

/// A speaker volume, always in `0..=100`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Volume(u8);

impl Volume {
    /// Silence.
    pub const MUTED: Self = Self(0);
    /// Full scale.
    pub const MAX: Self = Self(100);

    /// Constructs a volume, rejecting anything above 100.
    ///
    /// Rejecting rather than clamping is deliberate here: a caller passing 150 has a bug, and
    /// silently playing at full volume is the worst possible way to reveal it. Use
    /// [`Volume::clamped`] where a value from a slider legitimately needs pinning.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Invalid`] when `level` exceeds 100.
    ///
    /// Covers: RQ-CTL-012
    pub fn new(level: u8) -> Result<Self, CoreError> {
        if level <= 100 {
            Ok(Self(level))
        } else {
            Err(CoreError::invalid("volume", format!("{level} is above the maximum of 100")))
        }
    }

    /// Constructs a volume, pinning anything above 100 to the maximum.
    #[must_use]
    pub const fn clamped(level: u8) -> Self {
        if level > 100 { Self::MAX } else { Self(level) }
    }

    /// The level as a number.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Whether this is silence.
    #[must_use]
    pub const fn is_muted(self) -> bool {
        self.0 == 0
    }
}

impl TryFrom<u8> for Volume {
    type Error = CoreError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Volume> for u8 {
    fn from(value: Volume) -> Self {
        value.0
    }
}

impl FromStr for Volume {
    type Err = CoreError;

    /// Parses the string form a device returns in `GetVolumeResponse`.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let level: u8 = raw
            .trim()
            .parse()
            .map_err(|_| CoreError::invalid("volume", format!("`{raw}` is not a number")))?;
        Self::new(level)
    }
}

impl fmt::Display for Volume {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
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

    /// Covers: RQ-CTL-012
    #[test]
    fn rq_ctl_012_volume_is_always_in_range() {
        assert_eq!(Volume::new(0).unwrap().get(), 0);
        assert_eq!(Volume::new(100).unwrap().get(), 100);
        assert!(Volume::new(101).is_err(), "101 must be refused, not clamped silently");
        assert!(Volume::new(255).is_err());

        // Clamping is an explicit, separate choice.
        assert_eq!(Volume::clamped(200), Volume::MAX);
        assert_eq!(Volume::clamped(42).get(), 42);
    }

    #[test]
    fn the_invariant_survives_deserialisation() {
        // The IPC boundary deserialises this from a WebView, so the constructor is not the
        // only door. `try_from` closes the other one.
        assert!(serde_json::from_str::<Volume>("101").is_err());
        assert_eq!(serde_json::from_str::<Volume>("55").unwrap().get(), 55);
        assert_eq!(serde_json::to_string(&Volume::new(55).unwrap()).unwrap(), "55");
    }

    #[test]
    fn device_responses_parse_including_the_whitespace_they_carry() {
        assert_eq!("34".parse::<Volume>().unwrap().get(), 34);
        assert_eq!(" 7 \n".parse::<Volume>().unwrap().get(), 7);
        assert!("".parse::<Volume>().is_err());
        assert!("loud".parse::<Volume>().is_err());
        assert!("101".parse::<Volume>().is_err());
        assert!("-1".parse::<Volume>().is_err());
    }

    #[test]
    fn mute_is_expressible_and_recognisable() {
        assert!(Volume::MUTED.is_muted());
        assert!(!Volume::new(1).unwrap().is_muted());
        assert_eq!(Volume::MUTED.to_string(), "0");
    }

    #[test]
    fn volumes_order_the_way_a_slider_expects() {
        let mut levels = [Volume::new(50).unwrap(), Volume::MUTED, Volume::MAX];
        levels.sort_unstable();
        assert_eq!(levels, [Volume::MUTED, Volume::new(50).unwrap(), Volume::MAX]);
    }

    proptest::proptest! {
        /// However a volume is constructed, it is in range. There is no path to an invalid one.
        #[test]
        fn no_construction_path_escapes_the_range(raw in proptest::num::u8::ANY) {
            proptest::prop_assert!(Volume::clamped(raw).get() <= 100);
            if let Ok(volume) = Volume::new(raw) {
                proptest::prop_assert!(volume.get() <= 100);
            }
            if let Ok(volume) = raw.to_string().parse::<Volume>() {
                proptest::prop_assert!(volume.get() <= 100);
            }
        }
    }
}
