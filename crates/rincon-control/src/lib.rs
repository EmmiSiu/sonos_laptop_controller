//! Telling a Sonos player what to do.
//!
//! Rincon needs five things from a speaker: resolve its group coordinator, point it at our
//! stream, start playing, read and set volume, and stop. Everything else a Sonos exposes —
//! queues, favourites, alarms, music services — is deliberately absent (SPEC-004 §3).
//!
//! # Shape of the module
//!
//! ```text
//!   TransportControl (trait)        <- what the engine depends on
//!        │
//!        ├── SoapControl            <- the real device, over HTTP/SOAP
//!        └── (test fakes)           <- rincon-testkit, used by every engine test
//!
//!   soap      envelope construction + escaping + fault parsing
//!   didl      the metadata document that travels inside the envelope
//!   topology  which speaker actually accepts transport commands
//!   volume    a level that cannot be out of range
//!   error     one variant per thing the user might have to do about it
//! ```
//!
//! The engine depends on the trait, never on `SoapControl`, which is what allows the whole
//! session state machine to be tested in CI with no speaker in the building.

use std::fmt;
use std::net::IpAddr;

use async_trait::async_trait;
use rincon_core::device::Device;
use rincon_core::limits::Budget;
use rincon_core::stream_url::StreamUrl;
use rincon_core::{limits, net};
use tracing::{debug, instrument};
use url::Url;

pub mod didl;
pub mod error;
pub mod soap;
pub mod topology;
pub mod volume;

pub use didl::TrackMeta;
pub use error::ControlError;
pub use soap::{Service, SoapError};
pub use topology::Topology;
pub use volume::Volume;

/// What the engine needs from a speaker.
#[async_trait]
pub trait TransportControl: Send + Sync + fmt::Debug {
    /// Points the speaker at our stream URL.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when the device refuses or cannot be reached.
    async fn set_stream_uri(
        &self,
        target: &Device,
        uri: &StreamUrl,
        meta: &TrackMeta,
    ) -> Result<(), ControlError>;

    /// Starts playback.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when the device refuses or cannot be reached.
    async fn play(&self, target: &Device) -> Result<(), ControlError>;

    /// Stops playback.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when the device refuses or cannot be reached.
    async fn stop(&self, target: &Device) -> Result<(), ControlError>;

    /// Reads the current volume.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when the device refuses or cannot be reached.
    async fn volume(&self, target: &Device) -> Result<Volume, ControlError>;

    /// Sets the volume.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when the device refuses or cannot be reached.
    async fn set_volume(&self, target: &Device, level: Volume) -> Result<(), ControlError>;

    /// Resolves the UUID of the speaker that accepts transport commands for `target`.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError`] when the topology cannot be read.
    async fn coordinator_of(&self, target: &Device) -> Result<String, ControlError>;
}

/// The real implementation: UPnP SOAP over HTTP on port 1400.
#[derive(Debug, Clone)]
pub struct SoapControl {
    client: reqwest::Client,
}

