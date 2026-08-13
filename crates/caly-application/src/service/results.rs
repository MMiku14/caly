//! Actor result application by the single runtime owner.

use std::time::Duration;

use caly_domain::{EventCursor, OperationId, OperationState, OperationStatus};

use crate::{
    actor_result::{ActorReport, ActorResultReceiver, ResultDeltas},
    operations::{AdmissionController, AdmissionError, TimeSource},
    projection::{ProjectionRuntime, ProjectionRuntimeError},
    runtime::MailboxReceiveError,
    service::runtime_service::RuntimeService,
};

/// One result-mailbox poll outcome.
pub enum ActorResultOutcome {
    Idle,
    MailboxClosed,
    Terminal {
        status: OperationStatus,
        published_through: Option<EventCursor>,
    },
    /// A projection-only observation (e.g. a core crash) with no operation
    /// transition.
    Observed {
        published_through: Option<EventCursor>,
    },
}

/// Fatal result-application failure; runtime state must not continue silently.
#[derive(Debug)]
pub enum ActorResultError {
    InvalidTimeout,
    Admission(AdmissionError),
    Projection(ProjectionRuntimeError),
}

impl core::fmt::Display for ActorResultError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "actor result application failed: {self:?}; begin fatal runtime shutdown"
        )
    }
}

impl std::error::Error for ActorResultError {}

impl<T: TimeSource> RuntimeService<T, ProjectionRuntime> {
    /// W3a BUG-3: self-heal the projection after a poisoning fault by
    /// rebuilding from the projector's own last-consistent snapshot.
    /// Returns `Ok` when the runtime is (or became) usable again.
    pub fn recover_projection(&mut self) -> Result<(), ActorResultError> {
        self.projection_mut()
            .try_recover()
            .map_err(ActorResultError::Projection)
    }

    /// Polls and applies one report through projection and operation owners.
    pub fn apply_actor_result_once(
        &mut self,
        receiver: &ActorResultReceiver,
        timeout: Duration,
    ) -> Result<ActorResultOutcome, ActorResultError> {
        let report = match receiver.receive_timeout(timeout) {
            Ok(value) => value,
            Err(MailboxReceiveError::TimedOut) => return Ok(ActorResultOutcome::Idle),
            Err(MailboxReceiveError::Closed) => return Ok(ActorResultOutcome::MailboxClosed),
            Err(MailboxReceiveError::InvalidTimeout) => {
                return Err(ActorResultError::InvalidTimeout);
            }
        };
        self.apply_actor_report(report)
    }

    /// Applies a report already received by the runtime owner.
    pub fn apply_actor_report(
        &mut self,
        report: ActorReport,
    ) -> Result<ActorResultOutcome, ActorResultError> {
        match report {
            ActorReport::Completed {
                operation_id,
                deltas,
            } => self.apply_terminal(operation_id, deltas, |admission| {
                admission.complete(operation_id)
            }),
            ActorReport::Failed {
                operation_id,
                failure,
                deltas,
            } => self.apply_terminal(operation_id, deltas, |admission| {
                admission.fail(operation_id, failure)
            }),
            ActorReport::Crashed { deltas }
            | ActorReport::Recovered { deltas }
            | ActorReport::Observed { deltas } => {
                // A crash/recovery/telemetry event is an observed platform or
                // runtime event, not a client operation. Project it and emit an
                // event, but do not touch the operation store (no operation
                // exists to transition).
                let cursor = publish_deltas(self.projection_mut(), deltas)?;
                Ok(ActorResultOutcome::Observed {
                    published_through: cursor,
                })
            }
        }
    }

    fn apply_terminal(
        &mut self,
        operation_id: OperationId,
        deltas: ResultDeltas,
        transition: impl FnOnce(&mut AdmissionController<T>) -> Result<OperationStatus, AdmissionError>,
    ) -> Result<ActorResultOutcome, ActorResultError> {
        if let Some(outcome) = self.cancelled_outcome(operation_id)? {
            return Ok(outcome);
        }
        let cursor = publish_deltas(self.projection_mut(), deltas)?;
        let status = transition(self.admission_mut()).map_err(ActorResultError::Admission)?;
        Ok(ActorResultOutcome::Terminal {
            status,
            published_through: cursor,
        })
    }

    fn cancelled_outcome(
        &mut self,
        operation_id: OperationId,
    ) -> Result<Option<ActorResultOutcome>, ActorResultError> {
        let current = self
            .admission_mut()
            .status(operation_id)
            .map_err(ActorResultError::Admission)?;
        Ok(
            (current.state() == OperationState::Cancelled).then_some(
                ActorResultOutcome::Terminal {
                    status: current,
                    published_through: None,
                },
            ),
        )
    }
}

