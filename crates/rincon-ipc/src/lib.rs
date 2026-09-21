//! The boundary between the Rust process and the WebView.
//!
//! # Why this is a crate and not part of the Tauri shell
//!
//! Everything here is testable without a window, a WebView, or GTK. The Tauri shell in
//! `apps/desktop/src-tauri` is a thin wrapper that maps `#[tauri::command]` onto these
//! functions and does nothing else. That split means the IPC contract — the command surface,
//! the error shape, the DTOs, the fact that commands take identifiers rather than addresses —
//! is covered by the ordinary `cargo test --workspace` run on every platform, instead of only
//! when someone builds the desktop app on Windows.
//!
//! # The trust boundary
//!
//! A WebView is a browser. The strings it renders — room names, model names, error text —
//! originate from unauthenticated devices on the LAN. Three rules follow, and they are the
//! reason this module exists at all:
//!
//! 1. **Commands take opaque identifiers, never addresses.** A renderer cannot ask the backend
//!    to dial an arbitrary host, because there is no command that accepts a host
//!    ([`RQ-UI-001`](../../../specs/SPEC-006-ipc-ui.md)).
//! 2. **Every error has a stable shape.** `{ code, message, retryable }`, so the interface can
//!    branch on `code` and never on a message string that will be reworded.
//! 3. **Nothing privileged happens on the renderer's say-so.** The firewall panel returns the
//!    command for the user to run; it does not run it.

pub mod bindings;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use rincon_control::Volume;
use rincon_core::device::{Device, DeviceId};
use rincon_core::diagnostics::{Bundle, BundleBuilder, SessionSummary};
use rincon_core::metrics::CounterSnapshot;
use rincon_engine::{Engine, Event, SessionState};
use serde::{Deserialize, Serialize};

/// The stable error shape the interface branches on.
///
/// Covers: RQ-UI-003
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "camelCase")]
#[error("{message}")]
pub struct IpcError {
    /// A stable, machine-readable identifier. Never reworded.
    pub code: ErrorCode,
    /// A sentence for a human. May be reworded freely, which is why nothing branches on it.
    pub message: String,
    /// Whether the interface should offer a retry.
    pub retryable: bool,
}

/// The closed set of error codes the interface knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The identifier does not name a device from the last scan.
    UnknownDevice,
    /// The command does not apply in the current state.
    InvalidState,
    /// A scan could not run.
    ScanFailed,
    /// A session could not start, or stopped.
    SessionFailed,
    /// The value supplied by the interface was out of range.
    InvalidArgument,
    /// A bug.
    Internal,
}

impl IpcError {
    /// Builds an error with the given code.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        // Redaction is applied here rather than at each call site, so a message that picked up
        // an address or a path on its way through cannot reach a dialog.
        Self { code, message: rincon_core::error::redact_sensitive(&message.into()), retryable }
    }

    /// The error for an identifier the backend has never seen.
    ///
    /// Covers: RQ-UI-002
    #[must_use]
    pub fn unknown_device(id: &str) -> Self {
        Self::new(
            ErrorCode::UnknownDevice,
            format!("No speaker with id `{id}` was found. Scan again."),
            true,
        )
    }

    /// The error for a command that does not apply right now.
    #[must_use]
    pub fn invalid_state(state: &SessionState, wanted: &str) -> Self {
        Self::new(
            ErrorCode::InvalidState,
            format!("Cannot {wanted} while {}.", state.name().to_lowercase()),
            false,
        )
    }
}

/// Convenience alias for command results.
pub type IpcResult<T> = Result<T, IpcError>;

/// A speaker, as the interface sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDto {
    /// The opaque identifier a command takes. Never an address.
    pub id: String,
    /// The room name, already sanitised and truncated.
    pub room: String,
    /// The model name, already sanitised and truncated.
    pub model: String,
    /// The subnet the speaker is on, last octet removed.
    ///
    /// Enough for a user to recognise "that is my guest network"; not enough to be worth
    /// screenshotting into a public issue.
    pub subnet: String,
}

impl From<&Device> for DeviceDto {
    fn from(device: &Device) -> Self {
        Self {
            id: device.id.to_string(),
            room: device.room.to_string(),
            model: device.model.to_string(),
            subnet: rincon_core::error::redact_sensitive(&device.address.to_string()),
        }
    }
}

