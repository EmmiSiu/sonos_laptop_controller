//! A real HTTP server that answers like a Sonos player.
//!
//! [`FakeControl`](crate::FakeControl) is enough for engine tests, which care about
//! orchestration. This is for the layer below: it exercises `rincon-control` and
//! `rincon-discovery` over an actual socket, so the SOAP bytes, the headers, the escaping, and
//! the response parsing are all tested against something that can disagree with us.
//!
//! It records every request, so a test can assert on what was sent rather than only on what
//! came back — which is the difference between "playback started" and "we sent `Play` to the
//! coordinator with the right `SOAPACTION`".

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, body::Bytes};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// One request the mock received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// Path requested.
    pub path: String,
    /// The `SOAPACTION` header, if any.
    pub soap_action: Option<String>,
    /// The raw body.
    pub body: String,
}

impl RecordedRequest {
    /// Whether the body contains `needle`. Most assertions are of this shape.
    #[must_use]
    pub fn body_contains(&self, needle: &str) -> bool {
        self.body.contains(needle)
    }

    /// The bare action name from `SOAPACTION`, e.g. `Play`.
    #[must_use]
    pub fn action(&self) -> Option<&str> {
        self.soap_action.as_deref()?.rsplit('#').next().map(|a| a.trim_matches('"'))
    }
}

/// How the mock should behave.
#[derive(Debug, Clone)]
pub struct MockConfig {
    /// The room name the description reports.
    pub room: String,
    /// The model the description reports.
    pub model: String,
    /// The UDN the description reports.
    pub udn: String,
    /// When set, every SOAP call answers with this UPnP fault instead of succeeding.
    pub fault: Option<(u16, String)>,
    /// The coordinator reported by `GetZoneGroupState`. Defaults to the device itself.
    pub coordinator: Option<String>,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            room: "Kitchen".to_owned(),
            model: "Sonos One".to_owned(),
            udn: "uuid:RINCON_MOCK000000001400".to_owned(),
            fault: None,
            coordinator: None,
        }
    }
}

/// A running fake Sonos player.
#[derive(Debug)]
pub struct MockSonos {
    /// Where it is listening.
    pub addr: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
struct MockState {
    config: MockConfig,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl MockSonos {
    /// Starts a mock on loopback with the default configuration.
    ///
    /// # Panics
    ///
    /// Panics if a loopback port cannot be bound, which in a test means the environment is
    /// broken rather than the code.
    pub async fn start() -> Self {
        Self::with_config(MockConfig::default()).await
    }

    /// Starts a mock with a specific configuration.
    ///
    /// # Panics
    ///
    /// Panics if a loopback port cannot be bound.
    pub async fn with_config(config: MockConfig) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = MockState { config, requests: Arc::clone(&requests) };

        let app = Router::new()
            .route("/xml/device_description.xml", get(describe))
            .route("/MediaRenderer/AVTransport/Control", post(soap))
            .route("/MediaRenderer/RenderingControl/Control", post(soap))
            .route("/ZoneGroupTopology/Control", post(soap))
            .fallback(|| async { StatusCode::NOT_FOUND })
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("mock must bind loopback");
        let addr = listener.local_addr().expect("mock must report its address");

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });

