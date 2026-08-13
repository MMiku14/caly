//! Mutation operation lifecycle values.

use core::fmt;

use crate::{BoundedText, OperationId};

/// Maximum operation-kind length in UTF-8 bytes.
pub const OPERATION_KIND_MAX_BYTES: usize = 64;
/// Maximum safe failure-message length in UTF-8 bytes.
pub const FAILURE_MESSAGE_MAX_BYTES: usize = 1_024;
/// Maximum suggested-action length in UTF-8 bytes.
pub const SUGGESTED_ACTION_MAX_BYTES: usize = 512;

/// Milliseconds since the Unix epoch.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct UnixMillis(u64);

impl UnixMillis {
    /// Constructs a timestamp from a non-negative wire-safe value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the primitive timestamp value.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Explicit operation lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationState {
    /// Accepted but not yet executing.
    Pending,
    /// Currently executing before or after cancellable boundaries.
    Running,
    /// Completed successfully.
    Completed,
    /// Completed with a structured failure.
    Failed,
    /// Cancelled at an allowed cancellation point.
    Cancelled,
}

impl OperationState {
    /// Returns whether this state can be retained in the terminal LRU.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Stable machine-readable operation failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationFailureCode {
    /// Input failed domain or schema validation.
    InvalidInput,
    /// The operation conflicts with current state.
    Conflict,
    /// A bounded runtime resource is exhausted.
    ResourceExhausted,
    /// A required capability is unavailable.
    Unsupported,
    /// A deadline elapsed at an explicitly timed boundary.
    DeadlineExceeded,
    /// Cancellation arrived after the commit point.
    TooLateToCancel,
    /// An infrastructure action failed.
    Infrastructure,
    /// A compensation action failed and recovery is required.
    RecoveryRequired,
}

/// Safe, bounded failure information exposed to clients.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationFailure {
    code: OperationFailureCode,
    message: BoundedText<FAILURE_MESSAGE_MAX_BYTES>,
    suggested_action: BoundedText<SUGGESTED_ACTION_MAX_BYTES>,
}

impl OperationFailure {
    /// Constructs actionable operation failure information.
    pub const fn new(
        code: OperationFailureCode,
        message: BoundedText<FAILURE_MESSAGE_MAX_BYTES>,
        suggested_action: BoundedText<SUGGESTED_ACTION_MAX_BYTES>,
    ) -> Self {
        Self {
            code,
            message,
            suggested_action,
        }
    }

    /// Infallible constructor that clamps message and action to the bounded
    /// capacity. The `Result`-returning `new` is the right call when a
    /// mis-sized input is a bug that should fail fast during development;
    /// this constructor is the right call for hot-path call sites where the
    /// inputs may be dynamic (caller-supplied strings, formatted OS errors)
    /// and an over-long or empty value must gracefully degrade to a
    /// placeholder rather than abort the daemon. Mirrors
    /// `ActorFailure::clamped` on the application-error side.
    pub fn clamped(code: OperationFailureCode, message: &str, suggested_action: &str) -> Self {
        Self {
            code,
            message: BoundedText::from_nonempty_clamped(message.to_owned(), "_"),
            suggested_action: BoundedText::from_nonempty_clamped(suggested_action.to_owned(), "_"),
        }
    }

    /// Returns the stable category.
    pub const fn code(&self) -> OperationFailureCode {
        self.code
    }

    /// Returns the safe failure description.
    pub const fn message(&self) -> &BoundedText<FAILURE_MESSAGE_MAX_BYTES> {
        &self.message
    }

    /// Returns the next action available to the user.
    pub const fn suggested_action(&self) -> &BoundedText<SUGGESTED_ACTION_MAX_BYTES> {
        &self.suggested_action
    }
}

/// Error returned when an operation status violates lifecycle invariants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationStatusError {
    /// Updated time precedes creation time.
    TimeMovedBackwards,
    /// Failed state has no failure details.
    MissingFailure,
    /// A non-failed state unexpectedly carries failure details.
    UnexpectedFailure,
}

impl fmt::Display for OperationStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::TimeMovedBackwards => "operation update time precedes creation time",
            Self::MissingFailure => "failed operation requires failure details",
            Self::UnexpectedFailure => "only a failed operation may carry failure details",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for OperationStatusError {}

/// Queryable, immutable operation status projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationStatus {
    id: OperationId,
    kind: BoundedText<OPERATION_KIND_MAX_BYTES>,
    created_at: UnixMillis,
    updated_at: UnixMillis,
    state: OperationState,
    failure: Option<OperationFailure>,
}

