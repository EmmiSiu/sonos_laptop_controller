//! The HTTP server a Sonos player pulls audio from.
//!
//! # Why the laptop becomes a server
//!
//! A Sonos player cannot be pushed to. You hand it a URL and it fetches the audio itself. So
//! the laptop has to open a listening socket on the home network and serve the user's system
//! audio from it — which makes this the most security-sensitive file in the project, and the
//! reason SPEC-002 spends more space on refusal than on streaming.
//!
//! # Request handling order
//!
//! ```text
//!   any path that is not the stream route  ──► 404  (no other surface exists)
//!   peer not in the allowlist              ──► 403  (cheapest check, done first)
//!   token mismatch                         ──► 404  (indistinguishable from "no route")
//!   connection cap reached                 ──► 503  (the active stream is undisturbed)
//!   HEAD                                   ──► headers, no body
//!   GET                                    ──► header + endless PCM
//! ```
//!
//! The allowlist is checked before the token so that a stranger never causes a cryptographic
//! comparison, and so a flood of probes costs almost nothing. The trade-off — a stranger can
//! tell the route pattern exists, because they get 403 rather than 404 — is accepted: they
//! learn a path shape that is in this public source file anyway, and they still need 128 bits.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Router, body::Body};
use bytes::{Bytes, BytesMut};
use rincon_audio::convert;
use rincon_audio::ring::{FrameReceiver, RingError};
use rincon_core::audio::AudioFormat;
use rincon_core::metrics::{DropLayer, SessionCounters};
use rincon_core::stream_url::StreamUrl;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, warn};

use crate::config::{ConfigError, Framing, StreamConfig};
use crate::token::SessionToken;
use crate::wav;

/// How often the serving task moves audio to the socket.
const TICK: Duration = Duration::from_millis(10);

/// How much audio one tick may drain, in milliseconds. Larger than [`TICK`] so a late wake-up
/// catches up instead of falling permanently behind.
const DRAIN_MS: u32 = 30;

/// Buffered chunks between the serving task and the socket. Small: this is not where audio
/// should queue up, the ring is.
const CHANNEL_DEPTH: usize = 8;

/// Why the server could not start.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    /// The configuration was refused.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// The listening socket could not be opened.
    #[error("cannot bind {addr}: {source}")]
    Bind {
        /// The address we tried.
        addr: SocketAddr,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },

    /// The OS entropy source failed, so no token could be generated.
    #[error("cannot generate a session token: {0}")]
    Entropy(String),
}

/// State shared with every request handler.
#[derive(Debug)]
struct AppState {
    config: StreamConfig,
    token: SessionToken,
    /// The single audio consumer, taken by an active stream and returned on disconnect.
    ///
    /// A `std::sync::Mutex` rather than Tokio's: it is held only long enough to take or replace
    /// an `Option`, never across an `await`, and a blocking mutex can be released from `Drop`,
    /// which is how the receiver comes back when a client vanishes.
    source: Mutex<Option<FrameReceiver>>,
    active: AtomicUsize,
    counters: Arc<SessionCounters>,
    peer_ever_connected: AtomicBool,
}

