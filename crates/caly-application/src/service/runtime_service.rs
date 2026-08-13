//! Concrete single-owner Application service facade.

use std::time::Duration;

use caly_domain::{
    EventCursor, OperationFailure, OperationFailureCode, OperationId, OperationState,
    OperationStatus, PresentationSnapshot,
};

use crate::{
    command_bus::{CommandEnvelope, CommandReceiveError, CommandReceiver},
    events::SequencedEvent,
    operations::{AdmissionController, AdmissionError, StoreError, TimeSource},
    routing::{CommandSink, RouteDispatchError, route},
    service::{
        ApplicationServiceError, ApplicationServicePort, ApplicationWatch, CancellationResult,
        CommandSupportPolicy,
    },
};

/// Projection/replay reader owned by the runtime thread.
pub trait ProjectionService {
    fn snapshot(&self) -> Result<PresentationSnapshot, ApplicationServiceError>;
    fn watch_after(
        &self,
        cursor: Option<EventCursor>,
    ) -> Result<ApplicationWatch, ApplicationServiceError>;
    /// Subscribes to live projection events after the current position.
    fn subscribe_live(&self) -> tokio::sync::broadcast::Receiver<SequencedEvent>;
}

/// Serial facade joining atomic mutation admission with read projection.
pub struct RuntimeService<T, P> {
    admission: AdmissionController<T>,
    projection: P,
    command_support: CommandSupportPolicy,
}

impl<T, P> RuntimeService<T, P> {
    /// Creates a service with all commands enabled. This constructor is kept
    /// for isolated tests/embedders that supply every owner; daemon composition
    /// must use `new_with_policy`.
    pub const fn new(admission: AdmissionController<T>, projection: P) -> Self {
        Self::new_with_policy(admission, projection, CommandSupportPolicy::All)
    }

    /// Creates a service with an explicit admission-time command surface.
    pub const fn new_with_policy(
        admission: AdmissionController<T>,
        projection: P,
        command_support: CommandSupportPolicy,
    ) -> Self {
        Self {
            admission,
            projection,
            command_support,
        }
    }

    /// Executor/actor owner uses this path for legal operation transitions.
    pub const fn admission_mut(&mut self) -> &mut AdmissionController<T> {
        &mut self.admission
    }

    pub const fn projection_mut(&mut self) -> &mut P {
        &mut self.projection
    }
}

/// One bounded command-dispatch cycle result.
pub enum DispatchOutcome {
    Idle,
    IngressClosed,
    /// The operation was cancelled while Pending; its queued envelope was
    /// consumed without reaching an owner actor.
    CancelledBeforeDispatch(OperationStatus),
    Dispatched(OperationStatus),
    Rejected {
        status: OperationStatus,
        reason: DispatchRejectReason,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchRejectReason {
    ResourceExhausted,
    TargetClosed,
}

#[derive(Debug)]
pub enum RuntimeDispatchError {
    InvalidTimeout,
    Admission(AdmissionError),
}

impl core::fmt::Display for RuntimeDispatchError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "runtime dispatch failed: {self:?}; stop the runtime and inspect operation state"
        )
    }
}

impl std::error::Error for RuntimeDispatchError {}

impl<T: TimeSource, P> RuntimeService<T, P> {
    /// Starts an admitted operation, then routes it to one bounded owner mailbox.
    pub fn dispatch_once(
        &mut self,
        receiver: &CommandReceiver,
        sink: &mut impl CommandSink,
        timeout: Duration,
    ) -> Result<DispatchOutcome, RuntimeDispatchError> {
        let envelope = match receiver.receive_timeout(timeout) {
            Ok(value) => value,
            Err(CommandReceiveError::TimedOut) => return Ok(DispatchOutcome::Idle),
            Err(CommandReceiveError::Closed) => return Ok(DispatchOutcome::IngressClosed),
            Err(CommandReceiveError::InvalidTimeout) => {
                return Err(RuntimeDispatchError::InvalidTimeout);
            }
        };
        let current = self
            .admission
            .status(envelope.operation_id)
            .map_err(RuntimeDispatchError::Admission)?;
        if current.state() == OperationState::Cancelled {
            // Cancellation won the single-owner race while this envelope was
            // still queued. Consume it without starting or dispatching work.
            return Ok(DispatchOutcome::CancelledBeforeDispatch(current));
        }
        let operation_cancellation = self
            .admission
            .cancellation_token(envelope.operation_id)
            .map_err(RuntimeDispatchError::Admission)?;
        let running = self
            .admission
            .start(envelope.operation_id)
            .map_err(RuntimeDispatchError::Admission)?;
        let operation_id = envelope.operation_id;
        match sink.try_dispatch(route(envelope, operation_cancellation)) {
            Ok(()) => Ok(DispatchOutcome::Dispatched(running)),
            Err(error) => self.reject_dispatch(operation_id, error),
        }
    }

