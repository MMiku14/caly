//! Atomic operation reservation and bounded command admission.

use caly_domain::{
    BoundedText, OperationFailure, OperationId, OperationState, OperationStatus,
    OperationStatusError, TextError, UnixMillis,
};

use crate::command_bus::{CommandEnvelope, CommandIngress, CommandIngressError};

use super::{CancelDecision, InsertOutcome, OperationRecord, OperationStore, StoreError};

/// Time source owned by the Application runtime, never by Domain or Transport.
pub trait TimeSource {
    fn now(&mut self) -> UnixMillis;
}

/// Admission failure retains queue and rollback errors separately.
#[derive(Debug)]
pub enum AdmissionError {
    InvalidKind(TextError),
    Store(StoreError),
    Queue(CommandIngressError),
    QueueRollback {
        queue: CommandIngressError,
        rollback: StoreError,
    },
    Status(OperationStatusError),
}

impl core::fmt::Display for AdmissionError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "operation admission failed: {self:?}; query by operation id before retrying"
        )
    }
}

impl std::error::Error for AdmissionError {}

/// Single owner coordinating operation records with command ingress.
pub struct AdmissionController<T> {
    store: OperationStore,
    ingress: CommandIngress,
    clock: T,
}

impl<T: TimeSource> AdmissionController<T> {
    pub const fn new(store: OperationStore, ingress: CommandIngress, clock: T) -> Self {
        Self {
            store,
            ingress,
            clock,
        }
    }

    /// Reserves status before enqueue and rolls it back if enqueue fails.
    pub fn submit(&mut self, envelope: CommandEnvelope) -> Result<OperationStatus, AdmissionError> {
        if let Some(existing) = self.store.get(envelope.operation_id) {
            if existing.command() != &envelope.command {
                return Err(AdmissionError::Store(StoreError::IdempotencyConflict));
            }
            return existing.status().map_err(AdmissionError::Status);
        }
        let kind =
            BoundedText::new(envelope.command.kind()).map_err(AdmissionError::InvalidKind)?;
        let record = OperationRecord::pending(
            envelope.operation_id,
            // Clone keeps `envelope` whole for the ingress enqueue below
            // (a `Command` clone is cheap: `Arc` for W4 names, `Copy`
            // payloads elsewhere).
            envelope.command.clone(),
            kind,
            self.clock.now(),
        );
        let inserted = self.store.insert(record).map_err(AdmissionError::Store)?;
        if inserted == InsertOutcome::Existing {
            return self.status(envelope.operation_id);
        }
        let operation_id = envelope.operation_id;
        if let Err(queue) = self.ingress.try_submit(envelope) {
            return match self.store.remove_pending(operation_id) {
                Ok(_) => Err(AdmissionError::Queue(queue)),
                Err(rollback) => Err(AdmissionError::QueueRollback { queue, rollback }),
            };
        }
        self.status(operation_id)
    }

    /// Transitions an admitted command to Running when its owner receives it.
    pub fn start(&mut self, id: OperationId) -> Result<OperationStatus, AdmissionError> {
        self.transition(id, OperationState::Running, None)
    }

    /// Marks the irreversible coordinator commit point.
    pub fn mark_committed(&mut self, id: OperationId) -> Result<(), AdmissionError> {
        self.store.mark_committed(id).map_err(AdmissionError::Store)
    }

    /// Completes an operation successfully.
    pub fn complete(&mut self, id: OperationId) -> Result<OperationStatus, AdmissionError> {
        self.transition(id, OperationState::Completed, None)
    }

    /// Completes an operation with structured failure.
    pub fn fail(
        &mut self,
        id: OperationId,
        failure: OperationFailure,
    ) -> Result<OperationStatus, AdmissionError> {
        self.transition(id, OperationState::Failed, Some(failure))
    }

    /// Handles an explicit cancellation request.
    pub fn cancel(
        &mut self,
        id: OperationId,
    ) -> Result<(CancelDecision, OperationStatus), AdmissionError> {
        let decision = self
            .store
            .cancel(id, self.clock.now())
            .map_err(AdmissionError::Store)?;
        Ok((decision, self.status(id)?))
    }