/// A running server.
#[derive(Debug)]
pub struct Handle {
    /// The URL to hand the speaker. Renders redacted in logs.
    pub url: StreamUrl,
    /// The address actually bound, with the ephemeral port resolved.
    pub local_addr: SocketAddr,
    state: Arc<AppState>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Handle {
    /// Whether a permitted peer has ever fetched the stream.
    ///
    /// The engine uses this to tell "the speaker is buffering" from "the firewall is blocking
    /// us", which are otherwise identical from the outside.
    #[must_use]
    pub fn peer_connected(&self) -> bool {
        self.state.peer_ever_connected.load(Ordering::Acquire)
    }

    /// How many connections are being served right now.
    #[must_use]
    pub fn active_connections(&self) -> usize {
        self.state.active.load(Ordering::Acquire)
    }

    /// Shared counters, for the health indicator and diagnostics.
    #[must_use]
    pub fn counters(&self) -> Arc<SessionCounters> {
        Arc::clone(&self.state.counters)
    }

    /// Stops the server and waits for it to finish.
    pub async fn shutdown(mut self) {
        if let Some(signal) = self.shutdown.take() {
            let _ = signal.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // A dropped handle must not leave a socket serving the user's audio. The graceful path
        // is `shutdown().await`; this is the backstop for panics and early returns.
        if let Some(signal) = self.shutdown.take() {
            let _ = signal.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Binds the stream server and starts serving.
///
/// # Errors
///
/// Returns [`StreamError`] when the configuration is refused, the socket cannot be opened, or
/// no token can be generated.
///
/// Covers: RQ-STRM-007, RQ-STRM-012, RQ-STRM-013
pub async fn bind(
    config: StreamConfig,
    source: FrameReceiver,
    counters: Arc<SessionCounters>,
) -> Result<Handle, StreamError> {
    config.validate()?;

    let token = SessionToken::generate().map_err(|source| StreamError::Entropy(source.to_string()))?;

    let listener = TcpListener::bind(config.bind)
        .await
        .map_err(|source| StreamError::Bind { addr: config.bind, source })?;
    let local_addr =
        listener.local_addr().map_err(|source| StreamError::Bind { addr: config.bind, source })?;

    let url = StreamUrl::new(local_addr.ip(), local_addr.port(), token.expose());

    let state = Arc::new(AppState {
        config,
        token,
        source: Mutex::new(Some(source)),
        active: AtomicUsize::new(0),
        counters,
        peer_ever_connected: AtomicBool::new(false),
    });

    // Exactly one route. Everything else -- `/`, `/..`, `/admin`, a traversal attempt -- hits
    // the fallback, which is a bare 404 with no body to learn anything from.
    let app = Router::new()
        .route("/s/{token}/stream.wav", get(stream).head(stream))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .with_state(Arc::clone(&state));

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let served = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        if let Err(error) = served.await {
            warn!(%error, "stream server stopped");
        }
    });

    info!(%local_addr, url = %url, "stream server listening");
    Ok(Handle { url, local_addr, state, shutdown: Some(shutdown_tx), task: Some(task) })
}

/// Decrements the active-connection count however the handler exits.
#[derive(Debug)]
struct ConnectionSlot(Arc<AppState>);

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Returns the audio consumer to the pool when a stream ends, however it ends.
#[derive(Debug)]
struct SourceLease {
    state: Arc<AppState>,
    receiver: Option<FrameReceiver>,
}

impl Drop for SourceLease {
    fn drop(&mut self) {
        if let Some(receiver) = self.receiver.take() {
            let mut slot = self.state.source.lock().unwrap_or_else(PoisonError::into_inner);
            *slot = Some(receiver);
        }
    }
}

/// The one route.
async fn stream(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    method: Method,
) -> Response {
    // Covers: RQ-STRM-004
    if !state.config.permits(peer.ip()) {
        warn!(peer = %redact(peer.ip()), "refusing a stream request from an unallowlisted peer");
        return StatusCode::FORBIDDEN.into_response();
    }

    // Covers: RQ-STRM-005, RQ-STRM-006
    if !state.token.matches(&token) {
        debug!(peer = %redact(peer.ip()), "token mismatch");
        return StatusCode::NOT_FOUND.into_response();
    }

    // Covers: RQ-STRM-008
    let previous = state.active.fetch_add(1, Ordering::AcqRel);
    let slot = ConnectionSlot(Arc::clone(&state));
    if previous >= state.config.max_connections {
        debug!(peer = %redact(peer.ip()), "connection cap reached");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }

    state.peer_ever_connected.store(true, Ordering::Release);

    // Covers: RQ-STRM-010
    if method == Method::HEAD {
        drop(slot);
        return (headers(state.config.framing), Body::empty()).into_response();
    }

    // Covers: RQ-STRM-009
    let taken = {
        let mut source = state.source.lock().unwrap_or_else(PoisonError::into_inner);
        source.take()
    };
    let Some(receiver) = taken else {
        // A previous connection still holds the consumer; it will be returned when that task
        // notices the socket is gone. Asking again shortly is the right answer.
        debug!("audio source is still leased to a previous connection");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    info!(peer = %redact(peer.ip()), "serving audio");
    let framing = state.config.framing;
    let body = spawn_pump(state, receiver, slot);
    (headers(framing), body).into_response()
}

/// Response headers. Identical for `GET` and `HEAD`, by construction rather than by copy.
///
/// Covers: RQ-STRM-013
fn headers(framing: Framing) -> [(header::HeaderName, HeaderValue); 5] {
    // Both framings carry a WAV header, so the media type is the same; the difference is
    // whether a length is declared, which hyper decides from the body.
    let content_type = match framing {
        Framing::EndlessWav | Framing::Chunked => HeaderValue::from_static("audio/x-wav"),
    };
    [
        (header::CONTENT_TYPE, content_type),
        // There is nothing to seek to in a live stream, and claiming otherwise invites a
        // player to issue ranged requests we would only have to refuse.
        (header::ACCEPT_RANGES, HeaderValue::from_static("none")),
        (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        (header::CONNECTION, HeaderValue::from_static("close")),
        (
            header::HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ),
    ]
}

/// Starts the task that converts ring audio into socket bytes.
///
/// Covers: RQ-STRM-011, RQ-STRM-014, RQ-STRM-015
fn spawn_pump(state: Arc<AppState>, receiver: FrameReceiver, slot: ConnectionSlot) -> Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(CHANNEL_DEPTH);
    let format = state.config.format;
    let declared_data_bytes = state.config.declared_data_bytes;
    let channels = format.channels.get() as usize;
    let drain_samples = format.frames_in(DRAIN_MS) * channels;
    let silence_samples = format.frames_in(TICK.as_millis().try_into().unwrap_or(10)) * channels;

    let mut lease = SourceLease { state: Arc::clone(&state), receiver: Some(receiver) };

    tokio::spawn(async move {
        // Held for the task's lifetime so the connection count and the audio lease are both
        // released exactly when this task ends, whatever ends it.
        let _slot = slot;

        // The header goes out first and is never re-sent, even if the stream runs past the
        // size it declares (RQ-STRM-015).
        let head = wav::header(format, declared_data_bytes);
        if tx.send(Ok(Bytes::copy_from_slice(&head))).await.is_err() {
            return;
        }

        let mut scratch = vec![0.0_f32; drain_samples];
        let mut out = BytesMut::with_capacity(drain_samples * 2);
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;

            let Some(receiver) = lease.receiver.as_mut() else { break };
            let read = match receiver.read(&mut scratch) {
                Ok(count) => count,
                Err(RingError::DeviceLost) => {
                    debug!("capture ended; closing the stream");
                    break;
                }
            };

            out.clear();
            if read == 0 {
                // Covers: RQ-STRM-011 -- silence keeps the socket alive through a brief
                // underrun. Stalling would let the player's buffer drain and drop us.
                out.resize(silence_samples * 2, 0);
                state.counters.record_dropped(DropLayer::Socket, 0);
            } else {
                let mut bytes = Vec::with_capacity(read * 2);
                convert::f32_to_i16_le_bytes(scratch.get(..read).unwrap_or(&[]), &mut bytes);
                out.extend_from_slice(&bytes);
            }

            if tx.send(Ok(out.clone().freeze())).await.is_err() {
                debug!("consumer disconnected");
                break;
            }
        }
        // `lease` drops here, handing the receiver back so the next connection can stream.
    });

    Body::from_stream(ReceiverStream::new(rx))
}

/// Renders a peer address for a log line without the final octet.
fn redact(ip: IpAddr) -> String {
    rincon_core::error::redact_sensitive(&ip.to_string())
}
