//! Shared terminal ActorReport construction and submission.

use caly_domain::{OperationFailure, OperationFailureCode, OperationId, PresentationDelta};

use crate::actor_result::{ActorReport, ActorResultClient, ActorResultSendError, ResultDeltas};

use super::{ActorFailure, ActorFailureKind};

#[derive(Debug)]
pub enum HandlerReportError {
    DeltaCapacity,
    ResultMailbox(ActorResultSendError),
}

pub fn report_completed(
    client: &ActorResultClient,
    operation_id: OperationId,
    deltas: Vec<PresentationDelta>,
) -> Result<(), HandlerReportError> {
    let deltas =
        ResultDeltas::try_from_vec(deltas).map_err(|_| HandlerReportError::DeltaCapacity)?;
    tracing::debug!(operation = ?operation_id, "operation completed");
    client
        .try_report(ActorReport::Completed {
            operation_id,
            deltas,
        })
        .map_err(HandlerReportError::ResultMailbox)
}

pub fn report_failed(
    client: &ActorResultClient,
    operation_id: OperationId,
    failure: ActorFailure,
    deltas: Vec<PresentationDelta>,
) -> Result<(), HandlerReportError> {
    let deltas =
        ResultDeltas::try_from_vec(deltas).map_err(|_| HandlerReportError::DeltaCapacity)?;
    let failure = operation_failure(failure);
    tracing::warn!(
        operation = ?operation_id,
        code = ?failure.code(),
        "operation failed: {}",
        failure.message().as_str()
    );
    client
        .try_report(ActorReport::Failed {
            operation_id,
            failure,
            deltas,
        })
        .map_err(HandlerReportError::ResultMailbox)
}

fn operation_failure(value: ActorFailure) -> OperationFailure {
    let code = match value.kind {
        ActorFailureKind::InvalidCandidate => OperationFailureCode::InvalidInput,
        ActorFailureKind::Unsupported => OperationFailureCode::Unsupported,
        ActorFailureKind::ResourceExhausted => OperationFailureCode::ResourceExhausted,
        ActorFailureKind::DeadlineExceeded => OperationFailureCode::DeadlineExceeded,
        ActorFailureKind::Infrastructure => OperationFailureCode::Infrastructure,
        ActorFailureKind::GenerationConflict => OperationFailureCode::Conflict,
        ActorFailureKind::RecoveryRequired => OperationFailureCode::RecoveryRequired,
    };
    // `value.message` and `value.suggested_action` are dynamic strings
    // assembled by callers via `format!` and friends (e.g. the
    // `tun_cap_net_admin_hint` interpolation in `MihomoLifecycleBackend`).
    // They can easily exceed the 1 KiB / 512-byte bounded capacities on a
    // chatty OS error. The previous `BoundedText::new(...)?` form was an
    // early-return path that left the operation in a Started-but-not-Failed
    // state — every owner handler propagates `HandlerReportError`, so a
    // FailureText return means the operation would be forever stuck in
    // `Running` until cancelled. `OperationFailure::clamped` keeps the
    // same behaviour for the well-formed call sites and surfaces a stable
    // `"_"` fallback for any future error variant that overshoots the
    // bound, so the failure reaches `client.try_report` and the operation
    // reaches a terminal state.
    OperationFailure::clamped(
        code,
        value.message.as_str(),
        value.suggested_action.as_str(),
    )
}

/// Reports a command outcome as Completed or Failed, mapping the deltas/failure
/// onto the terminal `ActorReport`. This is the shared tail every owner handler
/// repeats, so it is centralized here.
pub fn report_outcome(
    client: &ActorResultClient,
    operation_id: OperationId,
    outcome: Result<Vec<PresentationDelta>, ActorFailure>,
) -> Result<(), HandlerReportError> {
    match outcome {
        Ok(deltas) => report_completed(client, operation_id, deltas),
        Err(failure) => report_failed(client, operation_id, failure, Vec::new()),
    }
}

#[cfg(test)]
mod reporting_tests {
    use super::*;
    use crate::actor_result::{ActorReport, ActorResultClient, actor_result_mailbox};
    use std::time::Duration;

    /// Regression: the previous `report_failed` form propagated a
    /// `HandlerReportError::FailureText` whenever the dynamic failure
    /// message overshot the bounded capacity, leaving the operation in a
    /// Started-but-not-Failed state and the dispatcher loop moving on
    /// without ever seeing a terminal report. `OperationFailure::clamped`
    /// closes the window: any failure message that overshoots the bound
    /// is truncated to a stable placeholder and the failure still reaches
    /// the result mailbox, so the operation reaches a terminal state.
    #[test]
    fn report_failed_accepts_oversized_actor_failure_message() -> Result<(), String> {
        let (ingress, receiver) =
            actor_result_mailbox(4).map_err(|error| format!("mailbox: {error}"))?;
        let client = ActorResultClient::new(ingress);
        // 2 KiB of multi-byte chars: far past the 1 KiB bounded
        // failure-message capacity and the previous abort path.
        let oversized: String = "🦀".repeat(2_048);
        let failure = ActorFailure::clamped(
            caly_ports::ActorFailureKind::Infrastructure,
            oversized.as_str(),
            "short action",
        );
        let operation_id = OperationId::from_bytes([9; 16]);
        report_failed(&client, operation_id, failure, Vec::new())
            .map_err(|error| format!("report_failed returned Err: {error:?}"))?;
        let report = receiver
            .receive_timeout(Duration::from_millis(10))
            .map_err(|error| format!("no report delivered: {error:?}"))?;
        match report {
            ActorReport::Failed { failure, .. } => {
                let message = failure.message().as_str();
                assert!(
                    message.len() <= 1_024,
                    "message not clamped: len = {}",
                    message.len()
                );
            }
            other => return Err(format!("expected Failed report, got {other:?}")),
        }
        Ok(())
    }

    /// Empty ActorFailure message / action must still produce a
    /// well-formed terminal report, not a `FailureText` early-return.
    #[test]
    fn report_failed_clamps_empty_failure_text() -> Result<(), String> {
        let (ingress, receiver) =
            actor_result_mailbox(4).map_err(|error| format!("mailbox: {error}"))?;
        let client = ActorResultClient::new(ingress);
        let failure = ActorFailure::clamped(caly_ports::ActorFailureKind::Unsupported, "", "");
        let operation_id = OperationId::from_bytes([10; 16]);
        report_failed(&client, operation_id, failure, Vec::new())
            .map_err(|error| format!("report_failed returned Err: {error:?}"))?;
        let report = receiver
            .receive_timeout(Duration::from_millis(10))
            .map_err(|error| format!("no report delivered: {error:?}"))?;
        match report {
            ActorReport::Failed { failure, .. } => {
                // Wire representation always carries non-empty fields.
                assert_eq!(failure.message().as_str(), "_");
                assert_eq!(failure.suggested_action().as_str(), "_");
            }
            other => return Err(format!("expected Failed report, got {other:?}")),
        }
        Ok(())
    }
}