fn publish_deltas(
    projection: &mut ProjectionRuntime,
    deltas: ResultDeltas,
) -> Result<Option<EventCursor>, ActorResultError> {
    let mut cursor = None;
    for delta in deltas.into_vec() {
        cursor = Some(
            projection
                .publish(delta)
                .map_err(ActorResultError::Projection)?,
        );
    }
    Ok(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        actor_result::{actor_result_mailbox, ActorReport},
        command_bus::{Command, CommandEnvelope},
        operations::{AdmissionController, CancelDecision, OperationStore},
    };
    use caly_domain::{
        AppliedState, BoundedVec, CapabilitySet, CoreKind, CoreRunState, DaemonInstanceId,
        DesiredState, EventSequence, ObservedState, OperationId, PlatformEffectView,
        PresentationSnapshot, ProxyMode, SnapshotRevision, UnixMillis,
    };

    struct Clock(u64);
    impl TimeSource for Clock {
        fn now(&mut self) -> UnixMillis {
            self.0 = self.0.saturating_add(1);
            UnixMillis::new(self.0)
        }
    }

    fn snapshot() -> Result<PresentationSnapshot, Box<dyn std::error::Error>> {
        let daemon = DaemonInstanceId::from_bytes([3; 16]);
        let cursor = EventCursor::new(daemon, EventSequence::ZERO);
        let capabilities = CapabilitySet::new(BoundedVec::new())?;
        Ok(PresentationSnapshot::new(
            daemon,
            SnapshotRevision::new(0),
            cursor,
            DesiredState::new(ProxyMode::Rule, None, None, false, false),
            AppliedState::stopped(),
            ObservedState::default(),
            PlatformEffectView::none(),
            capabilities,
            BoundedVec::new(),
        ))
    }

    #[test]
    fn cancelled_operation_suppresses_late_owner_report() -> Result<(), Box<dyn std::error::Error>>
    {
        let (ingress, _receiver) = crate::command_bus::command_bus(4)?;
        let admission = AdmissionController::new(OperationStore::new(4, 2)?, ingress, Clock(0));
        let projection = ProjectionRuntime::new(snapshot()?, 4, 4)?;
        let mut service = RuntimeService::new(admission, projection);
        let id = OperationId::from_bytes([8; 16]);
        service.admission_mut().submit(CommandEnvelope {
            operation_id: id,
            command: Command::SetTun { enabled: true },
        })?;
        service.admission_mut().start(id)?;
        let (decision, status) = service.admission_mut().cancel(id)?;
        assert_eq!(decision, CancelDecision::Cancelled);
        assert_eq!(status.state(), OperationState::Cancelled);

        let deltas =
            ResultDeltas::try_from_vec(vec![caly_domain::PresentationDelta::ObservedReplaced(
                ObservedState::new(9, 9, 9, 0),
            )])?;
        let outcome = service.apply_actor_report(ActorReport::Completed {
            operation_id: id,
            deltas,
        })?;
        assert!(matches!(
            outcome,
            ActorResultOutcome::Terminal {
                ref status,
                published_through: None,
            } if status.state() == OperationState::Cancelled
        ));
        assert_eq!(
            service.projection_mut().current_snapshot()?.observed(),
            &ObservedState::default()
        );
        Ok(())
    }

    #[test]
    fn crashed_report_projects_without_touching_operations(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (ingress, _receiver) = crate::command_bus::command_bus(4)?;
        let admission = AdmissionController::new(OperationStore::new(4, 2)?, ingress, Clock(0));
        let projection = ProjectionRuntime::new(snapshot()?, 4, 4)?;
        let mut service = RuntimeService::new(admission, projection);
        let (result_ingress, result_receiver) = actor_result_mailbox(4)?;

        // No operation exists for a crash; only the projection should change.
        let crashed =
            AppliedState::new(Some(CoreKind::Mihomo), CoreRunState::Crashed, None, Some(1))?;
        let deltas =
            ResultDeltas::try_from_vec(vec![caly_domain::PresentationDelta::AppliedReplaced(
                crashed,
            )])?;
        result_ingress
            .try_send(ActorReport::Crashed { deltas })
            .map_err(|_| "send failed")?;

        let outcome =
            service.apply_actor_result_once(&result_receiver, Duration::from_millis(1))?;
        assert!(matches!(
            outcome,
            ActorResultOutcome::Observed {
                published_through: Some(_)
            }
        ));
        assert_eq!(
            service
                .projection_mut()
                .current_snapshot()?
                .applied()
                .run_state(),
            CoreRunState::Crashed
        );
        Ok(())
    }

    #[test]
    fn observed_report_projects_telemetry_without_operations(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (ingress, _receiver) = crate::command_bus::command_bus(4)?;
        let admission = AdmissionController::new(OperationStore::new(4, 2)?, ingress, Clock(0));
        let projection = ProjectionRuntime::new(snapshot()?, 4, 4)?;
        let mut service = RuntimeService::new(admission, projection);
        let (result_ingress, result_receiver) = actor_result_mailbox(4)?;

        // A telemetry observation updates observed state without an operation.
        let observed = ObservedState::new(12, 34, 5, 0);
        let deltas =
            ResultDeltas::try_from_vec(vec![caly_domain::PresentationDelta::ObservedReplaced(
                observed,
            )])?;
        result_ingress
            .try_send(ActorReport::Observed { deltas })
            .map_err(|_| "send failed")?;

        let outcome =
            service.apply_actor_result_once(&result_receiver, Duration::from_millis(1))?;
        assert!(matches!(
            outcome,
            ActorResultOutcome::Observed {
                published_through: Some(_)
            }
        ));
        assert_eq!(
            service.projection_mut().current_snapshot()?.observed(),
            &observed
        );
        Ok(())
    }
}
