//! Queryable mutation operation ownership.

mod admission;
mod cancellation;
mod record;
mod store;

pub use admission::{AdmissionController, AdmissionError, TimeSource};
pub use cancellation::{CancellationSignal, CommitSignal, OperationCancellationToken};
pub use record::{OperationRecord, TransitionError};
pub use store::{CancelDecision, InsertOutcome, OperationStore, StoreError};
