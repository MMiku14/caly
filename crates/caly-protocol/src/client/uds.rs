//! Synchronous thin client over a Linux Unix-domain socket.
//!
//! The transport is length-prefixed compact JSON (see `framing`): one
//! synchronous request/response exchange per frame on the main connection,
//! plus a dedicated connection for the continuous watch stream. No gRPC or
//! protobuf machinery is involved; the wire is directly inspectable.

use std::{path::PathBuf, sync::Arc, time::Duration};

use tokio::{net::UnixStream, runtime::Runtime};

use super::{ClientContract, ClientError};
use crate::{
    framing::{FrameError, decode_json, encode_json, read_frame, write_frame},
    protocol::v2::{
        CancelOperationRequest, CancelOperationResponse, DecodeLimits, ExecuteRequest,
        ExecuteResponse, GetOperationStatusRequest, HandshakeRequest, HandshakeResponse,
        WatchEventsRequest, WatchResponse, WireId, WireOperationStatus, WirePresentationSnapshot,
    },
    wire_frames::{ClientFrame, JsonRequest, JsonResponse, ServerFrame, WireError, WireErrorCode},
};

/// Overall deadline for one non-watch request/response exchange (#109): a
/// wedged daemon used to hang every CLI call forever because framing reads
/// carried no timeout. Watch frames keep their own continuous-stream
/// semantics (the iterator's job is to block).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Thin JSON-framed UDS client with one runtime and one session-scoped socket.
pub struct UdsClient {
    runtime: Runtime,
    stream: UnixStream,
    socket: Arc<PathBuf>,
    session_token: Option<WireId>,
    message_ceiling: usize,
}

impl UdsClient {
    /// Connects to an owner-only Linux UDS.
    pub fn connect(socket: PathBuf) -> Result<Self, ClientError> {
        // A CLI invocation is one sequential UDS conversation: a
        // current-thread runtime (single worker, no extra threads) is the
        // right shape — the pre-change `Runtime::new()` spawned one Tokio
        // worker per CPU core on *every* `caly …` command (~4 threads +
        // per-thread stacks just to talk to the daemon once).
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| ClientError::TransportUnavailable)?;
        let socket_ref = Arc::new(socket);
        let stream = runtime
            .block_on(UnixStream::connect((*socket_ref).clone()))
            .map_err(|_| ClientError::TransportUnavailable)?;
        // Transport message ceiling derived from the negotiated decode limits.
        let ceiling = DecodeLimits::v2_default().max_message_bytes;
        Ok(Self {
            runtime,
            stream,
            socket: socket_ref,
            session_token: None,
            message_ceiling: ceiling,
        })
    }

    /// Returns the configured socket path.
    pub fn socket(&self) -> &PathBuf {
        &self.socket
    }

    /// Sends one request frame and reads one response frame.
    fn exchange(&mut self, request: JsonRequest) -> Result<JsonResponse, ClientError> {
        let frame = ClientFrame {
            session: self.session_token.map(hex_token),
            request,
        };
        let payload = encode_json(&frame).map_err(decode_rejected)?;
        // The timeout future must be CONSTRUCTED inside `block_on`: tokio
        // registers the `Sleep` eagerly at construction and panics ("no
        // reactor running") when built as an argument outside the runtime
        // context (caught by the daemon mock e2e).
        self.runtime
            .block_on(async {
                tokio::time::timeout(REQUEST_TIMEOUT, write_frame(&mut self.stream, &payload)).await
            })
            .map_err(|_| ClientError::DeadlineExceeded)?
            .map_err(map_frame)?;
        let payload = self
            .runtime
            .block_on(async {
                tokio::time::timeout(
                    REQUEST_TIMEOUT,
                    read_frame(&mut self.stream, self.message_ceiling),
                )
                .await
            })
            .map_err(|_| ClientError::DeadlineExceeded)?
            .map_err(map_frame)?;
        let server = decode_json::<ServerFrame>(&payload).map_err(decode_rejected)?;
        match server {
            ServerFrame::Result(response) => Ok(*response),
            ServerFrame::Error(error) => Err(map_wire_error(error)),
        }
    }
}

impl ClientContract for UdsClient {
    type Watch = UdsWatchIter;

    fn handshake(&mut self, request: HandshakeRequest) -> Result<HandshakeResponse, ClientError> {
        let response = self.exchange(JsonRequest::Handshake(request))?;
        match response {
            JsonResponse::Handshake(value) => {
                self.session_token = Some(value.session_token);
                // Audit #96: tighten the transport ceiling to the negotiated
                // limits — the pre-fix client kept the v2 default even when
                // the daemon negotiated a lower `max_message_bytes`.
                self.message_ceiling = value.limits.max_message_bytes;
                Ok(value)
            }
            other => Err(unexpected("handshake", other)),
        }
    }