impl SoapControl {
    /// Builds a controller with the audited timeouts.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::BadResponse`] when the HTTP client cannot be constructed.
    pub fn new() -> Result<Self, ControlError> {
        let client = reqwest::Client::builder()
            // A control response must never be a redirect we chase somewhere else.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(limits::CONTROL_TIMEOUT)
            .connect_timeout(limits::CONTROL_TIMEOUT)
            .user_agent(concat!("Rincon/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|source| ControlError::BadResponse(source.to_string()))?;
        Ok(Self { client })
    }

    /// Builds the control endpoint URL for a service on a device, validating the address.
    fn endpoint(target: &Device, service: Service) -> Result<Url, ControlError> {
        net::ensure_lan_destination(target.address)?;
        let authority = match target.address {
            IpAddr::V4(v4) => format!("{v4}:{}", target.port),
            IpAddr::V6(v6) => format!("[{v6}]:{}", target.port),
        };
        Url::parse(&format!("http://{authority}{}", service.control_path()))
            .map_err(|source| ControlError::BadResponse(source.to_string()))
    }

    /// Performs one SOAP call and returns its output arguments.
    #[instrument(skip(self, args), fields(device = %target.room, %action))]
    async fn call(
        &self,
        target: &Device,
        service: Service,
        action: &str,
        args: &[(&str, &str)],
    ) -> Result<std::collections::BTreeMap<String, String>, ControlError> {
        let endpoint = Self::endpoint(target, service)?;
        let body = soap::envelope(service, action, args);

        let response = self
            .client
            .post(endpoint)
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header("SOAPACTION", soap::action_header(service, action))
            .body(body)
            .send()
            .await
            .map_err(|source| ControlError::Unreachable { detail: source.to_string() })?;

        let status = response.status();
        let text = read_capped(response, Budget::CONTROL).await?;

        // A SOAP fault arrives with HTTP 500, so the body is parsed either way: the fault
        // detail is the only place the actionable UPnP code lives.
        match soap::parse_response(&text) {
            Ok(values) if status.is_success() => Ok(values),
            Ok(_) => Err(ControlError::BadResponse(format!("HTTP {status} with no fault detail"))),
            Err(source) => Err(source.into()),
        }
    }
}

/// Reads a response body, abandoning the transfer if it overruns `budget`.
async fn read_capped(
    mut response: reqwest::Response,
    budget: Budget,
) -> Result<String, ControlError> {
    let mut collected: Vec<u8> = Vec::with_capacity(4096);
    loop {
        let next = response
            .chunk()
            .await
            .map_err(|source| ControlError::Unreachable { detail: source.to_string() })?;
        let Some(chunk) = next else { break };
        if collected.len() + chunk.len() > budget.max_bytes {
            return Err(ControlError::BadResponse(format!(
                "response exceeded its {} byte budget",
                budget.max_bytes
            )));
        }
        collected.extend_from_slice(&chunk);
    }
    String::from_utf8(collected)
        .map_err(|_| ControlError::BadResponse("response is not valid UTF-8".to_owned()))
}

#[async_trait]
impl TransportControl for SoapControl {
    async fn set_stream_uri(
        &self,
        target: &Device,
        uri: &StreamUrl,
        meta: &TrackMeta,
    ) -> Result<(), ControlError> {
        // `uri` renders redacted in the log line; only the envelope gets the real value.
        debug!(url = %uri, "pointing the speaker at our stream");
        self.call(
            target,
            Service::AvTransport,
            "SetAVTransportURI",
            &[
                ("InstanceID", "0"),
                ("CurrentURI", uri.as_str()),
                ("CurrentURIMetaData", &meta.to_didl()),
            ],
        )
        .await
        .map(drop)
    }

    async fn play(&self, target: &Device) -> Result<(), ControlError> {
        self.call(target, Service::AvTransport, "Play", &[("InstanceID", "0"), ("Speed", "1")])
            .await
            .map(drop)
    }

    async fn stop(&self, target: &Device) -> Result<(), ControlError> {
        self.call(target, Service::AvTransport, "Stop", &[("InstanceID", "0")]).await.map(drop)
    }

    async fn volume(&self, target: &Device) -> Result<Volume, ControlError> {
        let values = self
            .call(
                target,
                Service::RenderingControl,
                "GetVolume",
                &[("InstanceID", "0"), ("Channel", "Master")],
            )
            .await?;

        values
            .get("CurrentVolume")
            .ok_or(ControlError::MissingValue("CurrentVolume"))?
            .parse()
            .map_err(|_| ControlError::BadResponse("volume is not a number in 0..=100".to_owned()))
    }

    async fn set_volume(&self, target: &Device, level: Volume) -> Result<(), ControlError> {
        self.call(
            target,
            Service::RenderingControl,
            "SetVolume",
            &[("InstanceID", "0"), ("Channel", "Master"), ("DesiredVolume", &level.to_string())],
        )
        .await
        .map(drop)
    }

    async fn coordinator_of(&self, target: &Device) -> Result<String, ControlError> {
        let values =
            self.call(target, Service::ZoneGroupTopology, "GetZoneGroupState", &[]).await?;

        let Some(state) = values.get("ZoneGroupState") else {
            // A device that does not implement topology is standalone as far as we care.
            return Ok(uuid_of(target));
        };

        let topology = topology::parse(state)?;
        Ok(topology.coordinator_of(&uuid_of(target)).to_owned())
    }
}

/// The bare UUID for a device, stripping the `uuid:` prefix the UDN carries.
///
/// Topology documents use the bare form (`RINCON_xxx`) while descriptions use the prefixed one
/// (`uuid:RINCON_xxx`). Comparing the two without normalising is a bug that presents as
/// "grouping support does not work", so it is done in exactly one place.
#[must_use]
pub fn uuid_of(device: &Device) -> String {
    device.id.as_str().trim_start_matches("uuid:").to_owned()
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

    use std::time::{Duration, Instant};

    use rincon_core::device::{DeviceId, DeviceName};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn device_at(uri: &str) -> Device {
        let url = Url::parse(uri).unwrap();
        Device {
            id: DeviceId::new("uuid:RINCON_TEST").unwrap(),
            room: DeviceName::new("Kitchen").unwrap(),
            model: DeviceName::new("Sonos One").unwrap(),
            address: url.host_str().unwrap().parse().unwrap(),
            port: url.port().unwrap(),
            reached_via: None,
        }
    }

    fn ok_body(action: &str, inner: &str) -> String {
        format!(
            "<s:Envelope xmlns:s='http://schemas.xmlsoap.org/soap/envelope/'><s:Body>\
             <u:{action}Response xmlns:u='urn:schemas-upnp-org:service:AVTransport:1'>{inner}\
             </u:{action}Response></s:Body></s:Envelope>"
        )
    }

    #[test]
    fn uuids_are_normalised_before_topology_comparison() {
        // The prefixed and bare forms must reconcile, or grouping silently never matches.
        let device = device_at("http://127.0.0.1:1400/");
        assert_eq!(uuid_of(&device), "RINCON_TEST");
    }

    #[test]
    fn endpoints_are_validated_against_the_private_address_guard() {
        let mut public = device_at("http://127.0.0.1:1400/");
        public.address = "8.8.8.8".parse().unwrap();
        assert!(matches!(
            SoapControl::endpoint(&public, Service::AvTransport),
            Err(ControlError::Refused(_))
        ));

        let private = device_at("http://127.0.0.1:1400/");
        let url = SoapControl::endpoint(&private, Service::AvTransport).unwrap();
        assert_eq!(url.path(), "/MediaRenderer/AVTransport/Control");
    }

    #[tokio::test]
    async fn play_sends_the_exact_action_header_and_envelope() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/MediaRenderer/AVTransport/Control"))
            .and(header("SOAPACTION", "\"urn:schemas-upnp-org:service:AVTransport:1#Play\""))
            .respond_with(ResponseTemplate::new(200).set_body_string(ok_body("Play", "")))
            .expect(1)
            .mount(&server)
            .await;

        let device = device_at(&server.uri());
        SoapControl::new().unwrap().play(&device).await.unwrap();
        // `expect(1)` is asserted when the server drops.
    }

    #[tokio::test]
    async fn volume_round_trips_through_the_device() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/MediaRenderer/RenderingControl/Control"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "<s:Envelope xmlns:s='http://schemas.xmlsoap.org/soap/envelope/'><s:Body>\
                 <u:GetVolumeResponse xmlns:u='urn:schemas-upnp-org:service:RenderingControl:1'>\
                 <CurrentVolume>37</CurrentVolume></u:GetVolumeResponse></s:Body></s:Envelope>",
            ))
            .mount(&server)
            .await;

        let device = device_at(&server.uri());
        let level = SoapControl::new().unwrap().volume(&device).await.unwrap();
        assert_eq!(level.get(), 37);
    }

    #[tokio::test]
    async fn a_fault_becomes_the_specific_actionable_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string(
                "<s:Envelope xmlns:s='http://schemas.xmlsoap.org/soap/envelope/'><s:Body>\
                 <s:Fault><faultstring>UPnPError</faultstring><detail><UPnPError>\
                 <errorCode>701</errorCode><errorDescription>Transition not available\
                 </errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>",
            ))
            .mount(&server)
            .await;

        let device = device_at(&server.uri());
        let err = SoapControl::new().unwrap().play(&device).await.unwrap_err();
        assert_eq!(err, ControlError::TransitionNotAvailable);
        assert!(err.retryable(), "the user can stop the other source and try again");
    }

    /// Covers: RQ-CTL-010
    #[tokio::test]
    async fn rq_ctl_010_times_out_and_does_not_retry_play() {
        let server = MockServer::start().await;
        // Answer slowly, and count how many times we are asked. A retry here would mean the
        // speaker receives `Play` twice, which is exactly the non-idempotent case the spec
        // forbids retrying.
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(ok_body("Play", ""))
                    .set_delay(Duration::from_secs(30)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let device = device_at(&server.uri());
        let started = Instant::now();
        let result = SoapControl::new().unwrap().play(&device).await;
        let elapsed = started.elapsed();

        assert!(result.is_err(), "a stalled device must not report success");
        assert!(
            elapsed < limits::CONTROL_TIMEOUT + Duration::from_secs(2),
            "gave up only after {elapsed:?}, over the {:?} budget",
            limits::CONTROL_TIMEOUT
        );
    }

    /// Covers: RQ-CTL-011
    #[tokio::test]
    async fn rq_ctl_011_caps_response_size() {
        let server = MockServer::start().await;
        let oversized = format!(
            "<s:Envelope><s:Body><x>{}</x></s:Body></s:Envelope>",
            "A".repeat(limits::MAX_SOAP_BYTES + 1024)
        );
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oversized))
            .mount(&server)
            .await;

        let device = device_at(&server.uri());
        let err = SoapControl::new().unwrap().play(&device).await.unwrap_err();
        assert!(
            matches!(err, ControlError::BadResponse(ref detail) if detail.contains("budget")),
            "an oversized response must be refused, got {err:?}"
        );
    }

    /// Covers: RQ-CTL-008
    #[tokio::test]
    async fn rq_ctl_008_resolves_group_coordinator() {
        let server = MockServer::start().await;
        // The topology is returned as escaped XML inside the SOAP response, exactly as a real
        // device does it -- the nested-escaping case a hand-rolled parser gets wrong.
        let inner = "&lt;ZoneGroups&gt;&lt;ZoneGroup Coordinator=&quot;RINCON_KITCHEN&quot;&gt;\
            &lt;ZoneGroupMember UUID=&quot;RINCON_TEST&quot; ZoneName=&quot;Dining&quot;/&gt;\
            &lt;/ZoneGroup&gt;&lt;/ZoneGroups&gt;";
        Mock::given(method("POST"))
            .and(path("/ZoneGroupTopology/Control"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                "<s:Envelope xmlns:s='http://schemas.xmlsoap.org/soap/envelope/'><s:Body>\
                 <u:GetZoneGroupStateResponse><ZoneGroupState>{inner}</ZoneGroupState>\
                 </u:GetZoneGroupStateResponse></s:Body></s:Envelope>"
            )))
            .mount(&server)
            .await;

        let device = device_at(&server.uri());
        let coordinator = SoapControl::new().unwrap().coordinator_of(&device).await.unwrap();
        assert_eq!(coordinator, "RINCON_KITCHEN", "a grouped member must retarget its leader");
    }

    /// Covers: RQ-CTL-009
    #[tokio::test]
    async fn rq_ctl_009_refuses_public_target() {
        let mut device = device_at("http://127.0.0.1:1400/");
        device.address = "93.184.216.34".parse().unwrap();

        // The guard fires before any socket is opened, so this returns immediately.
        let started = Instant::now();
        let err = SoapControl::new().unwrap().play(&device).await.unwrap_err();
        assert!(matches!(err, ControlError::Refused(_)), "got {err:?}");
        assert!(started.elapsed() < Duration::from_millis(100), "the guard must precede the dial");
    }

    #[tokio::test]
    async fn a_device_without_topology_support_is_treated_as_standalone() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ZoneGroupTopology/Control"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "<s:Envelope xmlns:s='http://schemas.xmlsoap.org/soap/envelope/'><s:Body>\
                 <u:GetZoneGroupStateResponse/></s:Body></s:Envelope>",
            ))
            .mount(&server)
            .await;

        let device = device_at(&server.uri());
        let coordinator = SoapControl::new().unwrap().coordinator_of(&device).await.unwrap();
        assert_eq!(coordinator, "RINCON_TEST", "falling back to itself keeps the session alive");
    }
}
