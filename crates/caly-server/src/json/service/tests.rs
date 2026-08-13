//! Tests for `json/service.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

use super::*;

use caly_application::{
    command_bus::CommandEnvelope,
    events::SequencedEvent,
    service::{ApplicationServiceError, ApplicationWatch, CancellationResult},
};
use caly_domain::UnixMillis;
use caly_domain::{EventCursor, OperationId, OperationStatus, PresentationSnapshot};
use caly_protocol::protocol::v2::{DecodeLimits, FeatureList, ProtocolVersion};

/// Session-only stub; the validated paths never touch the application.
struct StubApp;

impl ApplicationServicePort for StubApp {
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

fn session(token: [u8; 16], expires: u64) -> ProtocolSession {
    ProtocolSession {
        token,
        version: ProtocolVersion::V2_0,
        features: FeatureList::new(),
        expires_at: UnixMillis::new(expires),
    }
}

fn service_with_registry(capacity: usize) -> Result<JsonService<StubApp>, SessionError> {
    let adapter = ServiceAdapter::new(
        StubApp,
        [0; 16],
        [0; 16],
        crate::admission::TokenAdmission::Open,
        FeatureList::new(),
        DecodeLimits::v2_default(),
    );
    Ok(JsonService {
        state: Arc::new(Mutex::new(ServiceState {
            adapter,
            // Round 17: tests don't run a transport, so
            // the stop signal is unused.
            stop_notifier: None,
            registry: SessionRegistry::new(capacity)?,
        })),
    })
}

#[test]
fn validate_session_accepts_live_token_and_rejects_unknown() -> Result<(), SessionError> {
    let service = service_with_registry(4)?;
    service
        .state
        .lock()
        .map_err(|_| SessionError::InvalidToken)?
        .registry
        .insert(session([1; 16], u64::MAX))?;
    let accepted = service.validate_session(Some(caly_domain::to_hex([1; 16])));
    assert!(accepted.is_ok());
    assert!(
        service
            .validate_session(Some(caly_domain::to_hex([2; 16])))
            .is_err()
    );
    Ok(())
}

#[test]
fn validate_session_rejects_missing_token() {
    let service = service_with_registry(4).unwrap();
    assert!(service.validate_session(None).is_err());
}

#[test]
fn validate_session_rejects_expired_token() -> Result<(), SessionError> {
    let service = service_with_registry(4)?;
    service
        .state
        .lock()
        .map_err(|_| SessionError::InvalidToken)?
        .registry
        .insert(session([3; 16], 5))?; // expires at t=5
    // Wall clock is past 5ms since Unix epoch; the token must be expired.
    assert!(
        service
            .validate_session(Some(caly_domain::to_hex([3; 16])))
            .is_err()
    );
    Ok(())
}