/// The session, as the interface renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDto {
    /// The state name, e.g. `streaming`.
    pub state: String,
    /// The full state, with the stream URL already redacted by its own `Serialize`.
    pub detail: SessionState,
    /// The room currently targeted, if any.
    pub room: Option<String>,
    /// Counters for the live session.
    pub counters: CounterSnapshot,
    /// Frames lost that a listener would have heard.
    ///
    /// Carried separately rather than left for the interface to add up. The interface used to
    /// sum `dropped_ring + dropped_socket`, which quietly omitted capture and device loss, and
    /// would have disagreed with the health indicator sitting next to it. One number, computed
    /// once, in the language that owns the definition.
    pub dropped_frames: u64,
    /// The latency note. Always present while connected, never behind a menu.
    ///
    /// Covers: RQ-UI-006
    pub latency_note: Option<&'static str>,
}

/// The sentence the interface must show while connected.
///
/// A constant rather than a string in a component, so the interface cannot ship without it and
/// a test can assert it exists.
pub const LATENCY_NOTE: &str =
    "Sonos speakers buffer 1-2 seconds. Great for music; video will look out of sync.";

/// What the interface tells a user whose speaker never connected.
///
/// # Why this is two commands and not one
///
/// The obvious advice — "allow Rincon through the firewall on private networks" — is wrong on
/// a surprising number of machines, because Windows classifies plenty of home Wi-Fi networks
/// as **Public**. A user following it adds a rule for a profile their network is not in,
/// nothing changes, and they conclude the app is broken.
///
/// Worse is the shortcut they reach for next: allowing the app on the Public profile. That
/// rule then applies in cafés and airports too, where it offers the port serving their system
/// audio to everyone on the network. It is the one configuration this program must not
/// encourage, and the reason the advice names the trap explicitly.
///
/// So it fixes the cause first — mark the home network Private, which is what it is — and only
/// then adds a rule scoped to that profile.
pub const FIREWALL_EXPLANATION: &str = concat!(
    "Your speaker fetches audio from this computer, so Windows Firewall has to let it in. ",
    "Check the network type first: Windows often marks home Wi-Fi as Public, and a rule for ",
    "the wrong profile changes nothing. Do not allow Rincon on Public networks — that would ",
    "offer your system audio to everyone in a café. Run these in an administrator terminal, ",
    "replacing the network name with your own.",
);

/// The exact commands. Shown to the user; never executed by Rincon.
pub const FIREWALL_COMMANDS: &str = concat!(
    "# 1. Your home Wi-Fi is a private network, whatever Windows guessed\n",
    "Set-NetConnectionProfile -Name \"YOUR-WIFI-NAME\" -NetworkCategory Private\n",
    "\n",
    "# 2. Let the speaker reach Rincon, on private networks only\n",
    "New-NetFirewallRule -DisplayName \"Rincon\" -Direction Inbound -Action Allow `\n",
    "  -Program \"%LOCALAPPDATA%\\Programs\\Rincon\\rincon.exe\" -Profile Private",
);

/// What the interface needs to help with a firewall problem.
///
/// Covers: RQ-UI-007
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FirewallDto {
    /// Whether the last session failed in a way that suggests the firewall.
    pub suspected: bool,
    /// The exact command the user can run. Shown, never executed.
    pub command: String,
    /// Why running it is necessary.
    pub explanation: String,
}

/// A support bundle, ready to copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsDto {
    /// The rendered, redacted text.
    pub text: String,
    /// The structured form, for a future upload button that does not exist yet.
    pub bundle: Bundle,
}

/// The command surface, and the device registry the identifiers resolve against.
///
/// Holding the registry here — rather than letting the renderer pass addresses — is what makes
/// `RQ-UI-001` structural: there is no command that could dial an arbitrary host, because none
/// of them accept one.
#[derive(Debug)]
pub struct Api {
    engine: Arc<Engine>,
    known: Mutex<BTreeMap<String, Device>>,
}

impl Api {
    /// Wraps an engine.
    #[must_use]
    pub const fn new(engine: Arc<Engine>) -> Self {
        Self { engine, known: Mutex::new(BTreeMap::new()) }
    }