    /// Returns authoritative query status.
    pub fn status(&self, id: OperationId) -> Result<OperationStatus, AdmissionError> {
        self.store
            .get(id)
            .ok_or(AdmissionError::Store(StoreError::NotFound))?
            .status()
            .map_err(AdmissionError::Status)
    }

    pub fn cancellation_token(
        &self,
        id: OperationId,
    ) -> Result<super::OperationCancellationToken, AdmissionError> {
        self.store
            .cancellation_token(id)
            .map_err(AdmissionError::Store)
    }

    fn transition(
        &mut self,
        id: OperationId,
        state: OperationState,
        failure: Option<OperationFailure>,
    ) -> Result<OperationStatus, AdmissionError> {
        self.store
            .transition(id, state, self.clock.now(), failure)
            .map_err(AdmissionError::Store)?;
        self.status(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_bus::{Command, command_bus};

    struct Clock(u64);
    impl TimeSource for Clock {
        fn now(&mut self) -> UnixMillis {
            self.0 = self.0.saturating_add(1);
            UnixMillis::new(self.0)
        }
    }

    fn envelope(byte: u8) -> CommandEnvelope {
        CommandEnvelope {
            operation_id: OperationId::from_bytes([byte; 16]),
            command: Command::ApplyConfig {
                candidate_id: [byte; 16],
            },
        }
    }

    fn lifecycle_envelope(byte: u8) -> CommandEnvelope {
        CommandEnvelope {
            operation_id: OperationId::from_bytes([byte; 16]),
            command: Command::SwitchCore {
                target: caly_domain::CoreKind::Mihomo,
                action: crate::command_bus::CoreAction::Restart,
            },
        }
    }

    #[test]
    fn queue_full_rolls_back_pending_record() -> Result<(), Box<dyn std::error::Error>> {
        let (ingress, _receiver) = command_bus(1)?;
        let store = OperationStore::new(4, 2)?;
        let mut admission = AdmissionController::new(store, ingress, Clock(0));
        admission.submit(envelope(1))?;
        assert!(matches!(
            admission.submit(envelope(2)),
            Err(AdmissionError::Queue(_))
        ));
        assert!(matches!(
            admission.status(OperationId::from_bytes([2; 16])),
            Err(AdmissionError::Store(StoreError::NotFound))
        ));
        Ok(())
    }

    #[test]
    fn running_cancel_is_too_late_and_terminal_result_stays_legal()
    -> Result<(), Box<dyn std::error::Error>> {
        let (ingress, _receiver) = command_bus(2)?;
        let store = OperationStore::new(4, 2)?;
        let mut admission = AdmissionController::new(store, ingress, Clock(0));
        let id = OperationId::from_bytes([7; 16]);
        admission.submit(lifecycle_envelope(7))?;
        admission.start(id)?;
        let (decision, running) = admission.cancel(id)?;
        assert_eq!(decision, CancelDecision::TooLateToCancel);
        assert_eq!(running.state(), OperationState::Running);
        let completed = admission.complete(id)?;
        assert_eq!(completed.state(), OperationState::Completed);
        let (decision, terminal) = admission.cancel(id)?;
        assert_eq!(decision, CancelDecision::AlreadyTerminal);
        assert_eq!(terminal.state(), OperationState::Completed);
        Ok(())
    }

    #[test]
    fn duplicate_id_with_different_command_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let (ingress, _receiver) = command_bus(2)?;
        let store = OperationStore::new(4, 2)?;
        let mut admission = AdmissionController::new(store, ingress, Clock(0));
        admission.submit(envelope(1))?;
        let conflicting = CommandEnvelope {
            operation_id: OperationId::from_bytes([1; 16]),
            command: Command::SetTun { enabled: true },
        };
        assert!(matches!(
            admission.submit(conflicting),
            Err(AdmissionError::Store(StoreError::IdempotencyConflict))
        ));
        Ok(())
    }
}
