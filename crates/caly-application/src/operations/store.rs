//! Idempotent operation storage with terminal-only eviction.

use std::collections::{HashMap, VecDeque};

use caly_domain::{OperationId, OperationState, UnixMillis};

use super::{OperationCancellationToken, OperationRecord, TransitionError};

/// Insert result distinguishing idempotent replay from new acceptance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertOutcome {
    Inserted,
    Existing,
}

/// Explicit cancellation result independent of client wait lifetime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelDecision {
    Cancelled,
    AlreadyTerminal,
    TooLateToCancel,
}

/// Store failure with no optimistic success.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreError {
    InvalidCapacity,
    ResourceExhausted,
    IdempotencyConflict,
    NotFound,
    NotPending,
    Transition(TransitionError),
}

/// Single-owner operation store.
pub struct OperationStore {
    records: HashMap<OperationId, OperationRecord>,
    terminal_order: VecDeque<OperationId>,
    total_capacity: usize,
    terminal_capacity: usize,
}

impl OperationStore {
    /// Creates bounded total admission and terminal retention.
    pub fn new(total_capacity: usize, terminal_capacity: usize) -> Result<Self, StoreError> {
        if terminal_capacity == 0 || total_capacity < terminal_capacity {
            return Err(StoreError::InvalidCapacity);
        }
        Ok(Self {
            records: HashMap::new(),
            terminal_order: VecDeque::with_capacity(terminal_capacity),
            total_capacity,
            terminal_capacity,
        })
    }

    /// Inserts once; active records are never evicted to admit new work.
    pub fn insert(&mut self, record: OperationRecord) -> Result<InsertOutcome, StoreError> {
        if let Some(existing) = self.records.get(&record.id()) {
            return if existing.command() == record.command() {
                Ok(InsertOutcome::Existing)
            } else {
                Err(StoreError::IdempotencyConflict)
            };
        }
        if self.records.len() == self.total_capacity {
            self.evict_oldest_terminal();
        }
        if self.records.len() == self.total_capacity {
            return Err(StoreError::ResourceExhausted);
        }
        self.records.insert(record.id(), record);
        Ok(InsertOutcome::Inserted)
    }

    /// Returns an operation without mutating retention order.
    pub fn get(&self, id: OperationId) -> Option<&OperationRecord> {
        self.records.get(&id)
    }

    pub fn cancellation_token(
        &self,
        id: OperationId,
    ) -> Result<OperationCancellationToken, StoreError> {
        self.records
            .get(&id)
            .map(OperationRecord::cancellation_token)
            .ok_or(StoreError::NotFound)
    }

    /// Transitions a record and retains only terminal records in the LRU queue.
    pub fn transition(
        &mut self,
        id: OperationId,
        target: OperationState,
        now: UnixMillis,
        failure: Option<caly_domain::OperationFailure>,
    ) -> Result<(), StoreError> {
        let record = self.records.get_mut(&id).ok_or(StoreError::NotFound)?;
        record
            .transition(target, now, failure)
            .map_err(StoreError::Transition)?;
        if target.is_terminal() {
            self.retain_terminal(id);
        }
        Ok(())
    }

    /// Rolls back a reservation only while it is still pending.
    pub fn remove_pending(&mut self, id: OperationId) -> Result<OperationRecord, StoreError> {
        let state = self.records.get(&id).ok_or(StoreError::NotFound)?.state();
        if state != OperationState::Pending {
            return Err(StoreError::NotPending);
        }
        self.records.remove(&id).ok_or(StoreError::NotFound)
    }

    /// Handles only an explicit cancellation request.
    pub fn cancel(
        &mut self,
        id: OperationId,
        now: UnixMillis,
    ) -> Result<CancelDecision, StoreError> {
        let record = self.records.get(&id).ok_or(StoreError::NotFound)?;
        if record.state().is_terminal() {
            return Ok(CancelDecision::AlreadyTerminal);
        }
        if !record.can_cancel() {
            return Ok(CancelDecision::TooLateToCancel);
        }
        self.transition(id, OperationState::Cancelled, now, None)?;
        Ok(CancelDecision::Cancelled)
    }

    /// Marks the coordinator commit point.
    pub fn mark_committed(&mut self, id: OperationId) -> Result<(), StoreError> {
        self.records
            .get_mut(&id)
            .ok_or(StoreError::NotFound)?
            .mark_committed()
            .map_err(StoreError::Transition)
    }

