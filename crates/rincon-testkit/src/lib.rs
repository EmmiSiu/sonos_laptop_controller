//! Fakes that let the whole of Rincon be tested without a speaker in the room.
//!
//! # Why this crate exists
//!
//! The interesting failures in this project — a grouped speaker refusing `Play`, a firewall
//! swallowing the stream, a session leaking a port on the error path — all live in the
//! orchestration layer. If testing that layer required hardware, it would be tested on one
//! developer's laptop and nowhere else, least of all in CI, which is where a regression is
//! actually caught.
//!
//! So every trait the engine depends on has a fake here:
//!
//! | Trait | Fake | What it lets you test |
//! | ----- | ---- | --------------------- |
//! | [`rincon_discovery::DeviceDiscovery`] | [`FakeDiscovery`] | empty scans, failures, many devices |
//! | [`rincon_control::TransportControl`] | [`FakeControl`] | grouping, UPnP faults, command order |
//! | `rincon_audio::AudioCapture` | `rincon_audio::SyntheticCapture` | deterministic audio, no sound card |
//!
//! [`MockSonos`] goes one step further: a real HTTP server that answers real SOAP, so
//! `rincon-control` can be exercised over an actual socket rather than against a stub.
//!
//! Everything here records what it was asked to do, because "did we send `Play` to the
//! coordinator and not to the member?" is the question most engine tests are really asking.

// This crate is `publish = false` and only ever links into a test binary. A helper that
// silently produces a wrong fixture is far worse than one that panics: the panic names the
// line, while the wrong fixture makes some unrelated assertion fail three files away. So the
// production-code prohibition on `expect` is deliberately relaxed here, and nowhere else.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test-support crate: loud failure on programmer error is the correct behaviour"
)]

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rincon_control::{ControlError, TrackMeta, TransportControl, Volume};
use rincon_core::device::{Device, DeviceId, DeviceName};
use rincon_core::stream_url::StreamUrl;
use rincon_discovery::{DeviceDiscovery, ScanError, ScanOutcome};

pub mod mock_sonos;

pub use mock_sonos::MockSonos;

/// Builds a plausible device without the ceremony, for tests that just need *a* speaker.
///
/// # Panics
///
/// Panics if `room` cannot form a valid device name — which only happens if it is empty or
/// contains nothing printable. Test helpers are allowed to panic on programmer error; that is
/// what makes them readable at the call site.
#[must_use]
pub fn device(room: &str, last_octet: u8) -> Device {
    Device {
        id: DeviceId::new(format!("uuid:RINCON_{}", room.to_uppercase().replace(' ', "_")))
            .expect("test device id must be valid"),
        room: DeviceName::new(room).expect("test room name must be valid"),
        model: DeviceName::new("Sonos One").expect("test model name must be valid"),
        address: std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, last_octet)),
        port: 1400,
        reached_via: Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20))),
    }
}

/// What a fake discovery should do when asked.
#[derive(Debug, Clone)]
pub enum DiscoveryBehaviour {
    /// Return these devices.
    Found(Vec<Device>),
    /// Return nothing, successfully. The interface must distinguish this from a failure.
    Empty,
    /// Fail to run at all.
    Failing(String),
}

/// A [`DeviceDiscovery`] that answers from a script.
#[derive(Debug, Clone)]
pub struct FakeDiscovery {
    behaviour: DiscoveryBehaviour,
    calls: Arc<Mutex<usize>>,
}

impl FakeDiscovery {
    /// A discovery that finds `devices`.
    #[must_use]
    pub fn finding(devices: Vec<Device>) -> Self {
        Self { behaviour: DiscoveryBehaviour::Found(devices), calls: Arc::default() }
    }

    /// A discovery that finds nothing.
    #[must_use]
    pub fn empty() -> Self {
        Self { behaviour: DiscoveryBehaviour::Empty, calls: Arc::default() }
    }

    /// A discovery that cannot run.
    #[must_use]
    pub fn failing(detail: &str) -> Self {
        Self { behaviour: DiscoveryBehaviour::Failing(detail.to_owned()), calls: Arc::default() }
    }

    /// How many scans have been requested.
    #[must_use]
    pub fn call_count(&self) -> usize {
        *self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl DeviceDiscovery for FakeDiscovery {
    async fn discover(&self) -> Result<ScanOutcome, ScanError> {
        *self.calls.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        match &self.behaviour {
            DiscoveryBehaviour::Found(devices) => {
                Ok(ScanOutcome { devices: devices.clone(), ..Default::default() })
            }
            DiscoveryBehaviour::Empty => Ok(ScanOutcome::default()),
            DiscoveryBehaviour::Failing(detail) => {
                Err(ScanError::SocketFailed { detail: detail.clone() })
            }
        }
    }
}

/// One command a [`FakeControl`] was asked to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlCall {
    /// `SetAVTransportURI`, with the redacted URL so a recorded call is safe to print.
    SetUri {
        /// Which speaker.
        room: String,
        /// The URL, token removed.
        url: String,
    },
    /// `Play`.
    Play {
        /// Which speaker.
        room: String,
    },
    /// `Stop`.
    Stop {
        /// Which speaker.
        room: String,
    },
    /// `SetVolume`.
    SetVolume {
        /// Which speaker.
        room: String,
        /// The level.
        level: u8,
    },
    /// `GetZoneGroupState`.
    ResolveCoordinator {
        /// Which speaker.
        room: String,
    },
}

/// A [`TransportControl`] that records what it was told and answers from a script.
#[derive(Debug, Clone, Default)]
pub struct FakeControl {
    calls: Arc<Mutex<Vec<ControlCall>>>,
    coordinator: Arc<Mutex<Option<String>>>,
    fail_with: Arc<Mutex<Option<ControlError>>>,
    volume: Arc<Mutex<u8>>,
}

impl FakeControl {
    /// A controller that accepts everything.
    #[must_use]
    pub fn accepting() -> Self {
        Self { volume: Arc::new(Mutex::new(30)), ..Self::default() }
    }

