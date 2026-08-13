//! Shared JSON-framed service owner, per-connection dispatch and watch stream.
//!
//! One `JsonService` owns the application adapter and the bounded session
//! registry shared by every transport connection. Each accepted connection
//! runs one task that reads length-prefixed JSON request frames and writes
//! result/error frames; a `WatchEvents` request switches the connection to a
//! continuous watch stream on a dedicated connection until the peer
//! disconnects.

use std::sync::{Arc, Mutex};

use caly_application::service::ApplicationServicePort;
use caly_domain::UnixMillis;
use caly_protocol::{
    framing::{FrameError, decode_json, encode_json, read_frame, write_frame},
    protocol::v2::HandshakeRequest,
    wire_frames::{
        ClientFrame, JsonRequest, JsonResponse, ServerFrame, WireError, WireErrorCode, wire_error,
    },
};
use tokio::io::{AsyncRead, AsyncWrite};

use super::{
    ProtocolSession, ServiceAdapter, ServiceError, ServiceV2, SessionError, SessionRegistry,
    event_to_wire, parse_session_token, unix_millis,
};
use crate::json::SESSION_TTL_MS;

/// Shared JSON-framed service owner.
pub struct JsonService<A> {
    state: Arc<Mutex<ServiceState<A>>>,
}

struct ServiceState<A> {
    adapter: ServiceAdapter<A>,
    stop_notifier: Option<Arc<tokio::sync::Notify>>,
    registry: SessionRegistry,
}

impl<A> Clone for JsonService<A> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

/// Idle connections are dropped after this long without a frame
/// (2026-08-12 agent audit: a peer that connects and never sends a
/// frame must not pin a session task forever).
const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

impl<A> JsonService<A> {
    /// Wraps one application adapter in a shared service owner. The bounded
    /// session registry capacity is a compile-time constant, so construction
    /// only fails if that bound is ever violated; the error is surfaced
    /// instead of aborting the daemon.
    ///
    /// `stop_notifier` (Round 17): the daemon runtime
    /// passes its `Notify` here. When a `StopDaemon`
    /// operation completes successfully, `execute`
    /// triggers the notifier, which breaks the
    /// transport's serve loop and lets the daemon
    /// exit cleanly. `None` disables the stop signal
    /// (used by tests that don't run a transport).
    pub fn new(
        adapter: ServiceAdapter<A>,
        stop_notifier: Option<Arc<tokio::sync::Notify>>,
    ) -> Result<Self, SessionError> {
        Ok(Self {
            state: Arc::new(Mutex::new(ServiceState {
                adapter,
                stop_notifier,
                // Bounded; token collisions are resolved by refresh-on-handshake.
                registry: SessionRegistry::new(super::MAX_PROTOCOL_SESSIONS)?,
            })),
        })
    }

    /// Number of currently live sessions; used by tests and diagnostics.
    pub fn live_sessions(&self) -> usize {
        self.state.lock().map_or(0, |state| state.registry.len())
    }

    /// Transport message ceiling (bytes) derived from the decode limits.
    pub fn message_ceiling(&self) -> usize {
        self.state
            .lock()
            .map_or(0, |state| state.adapter.limits().max_message_bytes)
    }