    fn retain_terminal(&mut self, id: OperationId) {
        self.terminal_order.retain(|candidate| *candidate != id);
        self.terminal_order.push_back(id);
        while self.terminal_order.len() > self.terminal_capacity {
            self.evict_oldest_terminal();
        }
    }

    fn evict_oldest_terminal(&mut self) {
        while let Some(expired) = self.terminal_order.pop_front() {
            let removable = self
                .records
                .get(&expired)
                .is_some_and(|record| record.state().is_terminal());
            if removable {
                self.records.remove(&expired);
                break;
            }
        }
    }
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "operation store failure: {self:?}; query operation status and retry safely"
        )
    }
}

impl std::error::Error for StoreError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::OperationRecord;
    use caly_domain::{BoundedText, OperationId};

    fn record(byte: u8) -> Result<OperationRecord, caly_domain::TextError> {
        Ok(OperationRecord::pending(
            OperationId::from_bytes([byte; 16]),
            crate::command_bus::Command::ApplyConfig {
                candidate_id: [byte; 16],
            },
            BoundedText::new("config.apply")?,
            UnixMillis::new(1),
        ))
    }

    fn lifecycle_record(byte: u8) -> Result<OperationRecord, caly_domain::TextError> {
        Ok(OperationRecord::pending(
            OperationId::from_bytes([byte; 16]),
            crate::command_bus::Command::SwitchCore {
                target: caly_domain::CoreKind::Mihomo,
                action: crate::command_bus::CoreAction::Restart,
            },
            BoundedText::new("core.switch")?,
            UnixMillis::new(1),
        ))
    }

    #[test]
    fn commit_point_rejects_cancellation() -> Result<(), Box<dyn std::error::Error>> {
        let id = OperationId::from_bytes([1; 16]);
        let mut store = OperationStore::new(4, 2)?;
        store.insert(record(1)?)?;
        let token = store.cancellation_token(id)?;
        store.transition(id, OperationState::Running, UnixMillis::new(2), None)?;
        store.mark_committed(id)?;
        assert!(token.is_committed());
        assert_eq!(
            store.cancel(id, UnixMillis::new(3))?,
            CancelDecision::TooLateToCancel
        );
        Ok(())
    }

    #[test]
    fn pending_cancel_signals_the_owner_token() -> Result<(), Box<dyn std::error::Error>> {
        let id = OperationId::from_bytes([8; 16]);
        let mut store = OperationStore::new(4, 2)?;
        store.insert(record(8)?)?;
        let token = store.cancellation_token(id)?;
        assert!(!token.is_cancel_requested());
        assert_eq!(
            store.cancel(id, UnixMillis::new(2))?,
            CancelDecision::Cancelled
        );
        assert!(token.is_cancel_requested());
        Ok(())
    }

    #[test]
    fn cooperative_running_cancel_signals_owner_before_commit()
    -> Result<(), Box<dyn std::error::Error>> {
        let id = OperationId::from_bytes([7; 16]);
        let mut store = OperationStore::new(4, 2)?;
        store.insert(record(7)?)?;
        let token = store.cancellation_token(id)?;
        store.transition(id, OperationState::Running, UnixMillis::new(2), None)?;
        assert_eq!(
            store.cancel(id, UnixMillis::new(3))?,
            CancelDecision::Cancelled
        );
        assert!(token.is_cancel_requested());
        assert_eq!(
            store.get(id).map(OperationRecord::state),
            Some(OperationState::Cancelled)
        );
        Ok(())
    }

    #[test]
    fn running_without_owner_cancellation_is_too_late() -> Result<(), Box<dyn std::error::Error>> {
        let id = OperationId::from_bytes([9; 16]);
        let mut store = OperationStore::new(4, 2)?;
        store.insert(lifecycle_record(9)?)?;
        store.transition(id, OperationState::Running, UnixMillis::new(2), None)?;
        assert_eq!(
            store.cancel(id, UnixMillis::new(3))?,
            CancelDecision::TooLateToCancel
        );
        assert_eq!(
            store.get(id).map(OperationRecord::state),
            Some(OperationState::Running)
        );
        Ok(())
    }

    #[test]
    fn active_record_is_not_evicted_for_new_admission() -> Result<(), Box<dyn std::error::Error>> {
        let mut store = OperationStore::new(1, 1)?;
        store.insert(record(1)?)?;
        assert_eq!(store.insert(record(2)?), Err(StoreError::ResourceExhausted));
        assert!(store.get(OperationId::from_bytes([1; 16])).is_some());
        Ok(())
    }
}