    fn execute(&mut self, request: ExecuteRequest) -> Result<ExecuteResponse, ClientError> {
        let response = self.exchange(JsonRequest::Execute(request))?;
        match response {
            JsonResponse::Execute(value) => Ok(value),
            other => Err(unexpected("execute", other)),
        }
    }

    fn cancel(
        &mut self,
        request: CancelOperationRequest,
    ) -> Result<CancelOperationResponse, ClientError> {
        let response = self.exchange(JsonRequest::CancelOperation(request))?;
        match response {
            JsonResponse::CancelOperation(value) => Ok(value),
            other => Err(unexpected("cancel", other)),
        }
    }

    fn operation_status(
        &mut self,
        request: GetOperationStatusRequest,
    ) -> Result<WireOperationStatus, ClientError> {
        let response = self.exchange(JsonRequest::GetOperationStatus(request))?;
        match response {
            JsonResponse::GetOperationStatus(value) => Ok(value),
            other => Err(unexpected("operation status", other)),
        }
    }

    fn snapshot(&mut self) -> Result<WirePresentationSnapshot, ClientError> {
        let response = self.exchange(JsonRequest::GetSnapshot(
            crate::protocol::v2::GetSnapshotRequest,
        ))?;
        match response {
            JsonResponse::GetSnapshot(value) => Ok(value),
            other => Err(unexpected("snapshot", other)),
        }
    }

    fn watch(&mut self, request: WatchEventsRequest) -> Result<Self::Watch, ClientError> {
        // A watch is a continuous stream: it owns a dedicated connection so
        // the request/response connection stays available for later calls.
        let mut stream = self
            .runtime
            .block_on(UnixStream::connect((*self.socket).clone()))
            .map_err(|_| ClientError::TransportUnavailable)?;
        let frame = ClientFrame {
            session: self.session_token.map(hex_token),
            request: JsonRequest::WatchEvents(request),
        };
        let payload = encode_json(&frame).map_err(decode_rejected)?;
        self.runtime
            .block_on(write_frame(&mut stream, &payload))
            .map_err(map_frame)?;
        Ok(UdsWatchIter {
            handle: self.runtime.handle().clone(),
            stream,
            message_ceiling: self.message_ceiling,
            finished: false,
        })
    }
}

/// Lazy incremental watch iterator over the daemon's continuous live stream.
pub struct UdsWatchIter {
    handle: tokio::runtime::Handle,
    stream: UnixStream,
    message_ceiling: usize,
    finished: bool,
}

impl Iterator for UdsWatchIter {
    type Item = Result<WatchResponse, ClientError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let payload = self
            .handle
            .block_on(read_frame(&mut self.stream, self.message_ceiling));
        let payload = match payload {
            Ok(payload) => payload,
            Err(FrameError::Closed) => {
                self.finished = true;
                return None;
            }
            Err(error) => {
                self.finished = true;
                return Some(Err(map_frame(error)));
            }
        };
        let server = match decode_json::<ServerFrame>(&payload) {
            Ok(server) => server,
            Err(error) => {
                self.finished = true;
                return Some(Err(decode_rejected(error)));
            }
        };
        match server {
            ServerFrame::Result(boxed) => match *boxed {
                JsonResponse::WatchEvent(value) => Some(Ok(value)),
                other => {
                    self.finished = true;
                    Some(Err(ClientError::DecodeRejected {
                        reason: format!("watch stream received an unexpected frame: {other:?}"),
                        suggested_action: "the daemon and client protocol versions may differ"
                            .to_owned(),
                    }))
                }
            },
            ServerFrame::Error(error) => {
                self.finished = true;
                Some(Err(map_wire_error(error)))
            }
        }
    }
}

fn hex_token(token: WireId) -> String {
    caly_domain::to_hex(token)
}

fn map_frame(error: FrameError) -> ClientError {
    match error {
        FrameError::Closed | FrameError::Io => ClientError::TransportUnavailable,
        FrameError::Oversized { .. } => ClientError::ResourceExhausted,
    }
}

fn map_wire_error(error: WireError) -> ClientError {
    match WireErrorCode::from_code(error.code) {
        Some(WireErrorCode::Unauthenticated | WireErrorCode::HandshakeRequired) => {
            ClientError::AuthenticationRejected
        }
        Some(WireErrorCode::ResourceExhausted) => ClientError::ResourceExhausted,
        Some(WireErrorCode::IncompatibleVersion) => ClientError::IncompatibleVersion,
        Some(WireErrorCode::PermissionDenied | WireErrorCode::ApplicationUnavailable) => {
            ClientError::TransportUnavailable
        }
        Some(WireErrorCode::InvalidArgument) | None => ClientError::DecodeRejected {
            reason: error.message,
            suggested_action: error.suggested_action.unwrap_or_else(|| {
                "upgrade the client or inspect daemon protocol diagnostics".to_owned()
            }),
        },
    }
}

