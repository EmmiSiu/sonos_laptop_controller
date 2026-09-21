//! The session state machine, as a pure function.
//!
//! # Why this is a reducer and not `async` code
//!
//! A session has to sequence four subsystems under failure: capture must be running before the
//! server has anything to serve, the server must be bound before the speaker is handed a URL,
//! the URL must be reachable from the speaker's subnet, and any of the four can vanish at any
//! moment. Written as ad-hoc `async` code with `if let Err` at each step, that becomes a
//! tangle nobody can review and nobody can test without a speaker in the room.
//!
//! So the whole thing is one pure function:
//!
//! ```text
//!   step(state, event) -> (next_state, [effects])
//! ```
//!
//! No I/O. No clock. No allocation beyond the effects it returns. Every transition that can
//! happen is enumerated in one `match`, which means every transition can be unit-tested in
//! microseconds, and a transition that is not written down cannot occur.
//!
//! The [`crate::driver`] executes the effects. It is deliberately dull: its only job is to call
//! the other crates and feed their answers back as events.

use rincon_core::audio::AudioFormat;
use rincon_core::device::Device;
use rincon_core::metrics::Health;
use rincon_core::stream_url::StreamUrl;
use serde::{Deserialize, Serialize};

/// Where a preparation sequence has got to.
///
/// Surfaced in the interface as a five-step progress indicator. That is not decoration: when a
/// connection fails, the user has already seen which step it reached, which is most of the
/// diagnosis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrepareStep {
    /// Asking the household which speaker leads this one's group.
    ResolveCoordinator,
    /// Opening the system audio endpoint.
    StartCapture,
    /// Binding the HTTP server to the interface that reached the speaker.
    BindServer,
    /// Telling the speaker where to fetch audio from.
    SetUri,
    /// Telling the speaker to start.
    Play,
    /// Waiting for the speaker to actually connect.
    AwaitPeer,
}

impl PrepareStep {
    /// Every step, in the order they must run.
    pub const ORDER: [Self; 6] = [
        Self::ResolveCoordinator,
        Self::StartCapture,
        Self::BindServer,
        Self::SetUri,
        Self::Play,
        Self::AwaitPeer,
    ];

    /// Position in the sequence, for a progress indicator.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::ResolveCoordinator => 0,
            Self::StartCapture => 1,
            Self::BindServer => 2,
            Self::SetUri => 3,
            Self::Play => 4,
            Self::AwaitPeer => 5,
        }
    }

    /// A sentence for the interface.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ResolveCoordinator => "Finding the group leader",
            Self::StartCapture => "Opening system audio",
            Self::BindServer => "Starting the local stream",
            Self::SetUri => "Pointing the speaker at it",
            Self::Play => "Starting playback",
            Self::AwaitPeer => "Waiting for the speaker to connect",
        }
    }
}

/// Why a session failed, in terms that map to one sentence and one remediation each.
///
/// A closed enum, deliberately. This is what stops "Something went wrong" ever reaching a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum FailureReason {
    /// No audio playback endpoint exists on this machine.
    NoCaptureDevice,
    /// Another application holds the audio endpoint exclusively.
    CaptureBusy,
    /// The speaker did not answer.
    DeviceUnreachable,
    /// The speaker refused a command.
    RejectedByDevice {
        /// The UPnP code, so a bug report carries it.
        upnp_code: u16,
        /// What the device said.
        description: String,
    },
    /// We bound, we played, and the speaker never connected. Almost always the firewall.
    FirewallSuspected,
    /// The speaker is on a network segment that cannot reach us.
    NetworkIsolated,
    /// Discovery itself could not run.
    ScanFailed {
        /// Diagnostic detail, already redacted.
        detail: String,
    },
    /// A bug.
    Internal {
        /// Diagnostic detail, already redacted.
        detail: String,
    },
}