        Self { addr, requests, shutdown: Some(shutdown_tx), task: Some(task) }
    }

    /// The device description URL, as an SSDP `LOCATION` would give it.
    #[must_use]
    pub fn location(&self) -> String {
        format!("http://{}/xml/device_description.xml", self.addr)
    }

    /// Everything the mock has been asked, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The SOAP actions received, in order. The most common assertion.
    #[must_use]
    pub fn actions(&self) -> Vec<String> {
        self.requests().iter().filter_map(|request| request.action().map(str::to_owned)).collect()
    }

    /// Stops the mock.
    pub async fn shutdown(mut self) {
        if let Some(signal) = self.shutdown.take() {
            let _ = signal.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for MockSonos {
    fn drop(&mut self) {
        if let Some(signal) = self.shutdown.take() {
            let _ = signal.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// The device description, shaped like a real one — including the embedded sub-devices that
/// a naive parser trips over.
async fn describe(State(state): State<MockState>) -> Response {
    state.requests.lock().unwrap_or_else(PoisonError::into_inner).push(RecordedRequest {
        path: "/xml/device_description.xml".to_owned(),
        soap_action: None,
        body: String::new(),
    });

    let MockConfig { room, model, udn, .. } = &state.config;
    let body = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:ZonePlayer:1</deviceType>
    <friendlyName>Mock - {model}</friendlyName>
    <manufacturer>Sonos, Inc.</manufacturer>
    <modelName>{model}</modelName>
    <UDN>{udn}</UDN>
    <roomName>{room}</roomName>
    <deviceList>
      <device>
        <deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
        <modelName>Mock MediaRenderer</modelName>
        <UDN>{udn}_MR</UDN>
        <roomName>WRONG ROOM</roomName>
      </device>
    </deviceList>
  </device>
</root>"#
    );

    ([(axum::http::header::CONTENT_TYPE, "text/xml")], body).into_response()
}

/// Every SOAP endpoint, answering the handful of actions Rincon sends.
async fn soap(State(state): State<MockState>, headers: HeaderMap, body: Bytes) -> Response {
    let soap_action =
        headers.get("soapaction").and_then(|value| value.to_str().ok()).map(str::to_owned);
    let body_text = String::from_utf8_lossy(&body).into_owned();

    let recorded = RecordedRequest { path: "soap".to_owned(), soap_action, body: body_text };
    let action = recorded.action().unwrap_or_default().to_owned();
    state.requests.lock().unwrap_or_else(PoisonError::into_inner).push(recorded);

    if let Some((code, description)) = &state.config.fault {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(axum::http::header::CONTENT_TYPE, "text/xml")],
            fault_body(*code, description),
        )
            .into_response();
    }

    let inner = match action.as_str() {
        "GetVolume" => "<CurrentVolume>30</CurrentVolume>".to_owned(),
        "GetZoneGroupState" => {
            let coordinator = state
                .config
                .coordinator
                .clone()
                .unwrap_or_else(|| state.config.udn.trim_start_matches("uuid:").to_owned());
            let member = state.config.udn.trim_start_matches("uuid:");
            // Escaped, exactly as a real device nests the topology document.
            format!(
                "<ZoneGroupState>&lt;ZoneGroups&gt;&lt;ZoneGroup Coordinator=&quot;{coordinator}&quot;&gt;\
                 &lt;ZoneGroupMember UUID=&quot;{member}&quot; ZoneName=&quot;{room}&quot;/&gt;\
                 &lt;/ZoneGroup&gt;&lt;/ZoneGroups&gt;</ZoneGroupState>",
                room = state.config.room
            )
        }
        _ => String::new(),
    };

    let body = format!(
        "<?xml version=\"1.0\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body>\
         <u:{action}Response>{inner}</u:{action}Response></s:Body></s:Envelope>"
    );
    ([(axum::http::header::CONTENT_TYPE, "text/xml")], body).into_response()
}

fn fault_body(code: u16, description: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><s:Fault>\
         <faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail>\
         <UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\">\
         <errorCode>{code}</errorCode><errorDescription>{description}</errorDescription>\
         </UPnPError></detail></s:Fault></s:Body></s:Envelope>"
    )
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

    use rincon_control::{SoapControl, TrackMeta, TransportControl};
    use rincon_core::device::{Device, DeviceId, DeviceName};
    use rincon_core::stream_url::StreamUrl;

    use super::*;

    fn device_at(addr: SocketAddr) -> Device {
        Device {
            id: DeviceId::new("uuid:RINCON_MOCK000000001400").unwrap(),
            room: DeviceName::new("Kitchen").unwrap(),
            model: DeviceName::new("Sonos One").unwrap(),
            address: addr.ip(),
            port: addr.port(),
            reached_via: None,
        }
    }

    /// Covers: RQ-CTL-001, RQ-CTL-005
    #[tokio::test]
    async fn rq_ctl_001_the_real_controller_sends_what_a_device_expects() {
        // This is the end-to-end check that the golden envelope fixture is not merely
        // self-consistent: it goes over a socket to something that inspects it.
        let mock = MockSonos::start().await;
        let device = device_at(mock.addr);
        let control = SoapControl::new().unwrap();

        let url = StreamUrl::new(
            "192.168.1.20".parse().unwrap(),
            41_234,
            "0123456789abcdef0123456789abcdef",
        );
        control.set_stream_uri(&device, &url, &TrackMeta::default()).await.unwrap();
        control.play(&device).await.unwrap();

        assert_eq!(mock.actions(), vec!["SetAVTransportURI", "Play"]);

        let set_uri = &mock.requests()[0];
        assert_eq!(
            set_uri.soap_action.as_deref(),
            Some("\"urn:schemas-upnp-org:service:AVTransport:1#SetAVTransportURI\"")
        );
        assert!(set_uri.body_contains("<InstanceID>0</InstanceID>"));
        assert!(set_uri.body_contains("/stream.wav"), "the URL must reach the device intact");
        // The DIDL arrives escaped, exactly once, as a value rather than as markup.
        assert!(set_uri.body_contains("&lt;DIDL-Lite "), "metadata was not escaped");
        assert!(
            !set_uri.body_contains("<DIDL-Lite "),
            "raw metadata markup would break the request"
        );

        mock.shutdown().await;
    }

    /// Covers: RQ-CTL-006, RQ-CTL-007
    #[tokio::test]
    async fn rq_ctl_006_a_real_fault_response_becomes_a_typed_error() {
        let mock = MockSonos::with_config(MockConfig {
            fault: Some((701, "Transition not available".to_owned())),
            ..MockConfig::default()
        })
        .await;

        let error = SoapControl::new().unwrap().play(&device_at(mock.addr)).await.unwrap_err();
        assert_eq!(error, rincon_control::ControlError::TransitionNotAvailable);
        mock.shutdown().await;
    }

    /// Covers: RQ-CTL-008
    #[tokio::test]
    async fn rq_ctl_008_grouping_is_resolved_over_a_real_socket() {
        let mock = MockSonos::with_config(MockConfig {
            coordinator: Some("RINCON_COORDINATOR".to_owned()),
            ..MockConfig::default()
        })
        .await;

        let coordinator =
            SoapControl::new().unwrap().coordinator_of(&device_at(mock.addr)).await.unwrap();
        assert_eq!(coordinator, "RINCON_COORDINATOR");
        mock.shutdown().await;
    }

    /// Covers: RQ-DISC-006, RQ-DISC-007
    #[tokio::test]
    async fn rq_disc_007_a_real_description_parses_to_the_root_device() {
        let mock = MockSonos::start().await;
        let client = rincon_discovery::fetch::client().unwrap();
        let url = url::Url::parse(&mock.location()).unwrap();

        let body = rincon_discovery::fetch::get_bounded(
            &client,
            url,
            rincon_core::limits::Budget::DESCRIPTION,
        )
        .await
        .unwrap();

        let described = rincon_discovery::description::parse(&body).unwrap();
        assert_eq!(described.room.as_str(), "Kitchen");
        assert_ne!(described.room.as_str(), "WRONG ROOM", "the sub-device hijacked the identity");
        assert!(described.is_zone_player());
        mock.shutdown().await;
    }

    #[tokio::test]
    async fn the_mock_serves_nothing_it_was_not_asked_to() {
        let mock = MockSonos::start().await;
        let response = reqwest::get(format!("http://{}/admin", mock.addr)).await.unwrap();
        assert_eq!(response.status(), 404);
        mock.shutdown().await;
    }
}
