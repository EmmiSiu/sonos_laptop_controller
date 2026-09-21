//! The HTTP server that a Sonos player pulls the laptop's audio from.
//!
//! See [`server`] for the request pipeline and [SPEC-002](../../../specs/SPEC-002-stream-server.md)
//! for the contract. The short version:
//!
//! - one route, guarded by a peer allowlist and a 128-bit token compared in constant time;
//! - a WAV header followed by PCM, served until the user stops;
//! - never binds a wildcard address unless someone asked for that in writing.

pub mod config;
pub mod server;
pub mod token;
pub mod wav;

pub use config::{ConfigError, Framing, StreamConfig};
pub use server::{Handle, StreamError, bind};
pub use token::SessionToken;

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

    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::time::Duration;

    use rincon_audio::synthetic;
    use rincon_core::audio::{AudioFormat, SampleEncoding};
    use rincon_core::metrics::SessionCounters;

    use super::*;

    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    fn format() -> AudioFormat {
        AudioFormat::new(44_100, 2, SampleEncoding::S16Le).unwrap()
    }

    fn bind_addr() -> SocketAddr {
        SocketAddr::new(LOOPBACK, 0)
    }

    /// A server fed from a fixed script of samples, so assertions can be byte-exact.
    async fn serve_script(config: StreamConfig, script: &[Vec<f32>]) -> Handle {
        let (receiver, counters) = synthetic::scripted(config.format, script);
        bind(config, receiver, counters).await.expect("server must bind on loopback")
    }

    /// A server fed from an endless generator, for tests about behaviour over time.
    async fn serve_live(config: StreamConfig) -> Handle {
        let counters = SessionCounters::new();
        let (mut tx, rx) = rincon_audio::ring::channel(config.format, 500, Arc::clone(&counters));
        tokio::task::spawn_blocking(move || {
            let source = synthetic::SyntheticSource::new(format(), 1);
            let mut buffer = vec![0.0_f32; 441 * 2];
            let mut frame = 0_u64;
            for _ in 0..400 {
                source.fill(frame, &mut buffer);
                tx.push(&buffer);
                frame += 441;
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        bind(config, rx, counters).await.expect("server must bind on loopback")
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client must build")
    }

    /// Covers: RQ-STRM-004, RQ-SEC-006
    #[tokio::test]
    async fn rq_strm_004_rejects_unallowlisted_peer() {
        // The allowlist names only the speaker, so our own loopback request is a stranger.
        let mut config = StreamConfig::new(bind_addr(), "192.168.1.45".parse().unwrap(), format());
        config.allowed_peers = vec!["192.168.1.45".parse().unwrap()];

        let handle = serve_script(config, &[vec![0.5; 128]]).await;
        let response = client().get(handle.url.as_str()).send().await.unwrap();

        assert_eq!(response.status(), 403, "a stranger must be refused");
        // The refusal must happen before any audio is touched.
        assert_eq!(handle.counters().frames_served(), 0, "audio reached an unallowlisted peer");
        handle.shutdown().await;
    }

    /// Covers: RQ-STRM-006
    #[tokio::test]
    async fn rq_strm_006_wrong_token_is_indistinguishable_from_404() {
        let config = StreamConfig::new(bind_addr(), LOOPBACK, format());
        let handle = serve_script(config, &[vec![0.25; 128]]).await;
        let base = format!("http://{}", handle.local_addr);

        let wrong_token = client()
            .get(format!("{base}/s/00000000000000000000000000000000/stream.wav"))
            .send()
            .await
            .unwrap();
        let no_such_route = client().get(format!("{base}/nothing-here")).send().await.unwrap();

        assert_eq!(wrong_token.status(), 404);
        assert_eq!(no_such_route.status(), 404);
        // A prober must not be able to tell the two apart from the body either.
        assert_eq!(
            wrong_token.text().await.unwrap(),
            no_such_route.text().await.unwrap(),
            "the two 404s differ, which tells a prober the route exists"
        );
        handle.shutdown().await;
    }

    /// Covers: RQ-STRM-012
    #[tokio::test]
    async fn rq_strm_012_no_other_routes_exist() {
        let config = StreamConfig::new(bind_addr(), LOOPBACK, format());
        let handle = serve_script(config, &[vec![0.25; 128]]).await;
        let base = format!("http://{}", handle.local_addr);

        for path in [
            "/",
            "/index.html",
            "/s/",
            "/s/../../etc/passwd",
            "/admin",
            "/.env",
            "/stream.wav",
            "/s/token/stream.wav/../../../secret",
        ] {
            let response = client().get(format!("{base}{path}")).send().await.unwrap();
            assert_eq!(response.status(), 404, "`{path}` must not exist");
        }
        handle.shutdown().await;
    }

    /// Covers: RQ-STRM-013
    #[tokio::test]
    async fn rq_strm_013_security_headers_present() {
        let config = StreamConfig::new(bind_addr(), LOOPBACK, format());
        let handle = serve_script(config, &[vec![0.25; 256]]).await;

        let response = client().head(handle.url.as_str()).send().await.unwrap();
        let headers = response.headers();

        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(headers.get("content-type").unwrap(), "audio/x-wav");
        assert_eq!(headers.get("accept-ranges").unwrap(), "none");
        assert_eq!(headers.get("cache-control").unwrap(), "no-store");
        assert!(headers.get("server").is_none(), "a version banner helps nobody but an attacker");
        handle.shutdown().await;
    }

    /// Covers: RQ-STRM-010
    #[tokio::test]
    async fn rq_strm_010_head_returns_headers_without_body() {
        let config = StreamConfig::new(bind_addr(), LOOPBACK, format());
        let handle = serve_script(config, &[vec![0.25; 256]]).await;

        let head = client().head(handle.url.as_str()).send().await.unwrap();
        assert_eq!(head.status(), 200);
        assert_eq!(head.headers().get("content-type").unwrap(), "audio/x-wav");
        assert!(head.bytes().await.unwrap().is_empty(), "HEAD must carry no body");

        // A HEAD must not consume the audio source, or the following GET would find it leased.
        let get = client().get(handle.url.as_str()).send().await.unwrap();
        assert_eq!(get.status(), 200, "HEAD stole the audio source from the subsequent GET");
        handle.shutdown().await;
    }

    /// Covers: RQ-STRM-014
    #[tokio::test]
    async fn rq_strm_014_byte_stream_is_faithful() {
        // A script whose samples are recognisable, so reordering or duplication is obvious.
        let script = vec![vec![1.0, -1.0, 0.5, -0.5, 0.0, 0.25]];
        let config = StreamConfig::new(bind_addr(), LOOPBACK, format());
        let handle = serve_script(config, &script).await;

        let started = std::time::Instant::now();
        let body = client().get(handle.url.as_str()).send().await.unwrap().bytes().await.unwrap();
        let first_byte = started.elapsed();

        assert!(body.len() >= wav::HEADER_LEN, "the response is shorter than a WAV header");
        assert_eq!(&body[..4], b"RIFF");
        assert_eq!(&body[..wav::HEADER_LEN], &wav::endless_header(format()));

        // The exact samples, in order, converted the way `rincon_audio::convert` converts them.
        let mut expected = Vec::new();
        rincon_audio::convert::f32_to_i16_le_bytes(&script[0], &mut expected);
        let audio = &body[wav::HEADER_LEN..];
        assert!(audio.len() >= expected.len(), "audio was truncated");
        assert_eq!(&audio[..expected.len()], &expected[..], "the byte stream was not faithful");

        // Everything after the script is the silence the pump emits on underrun, never noise.
        assert!(
            audio[expected.len()..].iter().all(|&b| b == 0),
            "an underrun produced something other than silence"
        );

        assert!(first_byte < Duration::from_millis(500), "first byte took {first_byte:?}");
        handle.shutdown().await;
    }

    /// Covers: RQ-STRM-011
    #[tokio::test]
    async fn rq_strm_011_underrun_emits_silence_not_stall() {
        // An empty script means the ring is starved from the first tick. The connection must
        // stay open and carry silence; stalling would let the player's buffer drain.
        let config = StreamConfig::new(bind_addr(), LOOPBACK, format());
        let counters = SessionCounters::new();
        let (_tx, rx) = rincon_audio::ring::channel(format(), 500, Arc::clone(&counters));
        let handle = bind(config, rx, counters).await.unwrap();

        let response = client().get(handle.url.as_str()).send().await.unwrap();
        assert_eq!(response.status(), 200);

        // Read a bounded prefix: the stream is endless, so the test must stop, not the server.
        let mut stream = response;
        let mut collected = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(400);
        while tokio::time::Instant::now() < deadline && collected.len() < 4096 {
            match tokio::time::timeout(Duration::from_millis(200), stream.chunk()).await {
                Ok(Ok(Some(chunk))) => collected.extend_from_slice(&chunk),
                _ => break,
            }
        }

        assert!(collected.len() > wav::HEADER_LEN, "the socket stalled instead of sending silence");
        assert!(
            collected[wav::HEADER_LEN..].iter().all(|&b| b == 0),
            "an underrun must produce digital silence, not stale or random audio"
        );

        // Release the endless response before shutting down. `Handle::shutdown` would force
        // the listener closed anyway after its grace period, but a test should not spend it.
        drop(stream);
        handle.shutdown().await;
    }

    /// Covers: RQ-STRM-008
    #[tokio::test]
    async fn rq_strm_008_enforces_connection_cap() {
        let config = StreamConfig::new(bind_addr(), LOOPBACK, format()).with_max_connections(1);
        let handle = serve_live(config).await;

        // Hold the first connection open by not draining it.
        let first = client().get(handle.url.as_str()).send().await.unwrap();
        assert_eq!(first.status(), 200);

        let second = client().get(handle.url.as_str()).send().await.unwrap();
        assert_eq!(second.status(), 503, "a second consumer must be refused, not served");

        // The first stream is undisturbed by the refusal.
        assert_eq!(first.status(), 200);
        drop(first);
        drop(second);
        handle.shutdown().await;
    }

    /// Covers: RQ-STRM-009
    #[tokio::test]
    async fn rq_strm_009_survives_consumer_disconnect() {
        let config = StreamConfig::new(bind_addr(), LOOPBACK, format());
        let handle = serve_live(config).await;

        // Connect, take a little audio, then vanish the way a speaker does on a Wi-Fi blip.
        {
            let mut first = client().get(handle.url.as_str()).send().await.unwrap();
            assert_eq!(first.status(), 200);
            let _ = tokio::time::timeout(Duration::from_millis(200), first.chunk()).await;
        }

        // The audio source must come back to the pool, so a reconnect is served rather than
        // refused. Without the `SourceLease` drop guard this is where the session dies.
        let mut reconnected = None;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let response = client().get(handle.url.as_str()).send().await.unwrap();
            if response.status() == 200 {
                reconnected = Some(response);
                break;
            }
        }

        let response = reconnected.expect("the server never accepted a reconnect");
        assert_eq!(response.status(), 200);
        assert_eq!(handle.active_connections(), 1);

        // Surviving the reconnect is not enough: it has to be *counted*. Nothing called
        // `record_reconnect` until this assertion existed, so the interface showed
        // "Reconnects 0" for a session that had visibly reconnected -- and a Sonos reconnects
        // routinely, including once during its own startup.
        assert_eq!(
            handle.counters().reconnects(),
            1,
            "a reconnect the server served must reach the counters"
        );

        drop(response);
        handle.shutdown().await;
    }

    /// Covers: RQ-STRM-015
    #[tokio::test]
    async fn rq_strm_015_serving_continues_past_the_declared_size() {
        // The declared `data` size is a 32-bit field and cannot describe an endless stream, so
        // the real requirement is that we ignore it. With a deliberately tiny declaration, a
        // server that honoured the field would stop after 64 bytes.
        let config =
            StreamConfig::new(bind_addr(), LOOPBACK, format()).with_declared_data_bytes(64);
        let script = vec![vec![0.5_f32; 4096]];
        let handle = serve_script(config, &script).await;

        let body = client().get(handle.url.as_str()).send().await.unwrap().bytes().await.unwrap();
        let audio_len = body.len() - wav::HEADER_LEN;

        assert!(
            audio_len > 64,
            "served only {audio_len} bytes of audio; the declared size was treated as a limit"
        );
        // And the header still declares what it was configured to declare.
        let declared = u32::from_le_bytes([body[40], body[41], body[42], body[43]]);
        assert_eq!(declared, 64);
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn the_url_handed_to_a_speaker_resolves_to_the_bound_socket() {
        let config = StreamConfig::new(bind_addr(), LOOPBACK, format());
        let handle = serve_script(config, &[vec![0.1; 64]]).await;

        assert!(handle.url.as_str().contains(&handle.local_addr.port().to_string()));
        assert!(handle.url.as_str().ends_with("/stream.wav"));
        assert!(!handle.url.redacted().contains(handle.url.token()), "the token must not leak");
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn a_peer_connecting_is_observable_for_firewall_detection() {
        let config = StreamConfig::new(bind_addr(), LOOPBACK, format());
        let handle = serve_script(config, &[vec![0.1; 64]]).await;

        assert!(!handle.peer_connected(), "nothing has connected yet");
        let _ = client().head(handle.url.as_str()).send().await.unwrap();
        assert!(handle.peer_connected(), "the engine cannot tell firewall from buffering");
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn a_wildcard_bind_is_refused_before_a_socket_is_opened() {
        let config =
            StreamConfig::new(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0), LOOPBACK, format());
        let counters = SessionCounters::new();
        let (_tx, rx) = rincon_audio::ring::channel(format(), 100, Arc::clone(&counters));

        let error = bind(config, rx, counters).await.unwrap_err();
        assert!(matches!(error, StreamError::Config(ConfigError::WildcardBindNotAllowed(_))));
    }

    #[tokio::test]
    async fn shutdown_is_bounded_even_with_a_live_stream_attached() {
        // The failure this pins down actually happened: a purely graceful shutdown waits for
        // the in-flight response to finish, and the in-flight response here is *endless*, so
        // the wait never ends. Clicking Stop must not hang the interface.
        let config = StreamConfig::new(bind_addr(), LOOPBACK, format());
        let handle = serve_live(config).await;

        let held = client().get(handle.url.as_str()).send().await.unwrap();
        assert_eq!(held.status(), 200);

        let started = std::time::Instant::now();
        // `held` is deliberately still alive across the shutdown.
        handle.shutdown().await;
        let elapsed = started.elapsed();

        assert!(elapsed < Duration::from_secs(3), "shutdown took {elapsed:?} with a live stream");
        drop(held);
    }

    #[tokio::test]
    async fn shutting_down_releases_the_port() {
        let config = StreamConfig::new(bind_addr(), LOOPBACK, format());
        let handle = serve_script(config, &[vec![0.1; 64]]).await;
        let addr = handle.local_addr;
        handle.shutdown().await;

        // Rebinding the same port proves the listener is really gone rather than merely idle.
        let rebound = tokio::net::TcpListener::bind(addr).await;
        assert!(rebound.is_ok(), "the port was still held after shutdown: {rebound:?}");
    }
}
