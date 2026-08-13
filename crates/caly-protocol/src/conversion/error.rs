//! Strict wire-to-Domain conversion errors.

use core::fmt;

use crate::protocol::v2::BudgetError;

/// Field-level decode failure with no fallback semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    Budget(BudgetError),
    MissingField(&'static str),
    InvalidIdentityLength { field: &'static str, actual: usize },
    UnknownEnum { field: &'static str, raw: i32 },
    InvalidText { field: &'static str, reason: String },
    InvalidState { reason: String },
    EpochMismatch,
}

impl DecodeError {
    /// Returns the recovery action available to the caller.
    pub const fn suggested_action(&self) -> &'static str {
        match self {
            Self::Budget(_) => "reject the payload and request a smaller bounded response",
            Self::EpochMismatch => "discard incremental state and request a full snapshot",
            _ => "reject the payload and report protocol incompatibility",
        }
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "protocol decode failed: {self:?}; {}",
            self.suggested_action()
        )
    }
}

impl std::error::Error for DecodeError {}

impl From<BudgetError> for DecodeError {
    fn from(value: BudgetError) -> Self {
        Self::Budget(value)
    }
}
