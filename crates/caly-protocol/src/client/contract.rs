//! Runtime-neutral thin-client contract.

use crate::protocol::v2::{
    CancelOperationRequest, CancelOperationResponse, ExecuteRequest, ExecuteResponse,
    GetOperationStatusRequest, HandshakeRequest, HandshakeResponse, WatchEventsRequest,
    WatchResponse, WireOperationStatus, WirePresentationSnapshot,
};

/// Errors exposed by a concrete protocol client implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientError {
    TransportUnavailable,
    DeadlineExceeded,
    IncompatibleVersion,
    AuthenticationRejected,
    ResourceExhausted,
    DecodeRejected {
        reason: String,
        suggested_action: String,
    },
}

impl core::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TransportUnavailable => write!(formatter, "transport unavailable"),
            Self::DeadlineExceeded => write!(formatter, "operation deadline exceeded"),
            Self::IncompatibleVersion => write!(formatter, "incompatible protocol version"),
            Self::AuthenticationRejected => write!(formatter, "authentication rejected"),
            Self::ResourceExhausted => write!(formatter, "daemon resource exhausted"),
            Self::DecodeRejected { reason, .. } => write!(formatter, "decode rejected: {reason}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Transport-neutral request surface implemented later by local/remote clients.
///
/// This trait intentionally contains no async-runtime type in its signature.
pub trait ClientContract {
    /// Owned bounded/recoverable watch stream selected by an implementation.
    type Watch: Iterator<Item = Result<WatchResponse, ClientError>>;

    fn handshake(&mut self, request: HandshakeRequest) -> Result<HandshakeResponse, ClientError>;
    fn execute(&mut self, request: ExecuteRequest) -> Result<ExecuteResponse, ClientError>;
    fn cancel(
        &mut self,
        request: CancelOperationRequest,
    ) -> Result<CancelOperationResponse, ClientError>;
    fn operation_status(
        &mut self,
        request: GetOperationStatusRequest,
    ) -> Result<WireOperationStatus, ClientError>;
    fn snapshot(&mut self) -> Result<WirePresentationSnapshot, ClientError>;
    fn watch(&mut self, request: WatchEventsRequest) -> Result<Self::Watch, ClientError>;
}