impl FailureReason {
    /// Whether retrying could plausibly succeed.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            Self::NoCaptureDevice
            | Self::CaptureBusy
            | Self::DeviceUnreachable
            | Self::FirewallSuspected
            | Self::ScanFailed { .. } => true,
            // 701 is "busy, try later"; most other codes will not change on a retry.
            Self::RejectedByDevice { upnp_code, .. } => *upnp_code == 701,
            Self::NetworkIsolated | Self::Internal { .. } => false,
        }
    }

    /// The one sentence the interface shows.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        match self {
            Self::NoCaptureDevice => "No playback device found. Connect speakers or headphones.",
            Self::CaptureBusy => "Another app has exclusive control of the audio device.",
            Self::DeviceUnreachable => "The speaker stopped responding.",
            Self::RejectedByDevice { .. } => "The speaker refused the command.",
            Self::FirewallSuspected => "The speaker could not reach your computer.",
            Self::NetworkIsolated => "Your Wi-Fi is keeping this computer and the speaker apart.",
            Self::ScanFailed { .. } => "Could not search the network.",
            Self::Internal { .. } => "Something went wrong inside Rincon.",
        }
    }

    /// What the user can do about it.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::NoCaptureDevice => "Open Windows sound settings and choose an output device.",
            Self::CaptureBusy => "Close the app using the audio device, then try again.",
            Self::DeviceUnreachable => "Rescan the network.",
            Self::RejectedByDevice { .. } => "Stop what the speaker is playing, then try again.",
            Self::FirewallSuspected => "Allow Rincon through Windows Firewall on private networks.",
            Self::NetworkIsolated => {
                "Turn off AP isolation, or put both devices on the same network."
            }
            Self::ScanFailed { .. } => "Check that Wi-Fi is connected, then try again.",
            Self::Internal { .. } => "Copy a diagnostics bundle and open an issue.",
        }
    }
}

/// Why a healthy session became a degraded one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradeReason {
    /// The ring emptied: the machine is under load or the network is slow.
    Underrun,
    /// Frames were lost.
    FramesDropped,
    /// The speaker disconnected and we are waiting for it to come back.
    PeerReconnecting,
}

/// The authoritative session state. The interface renders this directly.
///
/// `Serialize` only: this crosses the IPC boundary outward, and the `StreamUrl` it carries
/// serialises redacted. There is no inward direction to deserialise — the interface sends
/// commands, never states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum SessionState {
    /// Nothing is happening.
    Idle,
    /// A scan is running.
    Scanning,
    /// A scan found speakers.
    DevicesFound {
        /// What it found, sorted by room name.
        devices: Vec<Device>,
    },
    /// A scan found nothing.
    NoDevices,
    /// A session is being set up.
    Preparing {
        /// The speaker we are connecting to.
        target: Box<Device>,
        /// Where the sequence has got to.
        step: PrepareStep,
        /// The stream URL, once the server has bound.
        ///
        /// Carried through the remaining steps rather than stashed in the driver: a reducer
        /// that loses what it learned two steps ago is not really the source of truth.
        url: Option<StreamUrl>,
        /// The negotiated capture format, once capture has started.
        format: Option<AudioFormat>,
    },
    /// Audio is flowing.
    Streaming {
        /// The speaker.
        target: Box<Device>,
        /// The URL it is fetching. Renders redacted.
        url: StreamUrl,
        /// Current health.
        health: Health,
        /// The negotiated capture format.
        format: AudioFormat,
    },
    /// Audio is flowing, badly.
    Degraded {
        /// The speaker.
        target: Box<Device>,
        /// The URL it is fetching.
        url: StreamUrl,
        /// The negotiated capture format.
        format: AudioFormat,
        /// Why.
        reason: DegradeReason,
    },
    /// The session is being torn down.
    Stopping {
        /// The speaker.
        target: Box<Device>,
    },
    /// The session failed.
    Failed {
        /// Why.
        reason: FailureReason,
        /// Whether a retry could work.
        retryable: bool,
        /// The speaker, so a retry knows where to go.
        target: Option<Box<Device>>,
    },
}

