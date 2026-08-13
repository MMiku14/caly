//! Daemon-side v2 transport service contract.

use caly_protocol::protocol::v2::{
    CancelOperationRequest, CancelOperationResponse, ExecuteRequest, ExecuteResponse,
    GetOperationStatusRequest, HandshakeRequest, HandshakeResponse, WatchEventsRequest,
    WatchResponse, WireOperationStatus, WirePresentationSnapshot,
};

/// Transport status with a stable remediation category.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceError {
    Unauthenticated,
    PermissionDenied,
    ResourceExhausted,
    InvalidArgument { reason: String },
    IncompatibleVersion,
    HandshakeRequired,
    ApplicationUnavailable,
}

/// Application-facing v2 service adapter.
///
/// Implementations must reject mutation overload rather than queueing without
/// bounds. Client wait cancellation must not be translated into operation
/// cancellation.
pub trait ServiceV2 {
    type Watch: Iterator<Item = Result<WatchResponse, ServiceError>>;

    fn handshake(&mut self, request: HandshakeRequest) -> Result<HandshakeResponse, ServiceError>;
    fn execute(&mut self, request: ExecuteRequest) -> Result<ExecuteResponse, ServiceError>;
    fn cancel(
        &mut self,
        request: CancelOperationRequest,
    ) -> Result<CancelOperationResponse, ServiceError>;
    fn status(
        &mut self,
        request: GetOperationStatusRequest,
    ) -> Result<WireOperationStatus, ServiceError>;
    fn snapshot(&mut self) -> Result<WirePresentationSnapshot, ServiceError>;
    fn watch(&mut self, request: WatchEventsRequest) -> Result<Self::Watch, ServiceError>;
}
