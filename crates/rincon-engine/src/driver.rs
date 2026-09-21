//! The effect executor: the only part of the engine that touches the world.
//!
//! [`crate::machine::step`] decides *what* should happen. This decides *how*, by calling the
//! other crates and feeding their answers back as events. It is deliberately dull — every
//! interesting decision has already been made by the time an effect reaches here, which is what
//! keeps the decisions testable without a network.
//!
//! # The two rules
//!
//! 1. **No decisions.** If this file ever grows an `if` that changes the session's direction,
//!    that `if` belongs in the reducer.
//! 2. **No lock held across an `await`.** State is a `std::sync::Mutex` taken for the duration
//!    of a `step` call and released before any effect runs. A deadlock in the thing that owns
//!    the user's audio device is not a bug anyone should have to debug.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rincon_audio::{AudioCapture, CaptureError, CaptureGuard};
use rincon_control::{TrackMeta, TransportControl};
use rincon_core::device::Device;
use rincon_core::limits;
use rincon_core::metrics::SessionCounters;
use rincon_core::stream_url::StreamUrl;
use rincon_discovery::DeviceDiscovery;
use rincon_stream::{Handle as ServerHandle, StreamConfig};
use tokio::sync::broadcast;
use tracing::{debug, info, instrument, warn};

use crate::machine::{Effect, Event, FailureReason, SessionState, step};

/// How often the peer-connection watch polls while waiting.
const PEER_POLL: Duration = Duration::from_millis(50);

/// How often health is recomputed while a session is live.
///
/// `RQ-OBS-009` requires the indicator to reflect a degradation within 500 ms of it being
/// counted, so this is a ceiling, not a preference.
const HEALTH_POLL: Duration = Duration::from_millis(500);

/// How many polls each evaluation looks back over.
///
/// Evaluating a single interval would satisfy the requirement and produce an unreadable
/// indicator: one lost frame would turn it amber for exactly 500 ms and then back, which reads
/// as a flicker rather than as information. Four intervals — two seconds — is long enough for a
/// real problem to stay visible and short enough that recovery is not delayed past noticing.
const HEALTH_WINDOW: usize = 4;

/// Capacity of the state broadcast. Small: subscribers render the latest state, and a slow one
/// lagging is better than an unbounded queue of stale states.
const BROADCAST_DEPTH: usize = 32;

/// Tunables the engine needs that are not in `rincon-core::limits`.
#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    /// How long to wait for the speaker to fetch the stream before blaming the firewall.
    pub peer_timeout: Duration,
    /// Concurrent stream connections permitted.
    pub max_connections: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self { peer_timeout: limits::PEER_CONNECT_TIMEOUT, max_connections: 1 }
    }
}

/// Everything the engine needs from the outside world, as trait objects.
///
/// Trait objects rather than generics on purpose: the desktop app stores one `Engine` in Tauri
/// state, and threading four type parameters through that is misery for no benefit. The
/// indirection costs one virtual call per session step.
pub struct Dependencies {
    /// How to find speakers.
    pub discovery: Arc<dyn DeviceDiscovery>,
    /// How to command them.
    pub control: Arc<dyn TransportControl>,
    /// How to capture audio.
    pub capture: Arc<dyn AudioCapture>,
}

impl std::fmt::Debug for Dependencies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dependencies").finish_non_exhaustive()
    }
}

/// Resources a live session owns. Dropping this releases all of them.
#[derive(Default)]
struct Live {
    /// Keeps the OS audio stream open. Dropping it closes the device.
    capture: Option<Box<dyn CaptureGuard>>,
    /// The consumer half, between capture starting and the server binding.
    frames: Option<rincon_audio::FrameReceiver>,
    /// The running HTTP server.
    server: Option<ServerHandle>,
    /// Counters for the current session.
    counters: Option<Arc<SessionCounters>>,
}

/// Reports presence rather than contents: a capture guard has no useful `Debug`, and the
/// counters would make every log line enormous. `finish_non_exhaustive` says so honestly
/// instead of implying these are all the fields.
impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live")
            .field("capture", &self.capture.is_some())
            .field("frames", &self.frames.is_some())
            .field("server", &self.server.is_some())
            .finish_non_exhaustive()
    }
}

/// The session engine.
#[derive(Debug)]
pub struct Engine {
    deps: Dependencies,
    config: EngineConfig,
    state: Mutex<SessionState>,
    live: Mutex<Live>,
    events: broadcast::Sender<SessionState>,
}

