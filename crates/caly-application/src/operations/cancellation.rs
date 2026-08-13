//! Per-operation cancellation state shared with the selected owner.
//!
//! The OperationStore remains the sole transition owner. This token is the
//! bounded, cloneable observation passed to an actor/backend so cooperative
//! commands can stop before their commit point without consulting global state.

use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

const ACTIVE: u8 = 0;
const CANCEL_REQUESTED: u8 = 1;
const COMMITTED: u8 = 2;

/// Runtime-owned cancellation observation for one operation.
#[derive(Clone, Debug)]
pub struct OperationCancellationToken(Arc<AtomicU8>);

impl OperationCancellationToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU8::new(ACTIVE)))
    }

    /// Requests cancellation atomically against the commit point.
    pub fn request_cancel(&self) -> CancellationSignal {
        match self.0.compare_exchange(
            ACTIVE,
            CANCEL_REQUESTED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => CancellationSignal::Requested,
            Err(CANCEL_REQUESTED) => CancellationSignal::AlreadyRequested,
            Err(COMMITTED) | Err(_) => CancellationSignal::TooLate,
        }
    }

    /// Marks the irreversible point atomically against cancellation.
    pub fn mark_committed(&self) -> CommitSignal {
        match self
            .0
            .compare_exchange(ACTIVE, COMMITTED, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => CommitSignal::Committed,
            Err(CANCEL_REQUESTED) => CommitSignal::CancellationWon,
            Err(COMMITTED) | Err(_) => CommitSignal::AlreadyCommitted,
        }
    }

    pub fn is_cancel_requested(&self) -> bool {
        self.0.load(Ordering::Acquire) == CANCEL_REQUESTED
    }

    pub fn is_committed(&self) -> bool {
        self.0.load(Ordering::Acquire) == COMMITTED
    }
}

impl Default for OperationCancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for OperationCancellationToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for OperationCancellationToken {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationSignal {
    Requested,
    AlreadyRequested,
    TooLate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitSignal {
    Committed,
    CancellationWon,
    AlreadyCommitted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_wins_before_commit() {
        let token = OperationCancellationToken::new();
        assert_eq!(token.request_cancel(), CancellationSignal::Requested);
        assert!(token.is_cancel_requested());
        assert_eq!(token.mark_committed(), CommitSignal::CancellationWon);
        assert!(!token.is_committed());
    }

    #[test]
    fn concurrent_cancel_and_commit_have_one_winner() -> Result<(), &'static str> {
        let token = OperationCancellationToken::new();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let cancel_token = token.clone();
        let cancel_barrier = Arc::clone(&barrier);
        let commit_token = token.clone();
        let commit_barrier = Arc::clone(&barrier);
        let cancel = std::thread::spawn(move || {
            cancel_barrier.wait();
            cancel_token.request_cancel()
        });
        let commit = std::thread::spawn(move || {
            commit_barrier.wait();
            commit_token.mark_committed()
        });
        barrier.wait();
        let cancel = cancel.join().map_err(|_| "cancel thread failed")?;
        let commit = commit.join().map_err(|_| "commit thread failed")?;
        assert!(matches!(
            (cancel, commit),
            (CancellationSignal::Requested, CommitSignal::CancellationWon)
                | (CancellationSignal::TooLate, CommitSignal::Committed)
        ));
        assert_ne!(token.is_cancel_requested(), token.is_committed());
        Ok(())
    }

    #[test]
    fn commit_rejects_late_cancellation() {
        let token = OperationCancellationToken::new();
        assert_eq!(token.mark_committed(), CommitSignal::Committed);
        assert!(token.is_committed());
        assert_eq!(token.request_cancel(), CancellationSignal::TooLate);
        assert!(!token.is_cancel_requested());
    }
}
