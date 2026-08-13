//! Strict operation status conversion.

use caly_domain::{
    BoundedText, OperationFailure, OperationFailureCode, OperationState, OperationStatus,
    UnixMillis,
};

use super::{operation_id_from_wire, DecodeError};
use crate::protocol::v2::{
    DecodeBudget, RawFailureCode, RawOperationState, WireOperationFailure, WireOperationStatus,
};

/// Converts an operation status without unknown-enum fallback.
pub fn operation_status_from_wire(
    wire: WireOperationStatus,
    budget: &mut DecodeBudget,
) -> Result<OperationStatus, DecodeError> {
    charge_text(budget, &wire.command_kind)?;
    let kind = BoundedText::new(wire.command_kind)
        .map_err(|error| invalid_text("operation.command_kind", error))?;
    let state = operation_state(wire.state)?;
    let failure = wire
        .failure
        .map(|value| failure_from_wire(value, budget))
        .transpose()?;
    OperationStatus::new(
        operation_id_from_wire(wire.operation_id),
        kind,
        UnixMillis::new(wire.created_at_unix_ms),
        UnixMillis::new(wire.updated_at_unix_ms),
        state,
        failure,
    )
    .map_err(|error| DecodeError::InvalidState {
        reason: error.to_string(),
    })
}

fn failure_from_wire(
    wire: WireOperationFailure,
    budget: &mut DecodeBudget,
) -> Result<OperationFailure, DecodeError> {
    charge_text(budget, &wire.message)?;
    charge_text(budget, &wire.suggested_action)?;
    let message = BoundedText::new(wire.message)
        .map_err(|error| invalid_text("operation.failure.message", error))?;
    let action = BoundedText::new(wire.suggested_action)
        .map_err(|error| invalid_text("operation.failure.suggested_action", error))?;
    Ok(OperationFailure::new(
        failure_code(wire.code)?,
        message,
        action,
    ))
}

fn operation_state(raw: RawOperationState) -> Result<OperationState, DecodeError> {
    match raw.0 {
        1 => Ok(OperationState::Pending),
        2 => Ok(OperationState::Running),
        3 => Ok(OperationState::Completed),
        4 => Ok(OperationState::Failed),
        5 => Ok(OperationState::Cancelled),
        value => Err(DecodeError::UnknownEnum {
            field: "operation.state",
            raw: value,
        }),
    }
}

fn failure_code(raw: RawFailureCode) -> Result<OperationFailureCode, DecodeError> {
    match raw.0 {
        1 => Ok(OperationFailureCode::InvalidInput),
        2 => Ok(OperationFailureCode::Conflict),
        3 => Ok(OperationFailureCode::ResourceExhausted),
        4 => Ok(OperationFailureCode::Unsupported),
        5 => Ok(OperationFailureCode::DeadlineExceeded),
        6 => Ok(OperationFailureCode::TooLateToCancel),
        7 => Ok(OperationFailureCode::Infrastructure),
        8 => Ok(OperationFailureCode::RecoveryRequired),
        value => Err(DecodeError::UnknownEnum {
            field: "operation.failure.code",
            raw: value,
        }),
    }
}

fn charge_text(budget: &mut DecodeBudget, value: &str) -> Result<(), DecodeError> {
    budget.check_string(value.len())?;
    budget.charge_bytes(value.len())?;
    Ok(())
}

fn invalid_text(field: &'static str, error: impl core::fmt::Display) -> DecodeError {
    DecodeError::InvalidText {
        field,
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::v2::DecodeLimits;

    #[test]
    fn unknown_state_is_rejected() {
        let wire = WireOperationStatus {
            operation_id: [1; 16],
            command_kind: "config.apply".to_owned(),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 2,
            state: RawOperationState(999),
            failure: None,
        };
        let result =
            operation_status_from_wire(wire, &mut DecodeBudget::new(DecodeLimits::v2_default()));
        assert!(matches!(
            result,
            Err(DecodeError::UnknownEnum { raw: 999, .. })
        ));
    }
}