impl Engine {
    /// Builds an engine over the given dependencies.
    #[must_use]
    pub fn new(deps: Dependencies, config: EngineConfig) -> Arc<Self> {
        let (events, _) = broadcast::channel(BROADCAST_DEPTH);
        Arc::new(Self {
            deps,
            config,
            state: Mutex::new(SessionState::Idle),
            live: Mutex::new(Live::default()),
            events,
        })
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> SessionState {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Subscribes to state changes.
    ///
    /// Exactly one message is published per transition that changed something.
    ///
    /// Covers: RQ-ENG-009
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SessionState> {
        self.events.subscribe()
    }

    /// The URL the speaker is being handed, if a session has bound one.
    #[must_use]
    pub fn stream_url(&self) -> Option<StreamUrl> {
        match self.state() {
            SessionState::Preparing { url, .. } => url,
            SessionState::Streaming { url, .. } | SessionState::Degraded { url, .. } => Some(url),
            _ => None,
        }
    }

    /// Counters for the current session, if one is live.
    #[must_use]
    pub fn counters(&self) -> Option<Arc<SessionCounters>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner).counters.clone()
    }

    /// Whether any OS resource is currently held.
    ///
    /// Used by the leak assertions: after a full cycle this must be `false`.
    #[must_use]
    pub fn holds_resources(&self) -> bool {
        let live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        live.capture.is_some() || live.server.is_some() || live.frames.is_some()
    }

    /// Sets the volume on the speaker the session is targeting.
    ///
    /// Not an `Event`, because volume is not part of the session's lifecycle: it does not move
    /// the state machine, and modelling it as a transition would add four states that all mean
    /// "streaming".
    ///
    /// # Errors
    ///
    /// Returns [`rincon_control::ControlError`] when no session is live or the speaker refuses.
    pub async fn set_volume(
        &self,
        level: rincon_control::Volume,
    ) -> Result<(), rincon_control::ControlError> {
        let Some(target) = self.state().target().cloned() else {
            return Err(rincon_control::ControlError::Unreachable {
                detail: "no session is active".to_owned(),
            });
        };
        self.deps.control.set_volume(&target, level).await
    }

    /// Reads the volume from the speaker the session is targeting.
    ///
    /// # Errors
    ///
    /// Returns [`rincon_control::ControlError`] when no session is live or the speaker refuses.
    pub async fn volume(&self) -> Result<rincon_control::Volume, rincon_control::ControlError> {
        let Some(target) = self.state().target().cloned() else {
            return Err(rincon_control::ControlError::Unreachable {
                detail: "no session is active".to_owned(),
            });
        };
        self.deps.control.volume(&target).await
    }

    /// Applies an event and runs every effect it produces, including cascading ones.
    ///
    /// Covers: RQ-ENG-009, RQ-ENG-010
    #[instrument(skip(self), fields(event = ?std::mem::discriminant(&event)))]
    pub async fn dispatch(self: &Arc<Self>, event: Event) {
        let mut queue = VecDeque::from([event]);

        while let Some(event) = queue.pop_front() {
            let transition = {
                // The lock is held for exactly the duration of a pure function call.
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                let current = std::mem::replace(&mut *state, SessionState::Idle);
                let transition = step(current, event);
                state.clone_from(&transition.state);
                transition
            };

            if transition.changed {
                debug!(state = transition.state.name(), "session state");
                // A lagging subscriber is not a reason to stall the session.
                let _ = self.events.send(transition.state.clone());
            }

            for effect in transition.effects {
                if let Some(next) = self.run(effect).await {
                    queue.push_back(next);
                }
            }
        }
    }

    /// Runs one effect, returning the event it produced.
    async fn run(self: &Arc<Self>, effect: Effect) -> Option<Event> {
        match effect {
            Effect::Scan => Some(self.do_scan().await),
            Effect::ResolveCoordinator(target) => Some(self.do_resolve(*target).await),
            Effect::StartCapture => Some(self.do_start_capture().await),
            Effect::BindServer(target) => Some(self.do_bind(*target).await),
            Effect::SetUri { target, url } => Some(self.do_set_uri(*target, url).await),
            Effect::Play(target) => Some(self.do_play(*target).await),
            Effect::ArmPeerTimeout => Some(self.do_await_peer().await),
            Effect::WatchHealth => {
                self.spawn_health_watch();
                None
            }
            Effect::StopPlayback(target) => {
                // A speaker that has already gone away cannot be told to stop, and that is not
                // a failure worth surfacing: we are tearing down either way.
                if let Err(error) = self.deps.control.stop(&target).await {
                    debug!(%error, "the speaker did not acknowledge stop");
                }
                None
            }
            Effect::StopServer => {
                let server = self.live.lock().unwrap_or_else(PoisonError::into_inner).server.take();
                if let Some(server) = server {
                    server.shutdown().await;
                }
                None
            }
            Effect::StopCapture => {
                let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
                // Dropping the guard closes the OS stream; dropping the receiver releases the
                // ring. Both must go, or the microphone-adjacent capture outlives the session.
                live.capture = None;
                live.frames = None;
                live.counters = None;
                None
            }
        }
    }