    /// Serves one accepted connection until the peer disconnects or the frame
    /// stream becomes unrecoverable.
    pub async fn handle_connection<S>(&self, stream: S)
    where
        A: ApplicationServicePort + Send,
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let ceiling = self.message_ceiling();
        loop {
            // Idle timeout: a peer that connects and never sends a frame
            // must not pin a task (and a session slot) forever — 2026-08-12
            // agent audit: repeated idle connections could exhaust the
            // session registry. 60s of silence ends the connection.
            let payload =
                match tokio::time::timeout(IDLE_TIMEOUT, read_frame(&mut reader, ceiling)).await {
                    Ok(Ok(payload)) => payload,
                    Ok(Err(FrameError::Closed | FrameError::Io)) => break,
                    Ok(Err(FrameError::Oversized { .. })) => {
                        // Framing cannot resynchronize after an oversized header.
                        let frame = ServerFrame::Error(wire_error(
                            WireErrorCode::InvalidArgument,
                            "frame exceeds the negotiated message ceiling",
                        ));
                        let _ = write_json_frame(&mut writer, &frame).await;
                        break;
                    }
                    // Timeout or an internal error: drop the connection.
                    Err(_) => break,
                };
            let client = match decode_json::<ClientFrame>(&payload) {
                Ok(client) => client,
                Err(error) => {
                    let frame = ServerFrame::Error(wire_error(
                        WireErrorCode::InvalidArgument,
                        format!("malformed request frame: {error}"),
                    ));
                    let _ = write_json_frame(&mut writer, &frame).await;
                    continue;
                }
            };
            match client.request {
                JsonRequest::Handshake(request) => match self.handshake(request) {
                    Ok(response) => {
                        let _ = write_json_frame(
                            &mut writer,
                            &ServerFrame::Result(Box::new(JsonResponse::Handshake(response))),
                        )
                        .await;
                    }
                    Err(error) => {
                        let _ = write_json_frame(&mut writer, &ServerFrame::Error(error)).await;
                    }
                },
                JsonRequest::WatchEvents(request) => {
                    if let Err(error) = self.validate_session(client.session) {
                        let _ = write_json_frame(&mut writer, &ServerFrame::Error(error)).await;
                        continue;
                    }
                    // The connection becomes a dedicated watch stream.
                    self.stream_watch(writer, request).await;
                    break;
                }
                other => {
                    let outcome = match self.validate_session(client.session) {
                        Ok(()) => self.dispatch(other).await,
                        Err(error) => Err(error),
                    };
                    match outcome {
                        Ok(response) => {
                            let _ = write_json_frame(
                                &mut writer,
                                &ServerFrame::Result(Box::new(response)),
                            )
                            .await;
                        }
                        Err(error) => {
                            let _ = write_json_frame(&mut writer, &ServerFrame::Error(error)).await;
                        }
                    }
                }
            }
        }
    }

    /// Registers (or refreshes) the daemon session token with a real expiry so
    /// subsequent requests are validated against the live registry.
    fn handshake(
        &self,
        request: HandshakeRequest,
    ) -> Result<caly_protocol::protocol::v2::HandshakeResponse, WireError>
    where
        A: ApplicationServicePort,
    {
        let mut state = lock_state(&self.state)?;
        let response = state.adapter.handshake(request).map_err(service_to_wire)?;
        let now = unix_millis();
        state.registry.purge_expired(now);
        let session = ProtocolSession {
            token: response.session_token,
            version: response.negotiated_version,
            features: response.enabled_features.clone(),
            expires_at: UnixMillis::new(now.value().saturating_add(SESSION_TTL_MS)),
        };
        state.registry.refresh(session).map_err(session_to_wire)?;
        Ok(response)
    }

    /// Validates the request's hex session token against the live registry.
    fn validate_session(&self, session: Option<String>) -> Result<(), WireError> {
        let value = session.ok_or_else(|| {
            wire_error(WireErrorCode::Unauthenticated, "session token is required")
        })?;
        let token = parse_session_token(&value)
            .map_err(|_| wire_error(WireErrorCode::Unauthenticated, "session token is invalid"))?;
        let mut state = lock_state(&self.state)?;
        let now = unix_millis();
        state.registry.purge_expired(now);
        state
            .registry
            .validate(token, now)
            .map(|_| ())
            .map_err(session_to_wire)
    }
}

mod rpc;
mod watch;

pub use watch::WatchStream;

/// Serializes and writes one JSON-framed server frame.
async fn write_json_frame<W>(writer: &mut W, frame: &ServerFrame) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let payload = encode_json(frame).map_err(|_| FrameError::Io)?;
    write_frame(writer, &payload).await
}

fn lock_state<A>(
    state: &Arc<Mutex<ServiceState<A>>>,
) -> Result<std::sync::MutexGuard<'_, ServiceState<A>>, WireError> {
    state.lock().map_err(|_| {
        wire_error(
            WireErrorCode::ApplicationUnavailable,
            "service state lock poisoned",
        )
    })
}

fn service_to_wire(error: ServiceError) -> WireError {
    match error {
        ServiceError::Unauthenticated | ServiceError::HandshakeRequired => wire_error(
            WireErrorCode::Unauthenticated,
            "handshake or session authentication required",
        ),
        ServiceError::PermissionDenied => {
            wire_error(WireErrorCode::PermissionDenied, "permission denied")
        }
        ServiceError::ResourceExhausted => wire_error(
            WireErrorCode::ResourceExhausted,
            "bounded resource exhausted",
        ),
        ServiceError::InvalidArgument { reason } => {
            wire_error(WireErrorCode::InvalidArgument, reason)
        }
        ServiceError::IncompatibleVersion => wire_error(
            WireErrorCode::IncompatibleVersion,
            "incompatible protocol version",
        ),
        ServiceError::ApplicationUnavailable => wire_error(
            WireErrorCode::ApplicationUnavailable,
            "application unavailable",
        ),
    }
}

fn session_to_wire(error: SessionError) -> WireError {
    match error {
        SessionError::CapacityReached => {
            wire_error(WireErrorCode::ResourceExhausted, "session capacity reached")
        }
        SessionError::TokenCollision | SessionError::InvalidToken => wire_error(
            WireErrorCode::Unauthenticated,
            "session token collision or invalid",
        ),
        SessionError::Missing | SessionError::Expired => wire_error(
            WireErrorCode::Unauthenticated,
            "session token is missing or expired",
        ),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod live_watch_tests;
