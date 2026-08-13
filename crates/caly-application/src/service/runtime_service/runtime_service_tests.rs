use super::*;
use crate::{
    command_bus::{Command, command_bus},
    events::SequencedEvent,
    operations::{CancelDecision, OperationStore},
    routing::{CommandSink, RouteDispatchError, RoutedCommand},
};
use caly_domain::{OperationId, OperationState, UnixMillis};

struct Clock(u64);
impl TimeSource for Clock {
    fn now(&mut self) -> UnixMillis {
        self.0 = self.0.saturating_add(1);
        UnixMillis::new(self.0)
    }
}

struct FullSink;
impl CommandSink for FullSink {
    fn try_dispatch(&mut self, command: RoutedCommand) -> Result<(), RouteDispatchError> {
        Err(RouteDispatchError::ResourceExhausted(command))
    }
}

#[derive(Default)]
struct CaptureSink(Option<RoutedCommand>);
impl CommandSink for CaptureSink {
    fn try_dispatch(&mut self, command: RoutedCommand) -> Result<(), RouteDispatchError> {
        self.0 = Some(command);
        Ok(())
    }
}

struct TestProjection;
impl ProjectionService for TestProjection {
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
        let (tx, rx) = tokio::sync::broadcast::channel(1);
        drop(tx);
        rx
    }
}

#[test]
fn unsupported_command_is_rejected_before_operation_reservation()
-> Result<(), Box<dyn std::error::Error>> {
    let (ingress, receiver) = command_bus(2)?;
    let admission = AdmissionController::new(OperationStore::new(4, 2)?, ingress, Clock(0));
    let mut service = RuntimeService::new_with_policy(
        admission,
        TestProjection,
        CommandSupportPolicy::lifecycle_only(caly_domain::CoreKind::Mihomo),
    );
    let id = OperationId::from_bytes([9; 16]);
    let result = service.submit(CommandEnvelope {
        operation_id: id,
        command: Command::SetTun { enabled: true },
    });
    assert!(matches!(
        result,
        Err(ApplicationServiceError::InvalidCommand(_))
    ));
    assert!(matches!(
        service.operation_status(id),
        Err(ApplicationServiceError::OperationNotFound)
    ));
    assert_eq!(
        receiver.receive_timeout(Duration::from_millis(1)),
        Err(CommandReceiveError::TimedOut)
    );
    Ok(())
}

#[test]
fn dispatcher_token_observes_running_cancellation() -> Result<(), Box<dyn std::error::Error>> {
    let (ingress, receiver) = command_bus(2)?;
    let admission = AdmissionController::new(OperationStore::new(4, 2)?, ingress, Clock(0));
    let mut service = RuntimeService::new(admission, TestProjection);
    let id = OperationId::from_bytes([6; 16]);
    service.submit(CommandEnvelope {
        operation_id: id,
        command: Command::SetTun { enabled: true },
    })?;
    let expected = service.admission_mut().cancellation_token(id)?;
    let mut sink = CaptureSink::default();
    let outcome = service.dispatch_once(&receiver, &mut sink, Duration::from_millis(1))?;
    assert!(matches!(outcome, DispatchOutcome::Dispatched(_)));
    let routed = sink.0.ok_or("routed command missing")?;
    assert_eq!(routed.cancellation, expected);
    assert!(!routed.cancellation.is_cancel_requested());
    let cancelled = service.cancel(id)?;
    assert_eq!(cancelled.decision, CancelDecision::Cancelled);
    assert_eq!(cancelled.status.state(), OperationState::Cancelled);
    assert!(routed.cancellation.is_cancel_requested());
    Ok(())
}

#[test]
fn pending_cancel_consumes_envelope_without_dispatch() -> Result<(), Box<dyn std::error::Error>> {
    let (ingress, receiver) = command_bus(2)?;
    let admission = AdmissionController::new(OperationStore::new(4, 2)?, ingress, Clock(0));
    let mut service = RuntimeService::new(admission, TestProjection);
    let id = OperationId::from_bytes([8; 16]);
    service.submit(CommandEnvelope {
        operation_id: id,
        command: Command::SetTun { enabled: true },
    })?;
    let cancelled = service.cancel(id)?;
    assert_eq!(cancelled.decision, CancelDecision::Cancelled);
    assert_eq!(cancelled.status.state(), OperationState::Cancelled);

    let outcome = service.dispatch_once(&receiver, &mut FullSink, Duration::from_millis(1))?;
    assert!(matches!(
        outcome,
        DispatchOutcome::CancelledBeforeDispatch(ref status)
            if status.state() == OperationState::Cancelled
    ));
    assert_eq!(
        service.operation_status(id)?.state(),
        OperationState::Cancelled
    );
    Ok(())
}

#[test]
fn target_backpressure_finishes_operation_as_failed() -> Result<(), Box<dyn std::error::Error>> {
    let (ingress, receiver) = command_bus(2)?;
    let admission = AdmissionController::new(OperationStore::new(4, 2)?, ingress, Clock(0));
    let mut service = RuntimeService::new(admission, ());
    let id = OperationId::from_bytes([1; 16]);
    service.admission_mut().submit(CommandEnvelope {
        operation_id: id,
        command: Command::SetTun { enabled: true },
    })?;
    let outcome = service.dispatch_once(&receiver, &mut FullSink, Duration::from_millis(1))?;
    assert!(matches!(outcome, DispatchOutcome::Rejected {
            ref status,
            reason: DispatchRejectReason::ResourceExhausted,
        } if status.state() == OperationState::Failed));
    Ok(())
}
