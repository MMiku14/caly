//! v2 transport adapter over the Application service port.

use caly_application::{
    operations::CancelDecision,
    service::{ApplicationServicePort, ApplicationWatch},
};
use caly_protocol::{
    conversion::{cursor_from_wire, operation_status_to_wire, snapshot_to_wire},
    protocol::v2::{
        CancelOperationRequest, CancelOperationResponse, DecodeLimits, ExecuteRequest,
        ExecuteResponse, FeatureList, GetOperationStatusRequest, HandshakeRequest,
        HandshakeResponse, ProtocolVersion, RawCancelOutcome, WatchEventsRequest, WatchResponse,
        WireId, WireOperationStatus, WirePresentationSnapshot, negotiate_handshake,
    },
};

use super::{
    CommandDecodeError, ServiceError, ServiceV2, command_from_wire, event_to_wire, service_error,
};

/// Stateless Transport mapping around one Application owner.
pub struct ServiceAdapter<A> {
    application: A,
    daemon_instance: WireId,
    session_token: WireId,
    admission: crate::admission::TokenAdmission,
    supported_features: FeatureList,
    limits: DecodeLimits,
    handshake_complete: bool,
}

impl<A> ServiceAdapter<A> {
    /// Builds the adapter. `admission` gates the handshake: the
    /// UDS transport constructs
    /// [`crate::admission::TokenAdmission::Open`], the TCP
    /// transport passes the configured `daemon.auth_token` so the
    /// control plane is never served unauthenticated to the
    /// network (#58).
    pub const fn new(
        application: A,
        daemon_instance: WireId,
        session_token: WireId,
        admission: crate::admission::TokenAdmission,
        supported_features: FeatureList,
        limits: DecodeLimits,
    ) -> Self {
        Self {
            application,
            daemon_instance,
            session_token,
            admission,
            supported_features,
            limits,
            handshake_complete: false,
        }
    }

    fn require_handshake(&self) -> Result<(), ServiceError> {
        if self.handshake_complete {
            Ok(())
        } else {
            Err(ServiceError::HandshakeRequired)
        }
    }

    /// Returns the negotiated decode limits (used for transport ceilings).
    pub const fn limits(&self) -> DecodeLimits {
        self.limits
    }

    /// Subscribes to live projection events for a continuous watch stream,
    /// after requiring a completed handshake.
    pub fn subscribe_live(
        &self,
    ) -> Result<
        tokio::sync::broadcast::Receiver<caly_application::events::SequencedEvent>,
        ServiceError,
    >
    where
        A: ApplicationServicePort,
    {
        self.require_handshake()?;
        Ok(self.application.subscribe_live())
    }
}

impl<A> ServiceV2 for ServiceAdapter<A>
where
    A: ApplicationServicePort,
{
    type Watch = std::vec::IntoIter<Result<WatchResponse, ServiceError>>;

    fn handshake(&mut self, request: HandshakeRequest) -> Result<HandshakeResponse, ServiceError> {
        // Admission first: a token-enforcing daemon refuses the
        // session before version/feature negotiation leaks any
        // further detail.
        if !self.admission.admit(request.auth_token.as_deref()) {
            return Err(ServiceError::Unauthenticated);
        }
        // Idempotent: each client process opens a fresh connection and
        // handshakes. The daemon session token is a constant, so a repeated
        // handshake re-negotiates version/features and returns the same token.
        let response = negotiate_handshake(
            request,
            ProtocolVersion::V2_0,
            &self.supported_features,
            self.daemon_instance,
            self.session_token,
            self.limits,
        )
        .map_err(|_| ServiceError::IncompatibleVersion)?;
        self.handshake_complete = true;
        Ok(response)
    }

    fn execute(&mut self, request: ExecuteRequest) -> Result<ExecuteResponse, ServiceError> {
        self.require_handshake()?;
        let envelope = command_from_wire(request).map_err(command_error)?;
        let status = self.application.submit(envelope).map_err(service_error)?;
        Ok(ExecuteResponse {
            operation: operation_status_to_wire(&status),
        })
    }

    fn cancel(
        &mut self,
        request: CancelOperationRequest,
    ) -> Result<CancelOperationResponse, ServiceError> {
        self.require_handshake()?;
        let id = caly_domain::OperationId::from_bytes(request.operation_id);
        let result = self.application.cancel(id).map_err(service_error)?;
        let outcome = match result.decision {
            CancelDecision::Cancelled => 1,
            CancelDecision::AlreadyTerminal => 2,
            CancelDecision::TooLateToCancel => 3,
        };
        Ok(CancelOperationResponse {
            operation: operation_status_to_wire(&result.status),
            outcome: RawCancelOutcome(outcome),
        })
    }

    fn status(
        &mut self,
        request: GetOperationStatusRequest,
    ) -> Result<WireOperationStatus, ServiceError> {
        self.require_handshake()?;
        let id = caly_domain::OperationId::from_bytes(request.operation_id);
        self.application
            .operation_status(id)
            .map(|status| operation_status_to_wire(&status))
            .map_err(service_error)
    }

    fn snapshot(&mut self) -> Result<WirePresentationSnapshot, ServiceError> {
        self.require_handshake()?;
        let snapshot = self.application.snapshot().map_err(service_error)?;
        snapshot_to_wire(&snapshot).map_err(|_| ServiceError::ApplicationUnavailable)
    }

    fn watch(&mut self, request: WatchEventsRequest) -> Result<Self::Watch, ServiceError> {
        self.require_handshake()?;
        let cursor = request.after.map(cursor_from_wire);
        let response = self
            .application
            .watch_after(cursor)
            .map_err(service_error)?;
        let mut messages = Vec::new();
        match response {
            ApplicationWatch::Replay(events) => {
                for event in &events {
                    let wire =
                        event_to_wire(event).map_err(|_| ServiceError::ApplicationUnavailable)?;
                    messages.push(Ok(WatchResponse::Event(wire)));
                }
            }
            ApplicationWatch::FullSnapshot(snapshot) => {
                let wire = snapshot_to_wire(&snapshot)
                    .map_err(|_| ServiceError::ApplicationUnavailable)?;
                messages.push(Ok(WatchResponse::FullSnapshot(Box::new(wire))));
            }
        }
        Ok(messages.into_iter())
    }
}

fn command_error(value: CommandDecodeError) -> ServiceError {
    ServiceError::InvalidArgument {
        reason: format!(
            "command decode rejected: {value:?}; upgrade the client or correct the command"
        ),
    }
}