impl SessionState {
    /// A short name for logs and diagnostics.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Scanning => "Scanning",
            Self::DevicesFound { .. } => "DevicesFound",
            Self::NoDevices => "NoDevices",
            Self::Preparing { .. } => "Preparing",
            Self::Streaming { .. } => "Streaming",
            Self::Degraded { .. } => "Degraded",
            Self::Stopping { .. } => "Stopping",
            Self::Failed { .. } => "Failed",
        }
    }

    /// Whether audio is currently being served.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(self, Self::Streaming { .. } | Self::Degraded { .. })
    }

    /// The speaker this state concerns, if any.
    #[must_use]
    pub const fn target(&self) -> Option<&Device> {
        match self {
            Self::Preparing { target, .. }
            | Self::Streaming { target, .. }
            | Self::Degraded { target, .. }
            | Self::Stopping { target }
            | Self::Failed { target: Some(target), .. } => Some(target),
            Self::Idle
            | Self::Scanning
            | Self::DevicesFound { .. }
            | Self::NoDevices
            | Self::Failed { target: None, .. } => None,
        }
    }
}

/// Everything that can happen to a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The user asked to scan.
    StartScan,
    /// A scan finished.
    ScanCompleted(Vec<Device>),
    /// A scan could not run.
    ScanFailed(String),
    /// The user picked a speaker.
    Connect(Box<Device>),
    /// The coordinator for the chosen speaker is known.
    CoordinatorResolved(Box<Device>),
    /// Capture is running at this format.
    CaptureStarted(AudioFormat),
    /// The server is bound at this URL.
    ServerBound(StreamUrl),
    /// The speaker accepted the URL.
    UriAccepted,
    /// The speaker accepted `Play`.
    PlaybackStarted,
    /// The speaker fetched the stream.
    PeerConnected,
    /// The speaker never fetched the stream.
    PeerConnectTimeout,
    /// The speaker stopped fetching the stream.
    PeerLost,
    /// A preparation step failed.
    PrepareFailed(FailureReason),
    /// Quality dropped.
    HealthDropped(DegradeReason),
    /// Quality recovered.
    HealthRecovered,
    /// The user asked to stop.
    Disconnect,
    /// Teardown finished.
    TeardownComplete,
    /// The user dismissed a failure.
    Acknowledge,
    /// The user asked to retry a failure.
    Retry,
}

/// Something the driver must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Run a discovery scan.
    Scan,
    /// Ask the household who coordinates this speaker.
    ResolveCoordinator(Box<Device>),
    /// Open the system audio endpoint.
    StartCapture,
    /// Bind the HTTP server on the interface that reached this speaker.
    BindServer(Box<Device>),
    /// Hand the speaker the stream URL.
    SetUri {
        /// The speaker.
        target: Box<Device>,
        /// The URL.
        url: StreamUrl,
    },
    /// Tell the speaker to play.
    Play(Box<Device>),
    /// Start the timer that decides "firewall" versus "still buffering".
    ArmPeerTimeout,
    /// Tell the speaker to stop.
    StopPlayback(Box<Device>),
    /// Shut the HTTP server down.
    StopServer,
    /// Close the audio endpoint.
    StopCapture,
}

/// The result of one transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    /// The state afterwards.
    pub state: SessionState,
    /// What the driver must do, in order.
    pub effects: Vec<Effect>,
    /// Whether anything actually changed. `false` means the event was inapplicable.
    pub changed: bool,
}

impl Transition {
    const fn to(state: SessionState, effects: Vec<Effect>) -> Self {
        Self { state, effects, changed: true }
    }

    const fn inert(state: SessionState) -> Self {
        Self { state, effects: Vec::new(), changed: false }
    }
}

/// The teardown every exit from a live session must perform.
///
/// Factored out precisely so that no exit path can forget one of them. `RQ-ENG-006` asserts
/// that every route out of `Streaming` and `Degraded` emits all three.
fn teardown(target: &Device) -> Vec<Effect> {
    vec![Effect::StopPlayback(Box::new(target.clone())), Effect::StopServer, Effect::StopCapture]
}

