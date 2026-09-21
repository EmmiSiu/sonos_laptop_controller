//! Session orchestration: the part that makes four subsystems behave like one product.
//!
//! The design is a **pure reducer plus a dull driver**:
//!
//! - [`machine::step`] is a pure function of `(state, event)`. Every transition the session can
//!   make is written down in one `match`, and all of them are unit-testable in microseconds.
//! - [`driver::Engine`] executes the effects the reducer returns. It makes no decisions.
//!
//! That split is what allows the whole of the session logic — including every failure path, the
//! teardown guarantees, and the firewall classification — to be tested in CI with no speaker,
//! no sound card, and no network.
//!
//! See [SPEC-005](../../../specs/SPEC-005-engine.md).

pub mod backoff;
pub mod driver;
pub mod machine;

pub use backoff::Backoff;
pub use driver::{Dependencies, Engine, EngineConfig};
pub use machine::{
    DegradeReason, Effect, Event, FailureReason, PrepareStep, SessionState, Transition, step,
};

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

    use std::sync::Arc;
    use std::time::Duration;

    use rincon_audio::SyntheticCapture;
    use rincon_core::audio::{AudioFormat, SampleEncoding};
    use rincon_testkit::{ControlCall, FakeControl, FakeDiscovery, device};

    use super::*;

    fn engine(discovery: FakeDiscovery, control: FakeControl) -> Arc<Engine> {
        engine_with(
            discovery,
            control,
            EngineConfig { peer_timeout: Duration::from_millis(1_500), max_connections: 1 },
        )
    }

    fn engine_with(
        discovery: FakeDiscovery,
        control: FakeControl,
        config: EngineConfig,
    ) -> Arc<Engine> {
        let format = AudioFormat::new(44_100, 2, SampleEncoding::F32Le).unwrap();
        Engine::new(
            Dependencies {
                discovery: Arc::new(discovery),
                control: Arc::new(control),
                capture: Arc::new(SyntheticCapture::new(format, 7)),
            },
            config,
        )
    }

    /// Acts as the speaker: watches for the stream URL and fetches it, exactly as a real player
    /// would once it has been handed one.
    fn play_the_speaker(engine: &Arc<Engine>) -> tokio::task::JoinHandle<bool> {
        let mut states = engine.subscribe();
        tokio::spawn(async move {
            while let Ok(state) = states.recv().await {
                if let SessionState::Preparing { url: Some(url), .. } = state {
                    let client = reqwest::Client::builder()
                        .timeout(Duration::from_secs(5))
                        .build()
                        .expect("client must build");
                    // A HEAD is enough to register as a connection, and it does not leave an
                    // endless body open for the rest of the test.
                    return client.head(url.as_str()).send().await.is_ok();
                }
            }
            false
        })
    }

    /// A device whose `reached_via` is loopback, so the server binds somewhere the test can
    /// actually reach.
    fn loopback_device(room: &str) -> rincon_core::device::Device {
        let mut device = device(room, 45);
        device.address = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
        device.reached_via = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        device
    }

    /// Covers: RQ-ENG-011
    #[tokio::test]
    async fn rq_eng_011_runs_entirely_on_fakes() {
        // The claim: the whole session -- discovery, capture, the real HTTP server, control,
        // and orchestration -- runs with no speaker, no sound card, and no LAN.
        let kitchen = loopback_device("Kitchen");
        let control = FakeControl::accepting();
        let engine = engine(FakeDiscovery::finding(vec![kitchen.clone()]), control.clone());

        engine.dispatch(Event::StartScan).await;
        match engine.state() {
            SessionState::DevicesFound { devices } => assert_eq!(devices.len(), 1),
            other => panic!("expected devices, got {other:?}"),
        }

        let speaker = play_the_speaker(&engine);
        engine.dispatch(Event::Connect(Box::new(kitchen))).await;
        assert!(speaker.await.unwrap(), "the simulated speaker could not fetch the stream");

        assert!(
            matches!(engine.state(), SessionState::Streaming { .. }),
            "expected Streaming, got {:?}",
            engine.state()
        );

        // And the commands went out in the order a real device needs them.
        let calls = control.calls();
        assert_eq!(
            calls
                .iter()
                .map(|call| match call {
                    ControlCall::ResolveCoordinator { .. } => "resolve",
                    ControlCall::SetUri { .. } => "set_uri",
                    ControlCall::Play { .. } => "play",
                    ControlCall::Stop { .. } => "stop",
                    ControlCall::SetVolume { .. } => "volume",
                })
                .take(3)
                .collect::<Vec<_>>(),
            vec!["resolve", "set_uri", "play"]
        );

        engine.dispatch(Event::Disconnect).await;
        engine.dispatch(Event::TeardownComplete).await;
    }

    /// Covers: RQ-ENG-010, RQ-SEC-007
    #[tokio::test]
    async fn rq_eng_010_cycle_leaks_nothing() {
        let kitchen = loopback_device("Kitchen");
        let engine =
            engine(FakeDiscovery::finding(vec![kitchen.clone()]), FakeControl::accepting());

        let speaker = play_the_speaker(&engine);
        engine.dispatch(Event::Connect(Box::new(kitchen))).await;
        let _ = speaker.await;

        assert!(matches!(engine.state(), SessionState::Streaming { .. }));
        assert!(engine.holds_resources(), "a live session must hold its resources");

        let port = engine
            .stream_url()
            .and_then(|url| {
                url.as_str()
                    .split('/')
                    .nth(2)
                    .and_then(|authority| authority.rsplit(':').next()?.parse::<u16>().ok())
            })
            .expect("a live session must have a bound port");

        engine.dispatch(Event::Disconnect).await;
        engine.dispatch(Event::TeardownComplete).await;

        assert_eq!(engine.state(), SessionState::Idle);
        assert!(
            !engine.holds_resources(),
            "the session left resources behind: {:?}",
            engine.state()
        );
        assert!(engine.counters().is_none(), "session counters outlived the session");

        // Rebinding the port proves the listener is gone, not merely idle.
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let rebound = tokio::net::TcpListener::bind(addr).await;
        assert!(rebound.is_ok(), "the stream port was still held after teardown: {rebound:?}");
    }

    /// Covers: RQ-ENG-005
    #[tokio::test]
    async fn rq_eng_005_firewall_timeout_is_classified() {
        // Nobody fetches the stream -- exactly what a firewall block looks like from here.
        let kitchen = loopback_device("Kitchen");
        let engine = engine_with(
            FakeDiscovery::finding(vec![kitchen.clone()]),
            FakeControl::accepting(),
            EngineConfig { peer_timeout: Duration::from_millis(300), max_connections: 1 },
        );

        engine.dispatch(Event::Connect(Box::new(kitchen))).await;

        match engine.state() {
            SessionState::Failed { reason, retryable, target } => {
                assert_eq!(reason, FailureReason::FirewallSuspected);
                assert!(retryable);
                assert!(target.is_some(), "a retry must know which speaker");
                assert!(reason.remediation().contains("Firewall"));
            }
            other => panic!("expected a classified firewall failure, got {other:?}"),
        }

        // And it cleaned up on the failure path, which is the path that usually leaks.
        assert!(!engine.holds_resources(), "a failed session left resources behind");
    }

    /// Covers: RQ-ENG-009
    #[tokio::test]
    async fn rq_eng_009_transitions_are_observable() {
        let engine = engine(FakeDiscovery::empty(), FakeControl::accepting());
        let mut states = engine.subscribe();

        engine.dispatch(Event::StartScan).await;

        // Exactly two transitions: Idle -> Scanning -> NoDevices. Not one, not three.
        let first = states.try_recv().expect("Scanning was not published");
        let second = states.try_recv().expect("NoDevices was not published");
        assert_eq!(first, SessionState::Scanning);
        assert_eq!(second, SessionState::NoDevices);
        assert!(states.try_recv().is_err(), "a third state was published for two transitions");

        // An inapplicable event publishes nothing at all.
        engine.dispatch(Event::PlaybackStarted).await;
        assert!(states.try_recv().is_err(), "an inert event was published as a transition");
    }

    /// Covers: RQ-SEC-007
    #[tokio::test]
    async fn rq_sec_007_capture_stops_with_session() {
        // Loopback capture reads everything the user hears, so it must not outlive the session
        // by even a moment. The guard is the mechanism; this is the proof.
        let kitchen = loopback_device("Kitchen");
        let engine =
            engine(FakeDiscovery::finding(vec![kitchen.clone()]), FakeControl::accepting());

        assert!(!engine.holds_resources(), "nothing is captured before a session starts");

        let speaker = play_the_speaker(&engine);
        engine.dispatch(Event::Connect(Box::new(kitchen))).await;
        let _ = speaker.await;
        assert!(engine.holds_resources());

        engine.dispatch(Event::Disconnect).await;
        assert!(!engine.holds_resources(), "capture outlived the session");
    }

    #[tokio::test]
    async fn a_failing_scan_is_reported_rather_than_looking_empty() {
        let engine = engine(FakeDiscovery::failing("no interface"), FakeControl::accepting());
        engine.dispatch(Event::StartScan).await;

        match engine.state() {
            SessionState::Failed { reason: FailureReason::ScanFailed { detail }, .. } => {
                assert!(detail.contains("interface"));
            }
            other => panic!("a broken scan must not look like an empty one: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_refused_command_becomes_an_actionable_failure_and_cleans_up() {
        let kitchen = loopback_device("Kitchen");
        let control =
            FakeControl::accepting().refusing(rincon_control::ControlError::TransitionNotAvailable);
        let engine = engine(FakeDiscovery::finding(vec![kitchen.clone()]), control);

        engine.dispatch(Event::Connect(Box::new(kitchen))).await;

        match engine.state() {
            SessionState::Failed {
                reason: FailureReason::RejectedByDevice { upnp_code, .. },
                ..
            } => {
                assert_eq!(upnp_code, 701);
            }
            other => panic!("expected a device rejection, got {other:?}"),
        }
        assert!(!engine.holds_resources(), "a refused command left resources behind");
    }

    #[tokio::test]
    async fn a_device_with_no_reachable_interface_fails_before_binding() {
        // `reached_via: None` means discovery never established a route back. Binding anyway
        // would produce a URL the speaker cannot reach, and a firewall message that is wrong.
        let mut orphan = device("Attic", 99);
        orphan.reached_via = None;
        let engine = engine(FakeDiscovery::finding(vec![orphan.clone()]), FakeControl::accepting());

        engine.dispatch(Event::Connect(Box::new(orphan))).await;

        match engine.state() {
            SessionState::Failed { reason, .. } => {
                assert_eq!(reason, FailureReason::NetworkIsolated);
                assert!(!reason.retryable(), "rescanning is the fix, not retrying");
            }
            other => panic!("expected NetworkIsolated, got {other:?}"),
        }
    }
}