impl OperationStatus {
    /// Validates a status projection before it crosses a boundary.
    pub fn new(
        id: OperationId,
        kind: BoundedText<OPERATION_KIND_MAX_BYTES>,
        created_at: UnixMillis,
        updated_at: UnixMillis,
        state: OperationState,
        failure: Option<OperationFailure>,
    ) -> Result<Self, OperationStatusError> {
        validate_status(created_at, updated_at, state, failure.as_ref())?;
        Ok(Self {
            id,
            kind,
            created_at,
            updated_at,
            state,
            failure,
        })
    }

    /// Returns the operation identity.
    pub const fn id(&self) -> OperationId {
        self.id
    }
    /// Returns the stable command kind.
    pub const fn kind(&self) -> &BoundedText<OPERATION_KIND_MAX_BYTES> {
        &self.kind
    }
    /// Returns the creation time supplied by the operation owner.
    pub const fn created_at(&self) -> UnixMillis {
        self.created_at
    }
    /// Returns the latest update time supplied by the operation owner.
    pub const fn updated_at(&self) -> UnixMillis {
        self.updated_at
    }
    /// Returns the lifecycle state.
    pub const fn state(&self) -> OperationState {
        self.state
    }
    /// Returns failure details when the operation failed.
    pub const fn failure(&self) -> Option<&OperationFailure> {
        self.failure.as_ref()
    }
}

fn validate_status(
    created_at: UnixMillis,
    updated_at: UnixMillis,
    state: OperationState,
    failure: Option<&OperationFailure>,
) -> Result<(), OperationStatusError> {
    if updated_at < created_at {
        return Err(OperationStatusError::TimeMovedBackwards);
    }
    match (state, failure) {
        (OperationState::Failed, None) => Err(OperationStatusError::MissingFailure),
        (OperationState::Failed, Some(_)) | (_, None) => Ok(()),
        (_, Some(_)) => Err(OperationStatusError::UnexpectedFailure),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_status_rejects_failure_details() -> Result<(), Box<dyn std::error::Error>> {
        let failure = OperationFailure::new(
            OperationFailureCode::Conflict,
            BoundedText::new("conflict")?,
            BoundedText::new("retry")?,
        );
        let result = OperationStatus::new(
            OperationId::from_bytes([1; 16]),
            BoundedText::new("config.apply")?,
            UnixMillis::new(1),
            UnixMillis::new(2),
            OperationState::Running,
            Some(failure),
        );
        assert_eq!(result, Err(OperationStatusError::UnexpectedFailure));
        Ok(())
    }

    #[test]
    fn pending_and_running_are_not_terminal() {
        assert!(!OperationState::Pending.is_terminal());
        assert!(!OperationState::Running.is_terminal());
        assert!(OperationState::Completed.is_terminal());
    }

    /// Regression: the `operation_failure` builder in
    /// `crates/caly-application/src/actors/reporting.rs` used to do
    /// `BoundedText::new(dynamic_message).map_err(...)?` on the formatted
    /// failure message, which left the operation in a Started-but-not-Failed
    /// state when a chatty OS error overshot the bound. `OperationFailure::clamped`
    /// is the infallible constructor that mirrors `ActorFailure::clamped` on
    /// the actor-error side; this test pins the new behaviour.
    #[test]
    fn clamped_accepts_short_inputs() {
        let failure = OperationFailure::clamped(
            OperationFailureCode::Infrastructure,
            "mihomo start failed",
            "inspect controller health",
        );
        assert_eq!(failure.code(), OperationFailureCode::Infrastructure);
        assert_eq!(failure.message().as_str(), "mihomo start failed");
        assert_eq!(
            failure.suggested_action().as_str(),
            "inspect controller health"
        );
    }

    #[test]
    fn clamped_truncates_oversized_message_to_bound() {
        // 1 KiB / 0x400 = 2_048 four-byte characters, exceeds the
        // FAILURE_MESSAGE_MAX_BYTES bound of 1_024.
        let oversized: String = "🦀".repeat(2_048);
        let failure = OperationFailure::clamped(
            OperationFailureCode::DeadlineExceeded,
            oversized.as_str(),
            "short action",
        );
        let message = failure.message().as_str();
        assert!(message.len() <= 1_024, "message len = {}", message.len());
        assert!(message.is_char_boundary(message.len()));
    }

    #[test]
    fn clamped_replaces_empty_inputs_with_fallback() {
        let failure = OperationFailure::clamped(OperationFailureCode::Unsupported, "", "");
        // The wire representation must always carry non-empty fields; a
        // thin client never has to special-case an empty `message` cell.
        assert_eq!(failure.message().as_str(), "_");
        assert_eq!(failure.suggested_action().as_str(), "_");
    }
}