/// Applies `event` to `state`.
///
/// Pure: the same inputs always produce the same outputs. An inapplicable event leaves the
/// state untouched and emits nothing, so a late reply from a cancelled operation cannot
/// resurrect a session the user already stopped.
///
/// Covers: RQ-ENG-001, RQ-ENG-002, RQ-ENG-003, RQ-ENG-004, RQ-ENG-005, RQ-ENG-006, RQ-ENG-007
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "the whole point of this function is that every transition is visible in one \
              match; splitting it would hide exactly what a reviewer needs to see at once"
)]
#[expect(
    clippy::match_same_arms,
    reason = "in a transition table, two arms sharing a body states that two different \
              situations are handled identically; merging them would hide one of them"
)]
pub fn step(state: SessionState, event: Event) -> Transition {
    use Event as E;
    use SessionState as S;

    match (state, event) {
        // ---- scanning ------------------------------------------------------------------
        (S::Idle | S::NoDevices | S::DevicesFound { .. }, E::StartScan) => {
            Transition::to(S::Scanning, vec![Effect::Scan])
        }
        (S::Scanning, E::ScanCompleted(devices)) => {
            if devices.is_empty() {
                Transition::to(S::NoDevices, Vec::new())
            } else {
                Transition::to(S::DevicesFound { devices }, Vec::new())
            }
        }
        (S::Scanning, E::ScanFailed(detail)) => Transition::to(
            S::Failed {
                reason: FailureReason::ScanFailed { detail },
                retryable: true,
                target: None,
            },
            Vec::new(),
        ),

        // ---- preparing: the fixed five-step sequence -----------------------------------
        // Connecting is allowed from every state that is not already mid-session: the user
        // may pick a speaker straight from a failure, or before ever scanning.
        (
            S::Idle | S::NoDevices | S::DevicesFound { .. } | S::Failed { .. },
            E::Connect(target),
        ) => {
            let effect = Effect::ResolveCoordinator(target.clone());
            Transition::to(
                S::Preparing {
                    target,
                    step: PrepareStep::ResolveCoordinator,
                    url: None,
                    format: None,
                },
                vec![effect],
            )
        }
        (
            S::Preparing { step: PrepareStep::ResolveCoordinator, .. },
            E::CoordinatorResolved(coordinator),
        ) => Transition::to(
            // The coordinator replaces the target from here on: a grouped member ignores
            // transport commands, and sending them anyway is the single most likely way a
            // first session fails in a household that uses grouping.
            S::Preparing {
                target: coordinator,
                step: PrepareStep::StartCapture,
                url: None,
                format: None,
            },
            vec![Effect::StartCapture],
        ),
        (
            S::Preparing { target, step: PrepareStep::StartCapture, .. },
            E::CaptureStarted(format),
        ) => {
            let effect = Effect::BindServer(target.clone());
            Transition::to(
                S::Preparing {
                    target,
                    step: PrepareStep::BindServer,
                    url: None,
                    format: Some(format),
                },
                vec![effect],
            )
        }
        (
            S::Preparing { target, step: PrepareStep::BindServer, format, .. },
            E::ServerBound(url),
        ) => {
            let effect = Effect::SetUri { target: target.clone(), url: url.clone() };
            Transition::to(
                S::Preparing { target, step: PrepareStep::SetUri, url: Some(url), format },
                vec![effect],
            )
        }
        (S::Preparing { target, step: PrepareStep::SetUri, url, format }, E::UriAccepted) => {
            let effect = Effect::Play(target.clone());
            Transition::to(
                S::Preparing { target, step: PrepareStep::Play, url, format },
                vec![effect],
            )
        }
        (S::Preparing { target, step: PrepareStep::Play, url, format }, E::PlaybackStarted) => {
            Transition::to(
                S::Preparing { target, step: PrepareStep::AwaitPeer, url, format },
                vec![Effect::ArmPeerTimeout],
            )
        }

        (S::Preparing { target, step: PrepareStep::AwaitPeer, url, format }, E::PeerConnected) => {
            // Unreachable while the sequence is followed, because `AwaitPeer` is only reached
            // through `ServerBound`. Reported as an internal failure rather than papered over
            // with a placeholder URL, which would be a wrong value the interface then shows as
            // if it were real.
            let Some(url) = url else {
                let effects = teardown(&target);
                return Transition::to(
                    S::Failed {
                        reason: FailureReason::Internal {
                            detail: "reached AwaitPeer without a bound stream URL".to_owned(),
                        },
                        retryable: false,
                        target: Some(target),
                    },
                    effects,
                );
            };
            Transition::to(
                S::Streaming {
                    target,
                    url,
                    health: Health::Good,
                    format: format.unwrap_or(AudioFormat::WIRE_DEFAULT),
                },
                Vec::new(),
            )
        }

        // We bound, we played, and nothing fetched the stream. On a home network that is the
        // firewall approximately always, so it gets its own remediation rather than a generic
        // timeout message.
        (S::Preparing { target, step: PrepareStep::AwaitPeer, .. }, E::PeerConnectTimeout) => {
            let effects = teardown(&target);
            Transition::to(
                S::Failed {
                    reason: FailureReason::FirewallSuspected,
                    retryable: true,
                    target: Some(target),
                },
                effects,
            )
        }
        (S::Preparing { target, .. }, E::PrepareFailed(reason)) => {
            // Teardown runs even though preparation never completed: an earlier step may well
            // have succeeded, and a half-built session leaks a capture handle and a port.
            let effects = teardown(&target);
            let retryable = reason.retryable();
            Transition::to(S::Failed { reason, retryable, target: Some(target) }, effects)
        }

        // ---- streaming -----------------------------------------------------------------
        (S::Streaming { target, url, format, .. }, E::HealthDropped(reason)) => {
            // A blip is a degradation, not a failure: the session keeps running and recovers
            // on its own. Treating it as a failure would tear down a working stream.
            Transition::to(S::Degraded { target, url, format, reason }, Vec::new())
        }
        (S::Degraded { target, url, format, .. }, E::HealthRecovered) => {
            Transition::to(S::Streaming { target, url, health: Health::Good, format }, Vec::new())
        }
        (S::Degraded { target, .. }, E::PeerLost) => {
            let effects = teardown(&target);
            Transition::to(
                S::Failed {
                    reason: FailureReason::DeviceUnreachable,
                    retryable: true,
                    target: Some(target),
                },
                effects,
            )
        }
        (S::Streaming { target, url, format, .. }, E::PeerLost) => Transition::to(
            S::Degraded { target, url, format, reason: DegradeReason::PeerReconnecting },
            Vec::new(),
        ),
        // Cancelling mid-preparation and stopping mid-stream are the same teardown: an
        // earlier step may already have opened the device or the port.
        (
            S::Preparing { target, .. } | S::Streaming { target, .. } | S::Degraded { target, .. },
            E::Disconnect,
        ) => {
            let effects = teardown(&target);
            Transition::to(S::Stopping { target }, effects)
        }

        // ---- stopping and failure ------------------------------------------------------
        (S::Stopping { .. }, E::TeardownComplete) => Transition::to(S::Idle, Vec::new()),
        (S::Failed { .. }, E::Acknowledge) => Transition::to(S::Idle, Vec::new()),
        (S::Failed { target: Some(target), .. }, E::Retry) => {
            let effect = Effect::ResolveCoordinator(target.clone());
            Transition::to(
                S::Preparing {
                    target,
                    step: PrepareStep::ResolveCoordinator,
                    url: None,
                    format: None,
                },
                vec![effect],
            )
        }
        (S::Failed { reason, retryable, target: None }, E::Retry) => {
            // Nothing to retry against; a rescan is the only sensible action.
            Transition::inert(S::Failed { reason, retryable, target: None })
        }

        // ---- everything else -----------------------------------------------------------
        // An event that does not apply leaves the session exactly as it was. This is the arm
        // that makes late replies from cancelled operations harmless.
        (state, _) => Transition::inert(state),
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

    use rincon_core::device::{DeviceId, DeviceName};

    use super::*;

    fn device(name: &str) -> Device {
        Device {
            id: DeviceId::new(format!("uuid:RINCON_{}", name.to_uppercase())).unwrap(),
            room: DeviceName::new(name).unwrap(),
            model: DeviceName::new("Sonos One").unwrap(),
            address: "192.168.1.45".parse().unwrap(),
            port: 1400,
            reached_via: Some("192.168.1.20".parse().unwrap()),
        }
    }

    fn url() -> StreamUrl {
        StreamUrl::new("192.168.1.20".parse().unwrap(), 41_234, "0123456789abcdef0123456789abcdef")
    }

    /// Drives a session from `Idle` to `Streaming`, returning every transition on the way.
    fn drive_to_streaming() -> Vec<Transition> {
        let kitchen = device("Kitchen");
        let mut state = SessionState::Idle;
        let mut history = Vec::new();

        for event in [
            Event::Connect(Box::new(kitchen.clone())),
            Event::CoordinatorResolved(Box::new(kitchen)),
            Event::CaptureStarted(AudioFormat::WIRE_DEFAULT),
            Event::ServerBound(url()),
            Event::UriAccepted,
            Event::PlaybackStarted,
            Event::PeerConnected,
        ] {
            let transition = step(state, event);
            state = transition.state.clone();
            history.push(transition);
        }
        history
    }

    /// Covers: RQ-ENG-001
    #[test]
    fn rq_eng_001_step_is_pure() {
        let kitchen = device("Kitchen");
        let state = SessionState::DevicesFound { devices: vec![kitchen.clone()] };
        let event = Event::Connect(Box::new(kitchen));

        let first = step(state.clone(), event.clone());
        let second = step(state.clone(), event.clone());
        let third = step(state, event);

        assert_eq!(first, second, "identical inputs produced different outputs");
        assert_eq!(second, third);
    }

    /// Covers: RQ-ENG-002
    #[test]
    fn rq_eng_002_unknown_pairs_are_inert() {
        // A late reply from an operation the user already cancelled must not resurrect it.
        let cases = [
            (SessionState::Idle, Event::PlaybackStarted),
            (SessionState::Idle, Event::PeerConnected),
            (SessionState::Scanning, Event::UriAccepted),
            (SessionState::NoDevices, Event::HealthRecovered),
            (SessionState::Stopping { target: Box::new(device("Kitchen")) }, Event::PeerConnected),
            (SessionState::Idle, Event::TeardownComplete),
            (SessionState::Idle, Event::Retry),
        ];

        for (state, event) in cases {
            let before = state.clone();
            let transition = step(state, event.clone());
            assert_eq!(transition.state, before, "{event:?} changed the state from {before:?}");
            assert!(transition.effects.is_empty(), "{event:?} emitted effects from {before:?}");
            assert!(!transition.changed, "{event:?} reported a change it did not make");
        }
    }

    /// Covers: RQ-ENG-003
    #[test]
    fn rq_eng_003_prepare_order_is_fixed() {
        let history = drive_to_streaming();

        let steps: Vec<PrepareStep> = history
            .iter()
            .filter_map(|t| match &t.state {
                SessionState::Preparing { step, .. } => Some(*step),
                _ => None,
            })
            .collect();

        assert_eq!(
            steps,
            vec![
                PrepareStep::ResolveCoordinator,
                PrepareStep::StartCapture,
                PrepareStep::BindServer,
                PrepareStep::SetUri,
                PrepareStep::Play,
                PrepareStep::AwaitPeer,
            ],
            "the preparation sequence is not in the order the spec fixes"
        );

        // The effects come out in the same order, which is the part that actually matters:
        // binding the server before capture is running would serve silence.
        let effects: Vec<&Effect> = history.iter().flat_map(|t| &t.effects).collect();
        assert!(matches!(effects.first(), Some(Effect::ResolveCoordinator(_))));
        assert!(matches!(effects.get(1), Some(Effect::StartCapture)));
        assert!(matches!(effects.get(2), Some(Effect::BindServer(_))));
        assert!(matches!(effects.get(3), Some(Effect::SetUri { .. })));
        assert!(matches!(effects.get(4), Some(Effect::Play(_))));
        assert!(matches!(effects.get(5), Some(Effect::ArmPeerTimeout)));

        assert!(matches!(history.last().map(|t| &t.state), Some(SessionState::Streaming { .. })));
    }

    /// Covers: RQ-ENG-004
    #[test]
    fn rq_eng_004_binds_reachable_interface() {
        // The server must be bound using the address discovery actually reached the speaker
        // on. Binding elsewhere produces a URL the speaker cannot route back to, which
        // presents as a firewall problem and wastes an afternoon.
        let history = drive_to_streaming();
        let bind = history
            .iter()
            .flat_map(|t| &t.effects)
            .find_map(|effect| match effect {
                Effect::BindServer(target) => Some(target.clone()),
                _ => None,
            })
            .expect("no BindServer effect was emitted");

        assert_eq!(
            bind.reached_via,
            Some("192.168.1.20".parse().unwrap()),
            "the bind effect lost the interface that reached the speaker"
        );
    }

    /// Covers: RQ-ENG-005
    #[test]
    fn rq_eng_005_firewall_timeout_is_classified() {
        let kitchen = device("Kitchen");
        let awaiting = SessionState::Preparing {
            target: Box::new(kitchen),
            step: PrepareStep::AwaitPeer,
            url: Some(url()),
            format: Some(AudioFormat::WIRE_DEFAULT),
        };

        let transition = step(awaiting, Event::PeerConnectTimeout);

        match &transition.state {
            SessionState::Failed { reason, retryable, target } => {
                assert_eq!(*reason, FailureReason::FirewallSuspected);
                assert!(*retryable, "the user can add a firewall rule and try again");
                assert!(target.is_some(), "a retry needs to know which speaker");
                assert!(reason.remediation().contains("Firewall"));
            }
            other => panic!("expected a classified failure, got {other:?}"),
        }
        // And it must clean up, or the port and the audio endpoint stay held.
        assert_eq!(transition.effects.len(), 3);
    }

    /// Covers: RQ-ENG-006
    #[test]
    fn rq_eng_006_teardown_always_releases_resources() {
        let kitchen = device("Kitchen");
        let live_states = [
            SessionState::Streaming {
                target: Box::new(kitchen.clone()),
                url: url(),
                health: Health::Good,
                format: AudioFormat::WIRE_DEFAULT,
            },
            SessionState::Degraded {
                target: Box::new(kitchen.clone()),
                url: url(),
                format: AudioFormat::WIRE_DEFAULT,
                reason: DegradeReason::Underrun,
            },
            SessionState::Preparing {
                target: Box::new(kitchen),
                step: PrepareStep::AwaitPeer,
                url: Some(url()),
                format: Some(AudioFormat::WIRE_DEFAULT),
            },
        ];

        let exits = [
            Event::Disconnect,
            Event::PeerConnectTimeout,
            Event::PrepareFailed(FailureReason::DeviceUnreachable),
            Event::PeerLost,
        ];

        for state in live_states {
            for event in exits.clone() {
                let before = state.clone();
                let transition = step(state.clone(), event.clone());

                // Only assert on transitions that actually left the live state.
                let left_live = before.is_live() && !transition.state.is_live();
                let left_preparing = matches!(before, SessionState::Preparing { .. })
                    && !matches!(transition.state, SessionState::Preparing { .. })
                    && transition.changed;
                if !(left_live || left_preparing) {
                    continue;
                }

                let has = |wanted: &Effect| transition.effects.iter().any(|e| e == wanted);
                assert!(has(&Effect::StopCapture), "{before:?} + {event:?} left capture running");
                assert!(has(&Effect::StopServer), "{before:?} + {event:?} left the port open");
                assert!(
                    transition.effects.iter().any(|e| matches!(e, Effect::StopPlayback(_))),
                    "{before:?} + {event:?} left the speaker playing"
                );
            }
        }
    }

    /// Covers: RQ-ENG-007
    #[test]
    fn rq_eng_007_underrun_degrades_not_fails() {
        let streaming = SessionState::Streaming {
            target: Box::new(device("Kitchen")),
            url: url(),
            health: Health::Good,
            format: AudioFormat::WIRE_DEFAULT,
        };

        let degraded = step(streaming, Event::HealthDropped(DegradeReason::Underrun));
        assert!(
            matches!(degraded.state, SessionState::Degraded { .. }),
            "a blip must not tear down a working stream"
        );
        assert!(degraded.effects.is_empty(), "degrading must not stop anything");
        assert!(degraded.state.is_live(), "audio is still flowing while degraded");

        // And it recovers on its own.
        let recovered = step(degraded.state, Event::HealthRecovered);
        assert!(matches!(recovered.state, SessionState::Streaming { .. }));
        assert!(recovered.effects.is_empty());
    }

    #[test]
    fn a_lost_peer_degrades_first_and_only_then_fails() {
        // A Wi-Fi blip drops the connection and the speaker re-fetches. Failing immediately
        // would make every household with a microwave oven unusable.
        let streaming = SessionState::Streaming {
            target: Box::new(device("Kitchen")),
            url: url(),
            health: Health::Good,
            format: AudioFormat::WIRE_DEFAULT,
        };
        let degraded = step(streaming, Event::PeerLost);
        assert!(matches!(
            degraded.state,
            SessionState::Degraded { reason: DegradeReason::PeerReconnecting, .. }
        ));

        // A second loss while already degraded is a real disconnection.
        let failed = step(degraded.state, Event::PeerLost);
        assert!(matches!(
            failed.state,
            SessionState::Failed { reason: FailureReason::DeviceUnreachable, .. }
        ));
        assert_eq!(failed.effects.len(), 3, "teardown must still run");
    }

    #[test]
    fn a_full_cycle_returns_to_idle() {
        let mut state = drive_to_streaming().pop().expect("history").state;
        for event in [Event::Disconnect, Event::TeardownComplete] {
            state = step(state, event).state;
        }
        assert_eq!(state, SessionState::Idle);
    }

    #[test]
    fn an_empty_scan_is_a_distinct_state_from_a_failed_one() {
        // The interface shows entirely different things for these, so the machine must too.
        let empty = step(SessionState::Scanning, Event::ScanCompleted(Vec::new()));
        assert_eq!(empty.state, SessionState::NoDevices);

        let broken = step(SessionState::Scanning, Event::ScanFailed("no interface".into()));
        assert!(matches!(
            broken.state,
            SessionState::Failed { reason: FailureReason::ScanFailed { .. }, .. }
        ));
    }

    #[test]
    fn retry_from_a_failure_restarts_the_whole_sequence() {
        let failed = SessionState::Failed {
            reason: FailureReason::FirewallSuspected,
            retryable: true,
            target: Some(Box::new(device("Kitchen"))),
        };
        let retried = step(failed, Event::Retry);
        assert!(matches!(
            retried.state,
            SessionState::Preparing { step: PrepareStep::ResolveCoordinator, .. }
        ));
        assert!(matches!(retried.effects.first(), Some(Effect::ResolveCoordinator(_))));
    }

    #[test]
    fn a_failure_with_no_target_cannot_be_retried_into_nothing() {
        let failed = SessionState::Failed {
            reason: FailureReason::ScanFailed { detail: "x".into() },
            retryable: true,
            target: None,
        };
        let transition = step(failed, Event::Retry);
        assert!(!transition.changed, "a retry with nothing to retry must be inert");
    }

    #[test]
    fn every_failure_reason_carries_a_message_and_a_remediation() {
        let reasons = [
            FailureReason::NoCaptureDevice,
            FailureReason::CaptureBusy,
            FailureReason::DeviceUnreachable,
            FailureReason::RejectedByDevice { upnp_code: 714, description: "x".into() },
            FailureReason::FirewallSuspected,
            FailureReason::NetworkIsolated,
            FailureReason::ScanFailed { detail: "x".into() },
            FailureReason::Internal { detail: "x".into() },
        ];
        for reason in reasons {
            assert!(reason.message().ends_with('.'), "{reason:?} message is not a sentence");
            assert!(
                reason.remediation().ends_with('.'),
                "{reason:?} remediation is not a sentence"
            );
            assert!(!reason.message().contains("Error"), "{reason:?} leaks developer vocabulary");
        }
        // 701 means "busy, try later"; 714 means "this will never work".
        assert!(
            FailureReason::RejectedByDevice { upnp_code: 701, description: String::new() }
                .retryable()
        );
        assert!(
            !FailureReason::RejectedByDevice { upnp_code: 714, description: String::new() }
                .retryable()
        );
    }

    #[test]
    fn prepare_steps_are_ordered_and_labelled_for_the_interface() {
        for (index, step) in PrepareStep::ORDER.iter().enumerate() {
            assert_eq!(step.index(), index, "{step:?} is out of order");
            assert!(!step.label().is_empty());
        }
    }
}