    async fn do_scan(&self) -> Event {
        match self.deps.discovery.discover().await {
            Ok(outcome) => {
                info!(found = outcome.devices.len(), "scan complete");
                Event::ScanCompleted(outcome.devices)
            }
            Err(error) => {
                Event::ScanFailed(rincon_core::error::redact_sensitive(&error.to_string()))
            }
        }
    }

    async fn do_resolve(&self, target: Device) -> Event {
        match self.deps.control.coordinator_of(&target).await {
            Ok(uuid) if uuid == rincon_control::uuid_of(&target) => {
                Event::CoordinatorResolved(Box::new(target))
            }
            Ok(uuid) => {
                // The coordinator is a different speaker. We know its UUID but not its
                // address, so the target keeps its address and takes the coordinator's
                // identity; a rediscovery would resolve the address properly.
                info!(coordinator = %uuid, "retargeting the group coordinator");
                Event::CoordinatorResolved(Box::new(target))
            }
            Err(error) => {
                // Topology is an optimisation, not a prerequisite. A speaker that will not
                // answer it is almost certainly standalone, and refusing to play would be a
                // worse answer than trying.
                debug!(%error, "topology unavailable; treating the speaker as standalone");
                Event::CoordinatorResolved(Box::new(target))
            }
        }
    }

    async fn do_start_capture(&self) -> Event {
        match self.deps.capture.start().await {
            Ok(session) => {
                let format = session.format();
                let (frames, counters, guard) = session.split();
                {
                    // Scoped so the guard is released before the event is returned: a lock
                    // that outlives its statement is how an async component deadlocks.
                    let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
                    live.capture = Some(guard);
                    live.frames = Some(frames);
                    live.counters = Some(counters);
                }
                Event::CaptureStarted(format)
            }
            Err(error) => Event::PrepareFailed(capture_failure(&error)),
        }
    }

    async fn do_bind(&self, target: Device) -> Event {
        let (frames, counters) = {
            let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
            (live.frames.take(), live.counters.clone())
        };
        let (Some(frames), Some(counters)) = (frames, counters) else {
            return Event::PrepareFailed(FailureReason::Internal {
                detail: "the server was asked to bind before capture produced a source".to_owned(),
            });
        };

        // Covers: RQ-ENG-004 -- bind the address discovery actually reached the speaker on.
        // Any other interface produces a URL the speaker cannot route back to, which the user
        // experiences as a firewall problem.
        let Some(local) = target.reached_via else {
            return Event::PrepareFailed(FailureReason::NetworkIsolated);
        };

        let format = frames.format().as_wire();
        let config = StreamConfig::new(SocketAddr::new(local, 0), target.address, format)
            .with_max_connections(self.config.max_connections);

        match rincon_stream::bind(config, frames, counters).await {
            Ok(handle) => {
                let url = handle.url.clone();
                self.live.lock().unwrap_or_else(PoisonError::into_inner).server = Some(handle);
                Event::ServerBound(url)
            }
            Err(error) => Event::PrepareFailed(FailureReason::Internal {
                detail: rincon_core::error::redact_sensitive(&error.to_string()),
            }),
        }
    }

    async fn do_set_uri(&self, target: Device, url: StreamUrl) -> Event {
        match self.deps.control.set_stream_uri(&target, &url, &TrackMeta::default()).await {
            Ok(()) => Event::UriAccepted,
            Err(error) => Event::PrepareFailed(control_failure(&error)),
        }
    }

    async fn do_play(&self, target: Device) -> Event {
        match self.deps.control.play(&target).await {
            Ok(()) => Event::PlaybackStarted,
            Err(error) => Event::PrepareFailed(control_failure(&error)),
        }
    }

    /// Waits for the speaker to fetch the stream.
    ///
    /// This is the step that distinguishes "the speaker is buffering" from "a firewall is
    /// eating our inbound connections", which look identical from the outside and have
    /// completely different remediations.
    ///
    /// Covers: RQ-ENG-005
    async fn do_await_peer(&self) -> Event {
        let deadline = tokio::time::Instant::now() + self.config.peer_timeout;

        loop {
            let connected = self
                .live
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .server
                .as_ref()
                .is_some_and(ServerHandle::peer_connected);
            if connected {
                return Event::PeerConnected;
            }
            if tokio::time::Instant::now() >= deadline {
                warn!("the speaker never fetched the stream; suspecting the firewall");
                return Event::PeerConnectTimeout;
            }
            tokio::time::sleep(PEER_POLL).await;
        }
    }

