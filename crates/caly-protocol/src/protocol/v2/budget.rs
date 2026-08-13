//! Protocol decode budgets independent of transport message ceilings.

use core::fmt;

/// Negotiated and locally enforced decode limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DecodeLimits {
    pub max_nodes: usize,
    pub max_connections: usize,
    pub max_groups: usize,
    pub max_rules: usize,
    pub max_logs: usize,
    pub max_subscriptions: usize,
    pub max_string_bytes: usize,
    pub max_config_bytes: usize,
    pub max_message_bytes: usize,
}

impl DecodeLimits {
    /// Conservative v2 defaults; peers may negotiate only lower values.
    pub const fn v2_default() -> Self {
        Self {
            max_nodes: 10_000,
            max_connections: 10_000,
            max_groups: 256,
            max_rules: 10_000,
            max_logs: 1_000,
            max_subscriptions: 64,
            max_string_bytes: 64 * 1_024,
            max_config_bytes: 16 * 1_024 * 1_024,
            max_message_bytes: 20 * 1_024 * 1_024,
        }
    }
}

/// Mutable accounting for one strict decode operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeBudget {
    limits: DecodeLimits,
    consumed_message_bytes: usize,
}

impl DecodeBudget {
    /// Starts accounting with negotiated limits.
    pub const fn new(limits: DecodeLimits) -> Self {
        Self {
            limits,
            consumed_message_bytes: 0,
        }
    }

    /// Returns negotiated limits for a fresh per-message budget.
    pub const fn limits(self) -> DecodeLimits {
        self.limits
    }

    /// Charges bytes before allocation or nested conversion.
    pub fn charge_bytes(&mut self, amount: usize) -> Result<(), BudgetError> {
        let total = self
            .consumed_message_bytes
            .checked_add(amount)
            .ok_or(BudgetError::ArithmeticOverflow)?;
        if total > self.limits.max_message_bytes {
            return Err(BudgetError::MessageBytes {
                limit: self.limits.max_message_bytes,
                actual: total,
            });
        }
        self.consumed_message_bytes = total;
        Ok(())
    }

    /// Rejects an oversized string before Domain construction.
    pub const fn check_string(&self, actual: usize) -> Result<(), BudgetError> {
        if actual > self.limits.max_string_bytes {
            return Err(BudgetError::StringBytes {
                limit: self.limits.max_string_bytes,
                actual,
            });
        }
        Ok(())
    }

    /// Rejects an oversized named collection.
    pub const fn check_count(
        &self,
        kind: CollectionKind,
        actual: usize,
    ) -> Result<(), BudgetError> {
        let limit = match kind {
            CollectionKind::Nodes => self.limits.max_nodes,
            CollectionKind::Connections => self.limits.max_connections,
            CollectionKind::Groups => self.limits.max_groups,
            CollectionKind::Rules => self.limits.max_rules,
            CollectionKind::Logs => self.limits.max_logs,
            CollectionKind::Subscriptions => self.limits.max_subscriptions,
        };
        if actual > limit {
            return Err(BudgetError::Collection {
                kind,
                limit,
                actual,
            });
        }
        Ok(())
    }
}

/// Collection category with its own negotiated limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionKind {
    Nodes,
    Connections,
    Groups,
    Rules,
    Logs,
    Subscriptions,
}

/// Decode rejection before lossy allocation or conversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetError {
    ArithmeticOverflow,
    MessageBytes {
        limit: usize,
        actual: usize,
    },
    StringBytes {
        limit: usize,
        actual: usize,
    },
    Collection {
        kind: CollectionKind,
        limit: usize,
        actual: usize,
    },
}

impl fmt::Display for BudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "decode budget exceeded: {self:?}; request a smaller payload"
        )
    }
}

impl std::error::Error for BudgetError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_accounting_never_wraps() {
        let mut budget = DecodeBudget::new(DecodeLimits::v2_default());
        assert_eq!(
            budget.charge_bytes(usize::MAX),
            Err(BudgetError::MessageBytes {
                limit: DecodeLimits::v2_default().max_message_bytes,
                actual: usize::MAX,
            })
        );
    }

    #[test]
    fn collection_limits_are_independent() {
        let budget = DecodeBudget::new(DecodeLimits::v2_default());
        assert!(
            budget
                .check_count(CollectionKind::Subscriptions, 65)
                .is_err()
        );
        assert!(budget.check_count(CollectionKind::Groups, 65).is_ok());
    }
}