fn decode_rejected(error: serde_json::Error) -> ClientError {
    ClientError::DecodeRejected {
        reason: format!("wire decode failed: {error}"),
        suggested_action: "upgrade the client or inspect daemon protocol diagnostics".to_owned(),
    }
}

fn unexpected(method: &str, response: JsonResponse) -> ClientError {
    ClientError::DecodeRejected {
        reason: format!("{method} received an unexpected response frame: {response:?}"),
        suggested_action: "the daemon and client protocol versions may differ".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spins up a one-shot in-process UDS peer answering a single request
    /// frame with a handshake response carrying the given negotiated limits;
    /// returns the socket path and the server thread handle.
    fn stub_handshake_server(limits: DecodeLimits) -> (PathBuf, std::thread::JoinHandle<()>) {
        use std::io::{Read as _, Write as _};
        let unique = format!(
            "caly-proto-uds-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos()),
        );
        let path = std::env::temp_dir().join(unique);
        let listener = std::os::unix::net::UnixListener::bind(&path)
            .unwrap_or_else(|error| panic!("bind test socket: {error}"));
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .unwrap_or_else(|error| panic!("accept test client: {error}"));
            let mut header = [0_u8; 4];
            stream
                .read_exact(&mut header)
                .unwrap_or_else(|error| panic!("read request header: {error}"));
            let length = u32::from_le_bytes(header) as usize;
            let mut payload = vec![0_u8; length];
            stream
                .read_exact(&mut payload)
                .unwrap_or_else(|error| panic!("read request payload: {error}"));
            let response =
                ServerFrame::Result(Box::new(JsonResponse::Handshake(HandshakeResponse {
                    negotiated_version: crate::protocol::v2::ProtocolVersion { major: 2, minor: 0 },
                    daemon_instance_id: [7_u8; 16],
                    enabled_features: crate::protocol::v2::all_features(),
                    unknown_requested_features: crate::protocol::v2::FeatureList::new(),
                    limits,
                    session_token: [9_u8; 16],
                })));
            let bytes = encode_json(&response)
                .unwrap_or_else(|error| panic!("encode handshake response: {error}"));
            let length = u32::try_from(bytes.len())
                .unwrap_or_else(|error| panic!("response length overflow: {error}"));
            stream
                .write_all(&length.to_le_bytes())
                .and_then(|()| stream.write_all(&bytes))
                .unwrap_or_else(|error| panic!("write handshake response: {error}"));
        });
        (path, server)
    }

    #[test]
    fn handshake_adopts_the_negotiated_message_ceiling() {
        let tightened = DecodeLimits {
            max_message_bytes: 4_096,
            ..DecodeLimits::v2_default()
        };
        let (path, server) = stub_handshake_server(tightened);
        let mut client = UdsClient::connect(path.clone())
            .unwrap_or_else(|error| panic!("connect stub server: {error}"));
        assert_eq!(
            client.message_ceiling,
            DecodeLimits::v2_default().max_message_bytes,
            "pre-handshake ceiling is the conservative v2 default",
        );
        client
            .handshake(HandshakeRequest {
                client_version: crate::protocol::v2::ProtocolVersion { major: 2, minor: 0 },
                requested_features: crate::protocol::v2::all_features(),
                auth_token: None,
            })
            .unwrap_or_else(|error| panic!("handshake: {error}"));
        assert_eq!(
            client.message_ceiling, tightened.max_message_bytes,
            "handshake must tighten the read ceiling to the negotiated limit (#96)",
        );
        server
            .join()
            .unwrap_or_else(|payload| panic!("stub server panicked: {payload:?}"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wire_error_codes_map_to_client_errors() {
        let unauthenticated = map_wire_error(WireError {
            code: WireErrorCode::Unauthenticated.code(),
            message: "token rejected".to_owned(),
            suggested_action: None,
        });
        assert_eq!(unauthenticated, ClientError::AuthenticationRejected);
        let exhausted = map_wire_error(WireError {
            code: WireErrorCode::ResourceExhausted.code(),
            message: "full".to_owned(),
            suggested_action: None,
        });
        assert_eq!(exhausted, ClientError::ResourceExhausted);
        let version = map_wire_error(WireError {
            code: WireErrorCode::IncompatibleVersion.code(),
            message: "old".to_owned(),
            suggested_action: None,
        });
        assert_eq!(version, ClientError::IncompatibleVersion);
        let invalid = map_wire_error(WireError {
            code: WireErrorCode::InvalidArgument.code(),
            message: "bad command".to_owned(),
            suggested_action: Some("fix the command".to_owned()),
        });
        assert!(matches!(invalid, ClientError::DecodeRejected { .. }));
        let unknown = map_wire_error(WireError {
            code: 999,
            message: "?".to_owned(),
            suggested_action: None,
        });
        assert!(matches!(unknown, ClientError::DecodeRejected { .. }));
    }
}