    fn reject_dispatch(
        &mut self,
        operation_id: OperationId,
        error: RouteDispatchError,
    ) -> Result<DispatchOutcome, RuntimeDispatchError> {
        let (reason, failure) = match error {
            RouteDispatchError::ResourceExhausted(_) => (
                DispatchRejectReason::ResourceExhausted,
                dispatch_failure(
                    OperationFailureCode::ResourceExhausted,
                    "target mailbox is full",
                    "query this operation, then retry later with a new operation id",
                ),
            ),
            RouteDispatchError::TargetClosed(_) => (
                DispatchRejectReason::TargetClosed,
                dispatch_failure(
                    OperationFailureCode::Infrastructure,
                    "target actor is closed",
                    "restart or reconnect to the daemon",
                ),
            ),
        };
        let status = self
            .admission
            .fail(operation_id, failure)
            .map_err(RuntimeDispatchError::Admission)?;
        Ok(DispatchOutcome::Rejected { status, reason })
    }
}

fn dispatch_failure(
    code: OperationFailureCode,
    message: &'static str,
    action: &'static str,
) -> OperationFailure {
    // The two inputs are static literals well within the 1 KiB / 512-byte
    // capacities, so the bounded constructor cannot fail for the well-formed
    // call sites. The previous `BoundedText::new(...).map_err(...)?` form
    // was a daemon-fatal path (a `?` early return inside `reject_dispatch`
    // would leave the operation in a Started-but-not-Failed state until
    // the next cancel; `dispatch_step` records the error as a fatal and
    // tears the whole dispatcher down). `OperationFailure::clamped` keeps
    // the same behaviour for the well-formed call sites and surfaces a
    // stable `"_"` fallback for any future refactor that accidentally
    // widens the input.
    OperationFailure::clamped(code, message, action)
}

impl<T, P> ApplicationServicePort for RuntimeService<T, P>
where
    T: TimeSource,
    P: ProjectionService,
{
    fn submit(
        &mut self,
        envelope: CommandEnvelope,
    ) -> Result<caly_domain::OperationStatus, ApplicationServiceError> {
        // Reject contract-only commands before reserving an Operation or
        // enqueueing work. A rejected command therefore cannot become a
        // permanently Running operation or reach an incompatible actor.
        self.command_support.validate(envelope.command.clone())?;
        self.admission.submit(envelope).map_err(map_admission)
    }

    fn cancel(
        &mut self,
        operation_id: OperationId,
    ) -> Result<CancellationResult, ApplicationServiceError> {
        let (decision, status) = self.admission.cancel(operation_id).map_err(map_admission)?;
        Ok(CancellationResult { decision, status })
    }

    fn operation_status(
        &self,
        operation_id: OperationId,
    ) -> Result<caly_domain::OperationStatus, ApplicationServiceError> {
        self.admission.status(operation_id).map_err(map_admission)
    }

    fn snapshot(&self) -> Result<PresentationSnapshot, ApplicationServiceError> {
        self.projection.snapshot()
    }

    fn watch_after(
        &self,
        cursor: Option<EventCursor>,
    ) -> Result<ApplicationWatch, ApplicationServiceError> {
        self.projection.watch_after(cursor)
    }

    fn subscribe_live(&self) -> tokio::sync::broadcast::Receiver<SequencedEvent> {
        self.projection.subscribe_live()
    }
}

fn map_admission(error: AdmissionError) -> ApplicationServiceError {
    match error {
        AdmissionError::Store(StoreError::ResourceExhausted) | AdmissionError::Queue(_) => {
            ApplicationServiceError::ResourceExhausted
        }
        AdmissionError::Store(StoreError::NotFound) => ApplicationServiceError::OperationNotFound,
        AdmissionError::Store(StoreError::IdempotencyConflict) => {
            ApplicationServiceError::IdempotencyConflict
        }
        AdmissionError::QueueRollback { .. }
        | AdmissionError::Store(_)
        | AdmissionError::InvalidKind(_)
        | AdmissionError::Status(_) => ApplicationServiceError::InternalInvariant,
    }
}

#[cfg(test)]
mod runtime_service_tests;