    /// A controller that answers `coordinator_of` with a different speaker, modelling a group.
    #[must_use]
    pub fn grouped_under(self, coordinator_uuid: &str) -> Self {
        *self.coordinator.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(coordinator_uuid.to_owned());
        self
    }

    /// A controller that refuses every command with `error`.
    #[must_use]
    pub fn refusing(self, error: ControlError) -> Self {
        *self.fail_with.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
        self
    }

    /// Everything the controller was asked to do, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<ControlCall> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Whether a matching call was recorded.
    #[must_use]
    pub fn saw(&self, call: &ControlCall) -> bool {
        self.calls().iter().any(|recorded| recorded == call)
    }

    fn record(&self, call: ControlCall) -> Result<(), ControlError> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).push(call);
        // Bound to a local first: a `MutexGuard` living until the end of a `match` is how a
        // fake ends up deadlocking the test it was meant to simplify.
        let failure = self.fail_with.lock().unwrap_or_else(PoisonError::into_inner).clone();
        failure.map_or(Ok(()), Err)
    }
}

#[async_trait]
impl TransportControl for FakeControl {
    async fn set_stream_uri(
        &self,
        target: &Device,
        uri: &StreamUrl,
        _meta: &TrackMeta,
    ) -> Result<(), ControlError> {
        self.record(ControlCall::SetUri { room: target.room.to_string(), url: uri.redacted() })
    }

    async fn play(&self, target: &Device) -> Result<(), ControlError> {
        self.record(ControlCall::Play { room: target.room.to_string() })
    }

    async fn stop(&self, target: &Device) -> Result<(), ControlError> {
        self.record(ControlCall::Stop { room: target.room.to_string() })
    }

    async fn volume(&self, _target: &Device) -> Result<Volume, ControlError> {
        Ok(Volume::clamped(*self.volume.lock().unwrap_or_else(PoisonError::into_inner)))
    }

    async fn set_volume(&self, target: &Device, level: Volume) -> Result<(), ControlError> {
        *self.volume.lock().unwrap_or_else(PoisonError::into_inner) = level.get();
        self.record(ControlCall::SetVolume { room: target.room.to_string(), level: level.get() })
    }

    async fn coordinator_of(&self, target: &Device) -> Result<String, ControlError> {
        self.record(ControlCall::ResolveCoordinator { room: target.room.to_string() })?;
        Ok(self
            .coordinator
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| rincon_control::uuid_of(target)))
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

    #[tokio::test]
    async fn fake_discovery_distinguishes_empty_from_broken() {
        let empty = FakeDiscovery::empty();
        let outcome = empty.discover().await.unwrap();
        assert!(outcome.is_empty(), "an empty scan is a success with no devices");
        assert_eq!(empty.call_count(), 1);

        let broken = FakeDiscovery::failing("no interface");
        assert!(broken.discover().await.is_err(), "a broken scan is an error");
    }

    #[tokio::test]
    async fn fake_control_records_commands_in_order() {
        let control = FakeControl::accepting();
        let kitchen = device("Kitchen", 45);

        control.coordinator_of(&kitchen).await.unwrap();
        control.play(&kitchen).await.unwrap();
        control.stop(&kitchen).await.unwrap();

        assert_eq!(
            control.calls(),
            vec![
                ControlCall::ResolveCoordinator { room: "Kitchen".into() },
                ControlCall::Play { room: "Kitchen".into() },
                ControlCall::Stop { room: "Kitchen".into() },
            ]
        );
    }

    #[tokio::test]
    async fn a_grouped_fake_retargets_its_coordinator() {
        let control = FakeControl::accepting().grouped_under("RINCON_LIVING_ROOM");
        let coordinator = control.coordinator_of(&device("Kitchen", 45)).await.unwrap();
        assert_eq!(coordinator, "RINCON_LIVING_ROOM");
    }

    #[tokio::test]
    async fn a_refusing_fake_still_records_what_it_refused() {
        // The recording matters: a test about error handling usually needs to assert that the
        // command was attempted, not merely that it failed.
        let control = FakeControl::accepting().refusing(ControlError::TransitionNotAvailable);
        let kitchen = device("Kitchen", 45);

        assert_eq!(control.play(&kitchen).await.unwrap_err(), ControlError::TransitionNotAvailable);
        assert!(control.saw(&ControlCall::Play { room: "Kitchen".into() }));
    }

    #[tokio::test]
    async fn recorded_urls_carry_no_token() {
        // Recorded calls end up in assertion messages, which end up in CI logs.
        let control = FakeControl::accepting();
        let url = StreamUrl::new(
            "192.168.1.20".parse().unwrap(),
            41_234,
            "0123456789abcdef0123456789abcdef",
        );
        control.set_stream_uri(&device("Kitchen", 45), &url, &TrackMeta::default()).await.unwrap();

        let recorded = format!("{:?}", control.calls());
        assert!(!recorded.contains("0123456789abcdef"), "a token reached a recorded call");
    }

    #[test]
    fn the_device_helper_produces_something_usable() {
        let kitchen = device("Living Room", 51);
        assert_eq!(kitchen.room.as_str(), "Living Room");
        assert_eq!(kitchen.id.as_str(), "uuid:RINCON_LIVING_ROOM");
        assert!(kitchen.reached_via.is_some(), "the engine needs an interface to bind");
    }
}
