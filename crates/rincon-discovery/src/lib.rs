//! Finding Sonos players on the local network.
//!
//! Discovery is where Rincon first touches untrusted input, and almost all of the code here
//! exists to handle that rather than to send a UDP packet. The pipeline is:
//!
//! ```text
//!   M-SEARCH (per interface)
//!        │
//!        ▼
//!   parse datagram ──── lenient about shape, strict about size   [ssdp]
//!        │
//!        ▼
//!   validate LOCATION ── private IP? http? literal host? port?   [rincon_core::net]
//!        │
//!        ▼
//!   fetch description ── no redirects, capped bytes, deadline    [fetch]
//!        │
//!        ▼
//!   parse description ── no DOCTYPE, depth-aware, sanitised      [description]
//!        │
//!        ▼
//!   Device { id, room, model, address, reached_via }
//! ```
//!
//! Every arrow is a place a hostile responder gets to try something, and each has its own
//! requirement and its own tests. See [SPEC-003](../../../specs/SPEC-003-discovery.md).

use std::fmt;

use async_trait::async_trait;
use rincon_core::device::Device;

pub mod description;
pub mod fetch;
pub mod scan;
pub mod ssdp;

pub use description::{DescriptionError, DeviceDescription};
pub use fetch::FetchError;
pub use scan::{
    InterfaceKind, ProbeInterface, ScanConfig, ScanError, ScanOutcome, UnresolvedDevice,
    preferred_lan_interface,
};
pub use ssdp::{SsdpError, SsdpResponse};

/// Anything that can produce a list of devices.
///
/// The engine depends on this trait rather than on [`SsdpDiscovery`], so its whole state
/// machine is exercised in CI against a fake that returns fixed devices.
#[async_trait]
pub trait DeviceDiscovery: Send + Sync + fmt::Debug {
    /// Runs one discovery pass.
    ///
    /// # Errors
    ///
    /// Returns [`ScanError`] only when discovery could not run at all. Finding nothing is a
    /// successful scan with an empty result.
    async fn discover(&self) -> Result<ScanOutcome, ScanError>;
}

/// The real implementation: SSDP multicast over every usable interface.
#[derive(Debug, Clone, Copy, Default)]
pub struct SsdpDiscovery {
    config: ScanConfig,
}

impl SsdpDiscovery {
    /// A discoverer using the audited default limits.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A discoverer with a custom configuration.
    #[must_use]
    pub const fn with_config(config: ScanConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl DeviceDiscovery for SsdpDiscovery {
    async fn discover(&self) -> Result<ScanOutcome, ScanError> {
        scan::scan(self.config).await
    }
}

/// Finds a previously seen device again after its address changed.
///
/// DHCP moves speakers. When a control request fails with a connection error, the engine
/// rediscovers and matches on the stable identity rather than the address — which is the whole
/// reason [`rincon_core::device::DeviceId`] exists.
#[must_use]
pub fn find_by_id<'a>(
    outcome: &'a ScanOutcome,
    id: &rincon_core::device::DeviceId,
) -> Option<&'a Device> {
    outcome.devices.iter().find(|device| &device.id == id)
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

    use rincon_core::device::{DeviceId, DeviceName};

    use super::*;

    fn device(id: &str, room: &str, last_octet: u8) -> Device {
        Device {
            id: DeviceId::new(id).unwrap(),
            room: DeviceName::new(room).unwrap(),
            model: DeviceName::new("Sonos One").unwrap(),
            address: std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, last_octet)),
            port: 1400,
            reached_via: Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20))),
        }
    }

    #[test]
    fn a_device_is_found_by_identity_not_by_address() {
        // The scenario: DHCP moved the speaker from .45 to .77 between scans. Lookup by
        // identity must still find it, which is what makes reconnection transparent.
        let before = ScanOutcome {
            devices: vec![device("uuid:RINCON_A", "Kitchen", 45)],
            ..Default::default()
        };
        let after = ScanOutcome {
            devices: vec![device("uuid:RINCON_A", "Kitchen", 77)],
            ..Default::default()
        };

        let id = DeviceId::new("uuid:RINCON_A").unwrap();
        assert_eq!(find_by_id(&before, &id).unwrap().address.to_string(), "192.168.1.45");
        assert_eq!(find_by_id(&after, &id).unwrap().address.to_string(), "192.168.1.77");

        let gone = DeviceId::new("uuid:RINCON_GONE").unwrap();
        assert!(find_by_id(&after, &gone).is_none());
    }

    #[test]
    fn the_real_discoverer_uses_the_audited_defaults() {
        let discovery = SsdpDiscovery::new();
        assert_eq!(discovery.config.window, rincon_core::limits::DISCOVERY_WINDOW);
    }
}
