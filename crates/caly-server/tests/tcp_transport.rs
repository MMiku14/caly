//! E2E: the configured loopback TCP transport serves a handshake and enables
//! every advertised feature. Uses a stub `ApplicationServicePort` so the test
//! exercises the real JSON-framed serve path without a daemon.

use caly_application::{
    command_bus::CommandEnvelope,
    events::SequencedEvent,
    service::{
        ApplicationServiceError, ApplicationServicePort, ApplicationWatch, CancellationResult,
    },
};
use caly_domain::{EventCursor, OperationId, OperationStatus, PresentationSnapshot};
use caly_protocol::{
    framing::{FrameError, decode_json, encode_json, read_frame, write_frame},
    protocol::v2::{DecodeLimits, HandshakeRequest, ProtocolVersion, all_features},
    wire_frames::{ClientFrame, JsonRequest, JsonResponse, ServerFrame},
};
use caly_server::{
    json::{JsonService, ServiceAdapter},
    tcp::serve_tcp_until,
};
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use tokio::net::TcpStream;

/// A handshake-only service stub; the handshake path does not touch any of the
/// data methods, so they may simply report unavailable.
struct Stub;

impl ApplicationServicePort for Stub {
    fn submit(
        &mut self,
        _envelope: CommandEnvelope,
    ) -> Result<OperationStatus, ApplicationServiceError> {
        Err(ApplicationServiceError::Unavailable)
    }
    fn cancel(
        &mut self,
        _operation_id: OperationId,
    ) -> Result<CancellationResult, ApplicationServiceError> {
        Err(ApplicationServiceError::Unavailable)
    }
    fn operation_status(
        &self,
        _operation_id: OperationId,
    ) -> Result<OperationStatus, ApplicationServiceError> {
        Err(ApplicationServiceError::Unavailable)
    }
    fn snapshot(&self) -> Result<PresentationSnapshot, ApplicationServiceError> {
        Err(ApplicationServiceError::Unavailable)
    }
    fn watch_after(
        &self,
        _cursor: Option<EventCursor>,
    ) -> Result<ApplicationWatch, ApplicationServiceError> {
        Err(ApplicationServiceError::Unavailable)
    }
    fn subscribe_live(&self) -> tokio::sync::broadcast::Receiver<SequencedEvent> {
        tokio::sync::broadcast::channel(1).0.subscribe()
    }
}

#[tokio::test]
async fn tcp_transport_serves_handshake_and_enables_features() -> Result<(), String> {
    // Reserve a free loopback port, then release it for the server to bind.
    let probe = StdTcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let listen: SocketAddr = probe.local_addr().map_err(|e| e.to_string())?;
    drop(probe);

    let adapter = ServiceAdapter::new(
        Stub,
        [0; 16],
        // Production injects the derived daemon session token here
        // (`session_token_for`, #28); the all-zero token is the session
        // registry's "no session" sentinel and is rejected as InvalidToken.
        [0xC0; 16],
        caly_server::admission::TokenAdmission::Open,
        all_features(),
        DecodeLimits::v2_default(),
    );
    let service = JsonService::new(adapter, None).map_err(|e| format!("{e:?}"))?;
    let server =
        tokio::spawn(async move { serve_tcp_until(listen, service, std::future::pending()).await });

    // The listener is bound asynchronously; retry the TCP connect until the
    // server is ready (and surface an early server error instead of hanging).
    let mut stream = None;
    for _ in 0..50 {
        if server.is_finished() {
            return Err(format!("server exited early: {:?}", server.await));
        }
        match TcpStream::connect(listen).await {
            Ok(value) => {
                stream = Some(value);
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
    let mut stream =
        stream.ok_or_else(|| "tcp transport never accepted a connection".to_owned())?;

    let request = HandshakeRequest {
        client_version: ProtocolVersion::V2_0,
        auth_token: None,
        requested_features: all_features(),
    };
    let frame = ClientFrame {
        session: None,
        request: JsonRequest::Handshake(request),
    };
    write_frame(
        &mut stream,
        &encode_json(&frame).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(FrameError::describe)?;
    let payload = read_frame(&mut stream, DecodeLimits::v2_default().max_message_bytes)
        .await
        .map_err(|error| match error {
            FrameError::Closed => "connection closed before the handshake reply".to_owned(),
            other => other.describe(),
        })?;
    let server = decode_json::<ServerFrame>(&payload).map_err(|e| e.to_string())?;
    match server {
        ServerFrame::Result(boxed) => match *boxed {
            JsonResponse::Handshake(response) => {
                assert_eq!(
                    response.enabled_features.len(),
                    5,
                    "all advertised features enabled"
                );
                assert!(response.unknown_requested_features.is_empty());
                assert_eq!(response.session_token, [0xC0; 16]);
            }
            other => return Err(format!("unexpected handshake frame: {other:?}")),
        },
        ServerFrame::Error(error) => {
            return Err(format!("handshake was rejected: {error:?}"));
        }
    }
    Ok(())
}
