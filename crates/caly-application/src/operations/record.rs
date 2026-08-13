//! Internal operation record and legal transitions.

use caly_domain::{
    BoundedText, OperationFailure, OperationId, OperationState, OperationStatus,
    OperationStatusError, UnixMillis,
};

use crate::command_bus::Command;

use super::{CommitSignal, OperationCancellationToken};

/// Internal operation record owned only by OperationStore.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationRecord {
    id: OperationId,
    command: Command,
    kind: BoundedText<64>,
    created_at: UnixMillis,
    updated_at: UnixMillis,
    state: OperationState,
    failure: Option<OperationFailure>,
    commit_reached: bool,
    cancellation: OperationCancellationToken,
}

impl OperationRecord {
    /// Creates a pending operation.
    pub fn pending(
        id: OperationId,
        command: Command,
        kind: BoundedText<64>,
        now: UnixMillis,
    ) -> Self {
        Self {
            id,
            command,
            kind,
            created_at: now,
            updated_at: now,
            state: OperationState::Pending,
            failure: None,
            commit_reached: false,
            cancellation: OperationCancellationToken::new(),
        }
    }

    /// Applies a checked state transition.
    pub fn transition(
        &mut self,
        target: OperationState,
        now: UnixMillis,
        failure: Option<OperationFailure>,
    ) -> Result<(), TransitionError> {
        if now < self.updated_at {
            return Err(TransitionError::TimeMovedBackwards);
        }
        if !legal_transition(self.state, target) {
            return Err(TransitionError::Illegal {
                from: self.state,
                to: target,
            });
        }
        validate_failure(target, failure.as_ref())?;
        if target == OperationState::Cancelled {
            match self.cancellation.request_cancel() {
                super::CancellationSignal::Requested
                | super::CancellationSignal::AlreadyRequested => {}
                super::CancellationSignal::TooLate => {
                    return Err(TransitionError::CancellationAfterCommit);
                }
            }
        }
        self.state = target;
        self.updated_at = now;
        self.failure = failure;
        Ok(())
    }

    /// Marks the point after which cancellation cannot compensate safely.
    pub fn mark_committed(&mut self) -> Result<(), TransitionError> {
        if self.state != OperationState::Running {
            return Err(TransitionError::CommitOutsideRunning);
        }
        match self.cancellation.mark_committed() {
            CommitSignal::Committed | CommitSignal::AlreadyCommitted => {
                self.commit_reached = true;
                Ok(())
            }
            CommitSignal::CancellationWon => Err(TransitionError::CommitAfterCancellation),
        }
    }

    /// Returns whether explicit cancellation remains legal.
    ///
    /// Pending work is always cancellable. Running work is cancellable only
    /// for commands whose owner contract supports cooperative cancellation and
    /// only before the explicit commit point.
    pub const fn can_cancel(&self) -> bool {
        !self.commit_reached
            && (matches!(self.state, OperationState::Pending)
                || (matches!(self.state, OperationState::Running)
                    && self.command.supports_running_cancellation()))
    }

    pub fn cancellation_token(&self) -> OperationCancellationToken {
        self.cancellation.clone()
    }

    /// Creates the immutable Domain status projection.
    pub fn status(&self) -> Result<OperationStatus, OperationStatusError> {
        OperationStatus::new(
            self.id,
            self.kind.clone(),
            self.created_at,
            self.updated_at,
            self.state,
            self.failure.clone(),
        )
    }

    pub const fn id(&self) -> OperationId {
        self.id
    }
    pub const fn command(&self) -> &Command {
        &self.command
    }
    pub const fn state(&self) -> OperationState {
        self.state
    }
}

/// Illegal lifecycle transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionError {
    TimeMovedBackwards,
    Illegal {
        from: OperationState,
        to: OperationState,
    },
    MissingFailure,
    UnexpectedFailure,
    CommitOutsideRunning,
    CommitAfterCancellation,
    CancellationAfterCommit,
}

impl core::fmt::Display for TransitionError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "invalid operation transition: {self:?}; inspect coordinator state"
        )
    }
}

impl std::error::Error for TransitionError {}

const fn legal_transition(from: OperationState, to: OperationState) -> bool {
    matches!(
        (from, to),
        (OperationState::Pending, OperationState::Running)
            | (OperationState::Pending, OperationState::Cancelled)
            | (OperationState::Running, OperationState::Completed)
            | (OperationState::Running, OperationState::Failed)
            | (OperationState::Running, OperationState::Cancelled)
    )
}

fn validate_failure(
    state: OperationState,
    failure: Option<&OperationFailure>,
) -> Result<(), TransitionError> {
    match (state, failure) {
        (OperationState::Failed, None) => Err(TransitionError::MissingFailure),
        (OperationState::Failed, Some(_)) | (_, None) => Ok(()),
        (_, Some(_)) => Err(TransitionError::UnexpectedFailure),
    }
}