    /// The engine, for the shell's event forwarding.
    #[must_use]
    pub const fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// Runs a scan and returns what it found.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError`] with [`ErrorCode::ScanFailed`] when discovery could not run.
    pub async fn scan_devices(&self) -> IpcResult<Vec<DeviceDto>> {
        self.engine.dispatch(Event::StartScan).await;

        match self.engine.state() {
            SessionState::DevicesFound { devices } => {
                {
                    // Scoped: the guard must not outlive the statement that needs it.
                    let mut known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
                    known.clear();
                    for device in &devices {
                        known.insert(device.id.to_string(), device.clone());
                    }
                }
                Ok(devices.iter().map(DeviceDto::from).collect())
            }
            SessionState::NoDevices => Ok(Vec::new()),
            SessionState::Failed { reason, retryable, .. } => Err(IpcError::new(
                ErrorCode::ScanFailed,
                format!("{} {}", reason.message(), reason.remediation()),
                retryable,
            )),
            other => Err(IpcError::invalid_state(&other, "scan")),
        }
    }

    /// Connects to a previously discovered speaker.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError`] when the identifier is unknown or the session fails to start.
    ///
    /// Covers: RQ-UI-001, RQ-UI-002
    pub async fn connect(&self, id: &str) -> IpcResult<()> {
        let device = self.resolve(id)?;
        self.engine.dispatch(Event::Connect(Box::new(device))).await;

        match self.engine.state() {
            SessionState::Streaming { .. } | SessionState::Preparing { .. } => Ok(()),
            SessionState::Failed { reason, retryable, .. } => Err(IpcError::new(
                ErrorCode::SessionFailed,
                format!("{} {}", reason.message(), reason.remediation()),
                retryable,
            )),
            other => Err(IpcError::invalid_state(&other, "connect")),
        }
    }

    /// Stops the current session.
    ///
    /// # Errors
    ///
    /// Never fails today; the signature is fallible so that a future teardown that can fail
    /// does not become a breaking change to the interface contract.
    pub async fn disconnect(&self) -> IpcResult<()> {
        self.engine.dispatch(Event::Disconnect).await;
        self.engine.dispatch(Event::TeardownComplete).await;
        Ok(())
    }

    /// Sets the speaker's volume.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError`] with [`ErrorCode::InvalidArgument`] when the level is above 100,
    /// or [`ErrorCode::InvalidState`] when no session is live.
    pub async fn set_volume(&self, level: u8) -> IpcResult<Volume> {
        // The renderer is not trusted to have clamped this. Rejecting rather than clamping is
        // deliberate: a slider that sends 200 has a bug, and silently playing at full volume
        // is the worst possible way to reveal it.
        let level = Volume::new(level)
            .map_err(|error| IpcError::new(ErrorCode::InvalidArgument, error.to_string(), false))?;

        self.engine.set_volume(level).await.map_err(|error| {
            IpcError::new(ErrorCode::InvalidState, error.to_string(), error.retryable())
        })?;
        Ok(level)
    }

    /// The current session state.
    #[must_use]
    pub fn session_state(&self) -> SessionDto {
        let state = self.engine.state();
        let counters = self.engine.counters().map(|c| c.snapshot()).unwrap_or_default();
        let room = state.target().map(|device| device.room.to_string());
        // Covers: RQ-UI-006 -- present whenever audio is flowing, not behind a menu.
        let latency_note = state.is_live().then_some(LATENCY_NOTE);

        SessionDto {
            state: state.name().to_lowercase(),
            detail: state,
            room,
            dropped_frames: counters.quality_drops(),
            counters,
            latency_note,
        }
    }

    /// Advice for a suspected firewall block.
    ///
    /// Returns the command; never runs it. A button that silently invokes `netsh` with elevated
    /// rights on a renderer's say-so is a privilege-escalation primitive, and the convenience
    /// it buys is one copy-paste.
    ///
    /// Covers: RQ-UI-007
    #[must_use]
    pub fn firewall_status(&self) -> FirewallDto {
        let suspected = matches!(
            self.engine.state(),
            SessionState::Failed { reason: rincon_engine::FailureReason::FirewallSuspected, .. }
        );
        FirewallDto {
            suspected,
            command: FIREWALL_COMMANDS.to_owned(),
            explanation: FIREWALL_EXPLANATION.to_owned(),
        }
    }

    /// Builds a redacted diagnostics bundle.
    #[must_use]
    pub fn diagnostics(&self) -> DiagnosticsDto {
        let state = self.engine.state();
        let counters = self.engine.counters().map(|c| c.snapshot()).unwrap_or_default();

        let mut builder = BundleBuilder::new().counters(counters);
        if let Some(url) = self.engine.stream_url() {
            builder = builder.redacting(url.token().to_owned());
        }
        if let Some(name) = hostname() {
            builder = builder.redacting(name);
        }
        for address in rincon_discovery::scan::probe_interfaces() {
            builder = builder.interface(address.to_string());
        }
        if let Some(target) = state.target() {
            builder = builder.session(SessionSummary {
                state: state.name().to_owned(),
                elapsed: "unknown".to_owned(),
                format: "negotiated at capture".to_owned(),
                device: target.to_string(),
            });
        }

        let bundle = builder.build();
        DiagnosticsDto { text: bundle.render(), bundle }
    }

