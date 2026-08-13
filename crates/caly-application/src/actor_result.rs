//! Bounded actor/coordinator result messages.

use caly_domain::{BoundedVec, OperationFailure, OperationId, PresentationDelta};

use crate::runtime::{ActorIngress, ActorReceiver, InvalidMailboxCapacity, actor_mailbox};

/// Maximum projection slice replacements attached to one terminal report.
pub const MAX_RESULT_DELTAS: usize = 16;
pub type ResultDeltas = BoundedVec<PresentationDelta, MAX_RESULT_DELTAS>;

/// Report applied only by RuntimeService/AdmissionController owner.
#[derive(Debug)]
pub enum ActorReport {
    Completed {
        operation_id: OperationId,
        deltas: ResultDeltas,
    },
    Failed {
        operation_id: OperationId,
        failure: OperationFailure,
        deltas: ResultDeltas,
    },
    /// A core process exited unexpectedly. This is an observation, not a
    /// client mutation, so it only publishes projection deltas and never
    /// transitions an operation.
    Crashed { deltas: ResultDeltas },
    /// A crashed core was automatically recovered (restarted) by the supervisor.
    /// Like `Crashed`, this is an observation with no associated operation.
    Recovered { deltas: ResultDeltas },
    /// A periodic telemetry observation (e.g. refreshed `ObservedState`). Like
    /// `Crashed`/`Recovered`, it publishes projection deltas only and never
    /// transitions an operation.
    Observed { deltas: ResultDeltas },
}

impl ActorReport {
    pub const fn operation_id(&self) -> Option<OperationId> {
        match self {
            Self::Completed { operation_id, .. } | Self::Failed { operation_id, .. } => {
                Some(*operation_id)
            }
            Self::Crashed { .. } | Self::Recovered { .. } | Self::Observed { .. } => None,
        }
    }
}

pub type ActorResultIngress = ActorIngress<ActorReport>;
pub type ActorResultReceiver = ActorReceiver<ActorReport>;

/// Ownership-preserving terminal report failure.
///
/// The unconsumed [`ActorReport`] is preserved on every variant so the
/// caller can decide what to do with it (retry, requeue, log, ...). The
/// previous form silently dropped the report on the `Result<(), ...>` path,
/// which left the operation in a Started-but-not-Failed state: every owner
/// handler propagated `HandlerReportError` upward and the dispatcher
/// surfaced a fatal `TokioTaskFailure::Task`, taking the whole daemon
/// down for what was originally a transient backpressure. Callers that
/// drop the report here are now explicit about it (the warning at the
/// call site, plus the diagnostic the caller can re-derive from the
/// `operation_id` field on the unconsumed `ActorReport`).
#[derive(Debug)]
pub enum ActorResultSendError {
    Full(ActorReport),
    Closed(ActorReport),
}

/// Restricted producer that can only submit terminal reports.
#[derive(Clone)]
pub struct ActorResultClient(ActorResultIngress);

impl ActorResultClient {
    pub const fn new(ingress: ActorResultIngress) -> Self {
        Self(ingress)
    }

    pub fn try_report(&self, report: ActorReport) -> Result<(), ActorResultSendError> {
        self.0.try_send(report).map_err(|error| match error {
            crate::runtime::MailboxSendError::Full(value) => ActorResultSendError::Full(value),
            crate::runtime::MailboxSendError::Closed(value) => ActorResultSendError::Closed(value),
        })
    }
}

/// Creates the sole bounded result path back to the runtime owner.
pub fn actor_result_mailbox(
    capacity: usize,
) -> Result<(ActorResultIngress, ActorResultReceiver), InvalidMailboxCapacity> {
    actor_mailbox(capacity)
}
