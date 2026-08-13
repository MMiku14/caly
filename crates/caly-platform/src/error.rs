//! Bounded infrastructure failure shared by platform components.

use caly_domain::BoundedText;

/// Safe platform failure with an actionable remediation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformFailure {
    pub operation: BoundedText<64>,
    pub resource: BoundedText<256>,
    pub message: BoundedText<1_024>,
    pub suggested_action: BoundedText<512>,
}

impl core::fmt::Display for PlatformFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{} ({})", self.message, self.suggested_action)
    }
}