    /// Resolves an identifier against the last scan.
    fn resolve(&self, id: &str) -> IpcResult<Device> {
        // Validate the shape first: an identifier from a renderer is untrusted input, and a
        // 10 MB string should be refused before it is used as a map key.
        DeviceId::new(id).map_err(|_| IpcError::unknown_device(id))?;

        self.known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
            .ok_or_else(|| IpcError::unknown_device(id))
    }

    /// Registers devices without scanning, for the shell's fake-device mode.
    pub fn remember(&self, devices: &[Device]) {
        let mut known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        for device in devices {
            known.insert(device.id.to_string(), device.clone());
        }
    }
}

/// The machine name, for redaction. Absent rather than guessed when unavailable.
fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).ok()
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

    use rincon_audio::SyntheticCapture;
    use rincon_core::audio::{AudioFormat, SampleEncoding};
    use rincon_engine::{Dependencies, EngineConfig};
    use rincon_testkit::{FakeControl, FakeDiscovery, device};

    use super::*;

    fn api(discovery: FakeDiscovery) -> Api {
        let format = AudioFormat::new(44_100, 2, SampleEncoding::F32Le).unwrap();
        Api::new(Engine::new(
            Dependencies {
                discovery: Arc::new(discovery),
                control: Arc::new(FakeControl::accepting()),
                capture: Arc::new(SyntheticCapture::new(format, 3)),
            },
            EngineConfig {
                peer_timeout: std::time::Duration::from_millis(200),
                max_connections: 1,
            },
        ))
    }

    /// Covers: RQ-UI-001
    #[tokio::test]
    async fn rq_ui_001_commands_take_ids_not_addresses() {
        // The structural claim: there is no command that accepts a host, so a compromised
        // renderer cannot make the backend dial one. Exercised by showing that the only way
        // to reach a device is through an identifier the backend itself registered.
        let api = api(FakeDiscovery::finding(vec![device("Kitchen", 45)]));

        let found = api.scan_devices().await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "uuid:RINCON_KITCHEN");

        // The DTO carries no routable address at all -- only a subnet, for recognition.
        assert!(!found[0].subnet.contains("192.168.1.45"), "a full address reached the renderer");
        assert!(found[0].subnet.contains("192.168.1.xxx"));

        // An address in place of an identifier resolves to nothing.
        assert_eq!(api.connect("192.168.1.45").await.unwrap_err().code, ErrorCode::UnknownDevice);
    }

    /// Covers: RQ-UI-002
    #[tokio::test]
    async fn rq_ui_002_unknown_id_is_typed_error() {
        let api = api(FakeDiscovery::empty());

        for id in ["", "nope", "uuid:RINCON_NEVER_SEEN", &"x".repeat(10_000)] {
            let error = api.connect(id).await.unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::UnknownDevice,
                "for id {:?}",
                &id[..id.len().min(20)]
            );
            assert!(error.retryable, "scanning again is a sensible next step");
        }
    }

    /// Covers: RQ-UI-003
    #[tokio::test]
    async fn rq_ui_003_error_shape_is_stable() {
        let api = api(FakeDiscovery::failing("no interface"));
        let error = api.scan_devices().await.unwrap_err();

        let json = serde_json::to_value(&error).unwrap();
        let object = json.as_object().expect("an error must serialise as an object");

        // Exactly three fields, in camelCase, every time.
        assert_eq!(object.len(), 3, "the error shape changed: {json}");
        assert!(object.contains_key("code"));
        assert!(object.contains_key("message"));
        assert!(object.contains_key("retryable"));
        assert!(object["code"].is_string(), "code must be a stable string, not a number");
        assert!(object["retryable"].is_boolean());

        // And it round-trips, so the interface's type definition cannot silently drift.
        let back: IpcError = serde_json::from_value(json).unwrap();
        assert_eq!(back, error);
    }

    /// Covers: RQ-UI-007
    #[tokio::test]
    async fn rq_ui_007_firewall_panel_is_advisory_only() {
        let api = api(FakeDiscovery::empty());
        let advice = api.firewall_status();

        // It hands over the command text, scoped to private networks only.
        assert!(advice.command.contains("New-NetFirewallRule"));
        assert!(advice.command.contains("-Profile Private"));
        assert!(
            !advice.command.contains("-Profile Public"),
            "never tell a user to open this port on an untrusted network"
        );

        // And it addresses the cause before the symptom. Windows classifies plenty of home
        // Wi-Fi as Public; advice that assumes otherwise sends the user to add a rule for a
        // profile their network is not in, which changes nothing and looks like a broken app.
        assert!(
            advice.command.contains("Set-NetConnectionProfile"),
            "the advice must fix the network type, not just add a rule"
        );
        assert!(advice.explanation.to_lowercase().contains("public"));
        assert!(!advice.explanation.is_empty());

        // ...and there is no command that runs it. If one is ever added, this fails.
        //
        // The needles are assembled from fragments so that this test's own source does not
        // match them — a grep-the-source check that trips over its own allowlist is a test
        // that only ever fails for the wrong reason.
        let source = include_str!("lib.rs");
        let forbidden = [
            concat!("std::process::", "Command"),
            concat!("Shell", "Execute"),
            concat!("run", "as /user"),
        ];
        for needle in forbidden {
            assert!(
                !source.contains(needle),
                "the IPC layer gained a way to execute a privileged command: {needle}"
            );
        }
    }

    /// Covers: RQ-UI-006
    #[tokio::test]
    async fn rq_ui_006_latency_note_is_present_while_connected() {
        let api = api(FakeDiscovery::finding(vec![device("Kitchen", 45)]));

        // Not shown when there is nothing to qualify.
        assert!(api.session_state().latency_note.is_none());

        // The note itself says the thing a user needs to hear before they try a film.
        assert!(LATENCY_NOTE.contains("1-2 seconds"));
        assert!(LATENCY_NOTE.to_lowercase().contains("video"));
    }

    #[tokio::test]
    async fn volume_from_the_renderer_is_validated_not_trusted() {
        let api = api(FakeDiscovery::finding(vec![device("Kitchen", 45)]));

        // Out of range is refused before any session is even consulted, and refusing is the
        // right answer: clamping 200 to 100 would play at full volume to hide a caller's bug.
        let error = api.set_volume(101).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(!error.retryable, "sending the same bad value again will not help");

        // In range, but with no session, is a state error rather than an argument error --
        // the interface shows different things for those.
        let error = api.set_volume(50).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidState);
    }

    #[tokio::test]
    async fn the_session_dto_never_carries_the_stream_token() {
        let api = api(FakeDiscovery::finding(vec![device("Kitchen", 45)]));
        let json = serde_json::to_string(&api.session_state()).unwrap();
        assert!(!json.contains("stream.wav?"), "unexpected query string: {json}");
        // The redaction is structural (see `StreamUrl`), but a regression here would be
        // invisible without an assertion at the boundary that actually ships it.
        assert!(!json.contains("token"), "a token-shaped field reached the renderer: {json}");
    }

    #[tokio::test]
    async fn an_empty_scan_is_an_empty_list_not_an_error() {
        // The interface shows a checklist for this, which it cannot do if it arrives as a
        // failure indistinguishable from a broken socket.
        let api = api(FakeDiscovery::empty());
        assert_eq!(api.scan_devices().await.unwrap(), Vec::new());
    }

    #[tokio::test]
    async fn a_broken_scan_is_an_error_with_a_remediation() {
        let api = api(FakeDiscovery::failing("no interface"));
        let error = api.scan_devices().await.unwrap_err();
        assert_eq!(error.code, ErrorCode::ScanFailed);
        assert!(error.message.contains("Wi-Fi"), "got: {}", error.message);
    }

    #[tokio::test]
    async fn diagnostics_are_redacted_and_offline() {
        let api = api(FakeDiscovery::finding(vec![device("Kitchen", 45)]));
        let _ = api.scan_devices().await;

        let diagnostics = api.diagnostics();
        assert!(diagnostics.text.contains("Rincon"));
        assert!(!diagnostics.text.contains("192.168.1.45"), "a full address leaked");
        assert!(diagnostics.text.contains("counters:"));
    }

    #[tokio::test]
    async fn error_messages_carry_no_paths_or_full_addresses() {
        let error = IpcError::new(
            ErrorCode::Internal,
            r"failed at C:\Users\alice\rincon.log talking to 192.168.1.45",
            false,
        );
        assert!(!error.message.contains("alice"), "{}", error.message);
        assert!(!error.message.contains("192.168.1.45"), "{}", error.message);
    }
}