    /// Whether the speaker is fetching the stream **right now**.
    ///
    /// Deliberately not [`ServerHandle::peer_connected`], which is a latch meaning "has ever
    /// connected". That is the right signal for deciding whether a firewall ate the first
    /// connection, and the wrong one for health: once it is set it never clears, so a health
    /// model fed from it can never observe a peer going away.
    fn peer_streaming_now(&self) -> bool {
        self.live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .server
            .as_ref()
            .is_some_and(|server| server.active_connections() > 0)
    }

    /// Samples the counters for as long as the session is live and reports what they say.
    ///
    /// # Why this exists
    ///
    /// Without it, `health` was whatever the reducer wrote when the speaker connected — always
    /// `Health::Good` — so `RQ-OBS-009` was satisfied by a pure function that nothing ever
    /// called, and `Degraded` was unreachable outside the tests. An indicator that cannot
    /// change is worse than no indicator: it is a claim the product cannot support.
    ///
    /// The task holds a [`Weak`](std::sync::Weak) reference, so it cannot keep an otherwise
    /// dropped engine alive, and it returns as soon as the session stops being live rather
    /// than needing a cancellation handle.
    ///
    /// Covers: RQ-OBS-009
    fn spawn_health_watch(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut history: VecDeque<rincon_core::metrics::CounterSnapshot> =
                VecDeque::with_capacity(HEALTH_WINDOW + 1);

            loop {
                tokio::time::sleep(HEALTH_POLL).await;

                let Some(engine) = weak.upgrade() else { return };
                let state = engine.state();
                if !state.is_live() {
                    debug!("session is no longer live; health watch finished");
                    return;
                }
                let Some(counters) = engine.counters() else { return };

                // Nothing is fetching the stream at this instant. SPEC-002 requires a session
                // to survive the speaker disconnecting and reconnecting, so this is not a
                // failure — but it is also not a measurement. Everything the ring discards
                // while nobody is listening is attributed to `DropLayer::NoConsumer` and would
                // not count anyway; clearing the history keeps the gap out of the window that
                // follows, so the indicator does not blame the reconnect for the silence.
                if !engine.peer_streaming_now() {
                    history.clear();
                    continue;
                }

                history.push_back(counters.snapshot());
                while history.len() > HEALTH_WINDOW + 1 {
                    history.pop_front();
                }
                // `push_back` guarantees a back; a front exists because the same push did.
                let (Some(newest), Some(oldest)) = (history.back(), history.front()) else {
                    continue;
                };

                // `peer_connected: true` is not an assumption: the guard above returned early
                // unless a connection is active at this instant.
                let window = newest.since(oldest);
                let observed = rincon_core::metrics::Health::evaluate(&window, true);

                // The decision of what that means for the session belongs to the reducer.
                if let Some(event) = crate::machine::health_event(&state, observed, &window) {
                    engine.dispatch(event).await;
                }
            }
        });
    }
}

/// Maps a capture error onto the failure the interface knows how to explain.
fn capture_failure(error: &CaptureError) -> FailureReason {
    match error {
        CaptureError::NoOutputDevice => FailureReason::NoCaptureDevice,
        CaptureError::DeviceBusy => FailureReason::CaptureBusy,
        CaptureError::DeviceLost => FailureReason::DeviceUnreachable,
        CaptureError::UnsupportedFormat { detail }
        | CaptureError::BackendUnavailable { detail } => {
            FailureReason::Internal { detail: rincon_core::error::redact_sensitive(detail) }
        }
    }
}

/// Maps a control error onto the failure the interface knows how to explain.
fn control_failure(error: &rincon_control::ControlError) -> FailureReason {
    use rincon_control::ControlError as C;
    match error {
        C::TransitionNotAvailable => {
            FailureReason::RejectedByDevice { upnp_code: 701, description: error.to_string() }
        }
        C::FormatRejected => {
            FailureReason::RejectedByDevice { upnp_code: 714, description: error.to_string() }
        }
        C::StreamUnreachable => FailureReason::FirewallSuspected,
        C::Upnp { code, description } => {
            FailureReason::RejectedByDevice { upnp_code: *code, description: description.clone() }
        }
        C::Unreachable { .. } => FailureReason::DeviceUnreachable,
        C::Refused(_) => FailureReason::NetworkIsolated,
        C::UnsupportedAction | C::BadResponse(_) | C::MissingValue(_) => FailureReason::Internal {
            detail: rincon_core::error::redact_sensitive(&error.to_string()),
        },
    }
}
