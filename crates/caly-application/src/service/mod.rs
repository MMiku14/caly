//! Transport-facing Application service port.

use caly_domain::{
    BoundedText, BoundedVec, EventCursor, OperationId, OperationStatus, PresentationSnapshot,
};

use crate::{command_bus::CommandEnvelope, events::SequencedEvent, operations::CancelDecision};

/// Maximum replay events returned in one bounded service batch.
pub const MAX_REPLAY_BATCH: usize = 1_024;
pub type ReplayBatch = BoundedVec<SequencedEvent, MAX_REPLAY_BATCH>;

/// Watch recovery result selected by the Application owner.
pub enum ApplicationWatch {
    Replay(ReplayBatch),
    FullSnapshot(Box<PresentationSnapshot>),
}

/// Cancellation result with authoritative current status.
pub struct CancellationResult {
    pub decision: CancelDecision,
    pub status: OperationStatus,
}

/// Safe service error mapped by Transport in one place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplicationServiceError {
    ResourceExhausted,
    OperationNotFound,
    IdempotencyConflict,
    InvalidCommand(BoundedText<512>),
    Unavailable,
    InternalInvariant,
}

impl core::fmt::Display for ApplicationServiceError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "application service rejected request: {self:?}")
    }
}

impl std::error::Error for ApplicationServiceError {}

/// All transport calls enter Application through this interface.
pub trait ApplicationServicePort {
    fn submit(
        &mut self,
        envelope: CommandEnvelope,
    ) -> Result<OperationStatus, ApplicationServiceError>;
    fn cancel(
        &mut self,
        operation_id: OperationId,
    ) -> Result<CancellationResult, ApplicationServiceError>;
    fn operation_status(
        &self,
        operation_id: OperationId,
    ) -> Result<OperationStatus, ApplicationServiceError>;
    fn snapshot(&self) -> Result<PresentationSnapshot, ApplicationServiceError>;
    fn watch_after(
        &self,
        cursor: Option<EventCursor>,
    ) -> Result<ApplicationWatch, ApplicationServiceError>;
    /// Subscribes to live projection events for a continuous watch stream.
    fn subscribe_live(&self) -> tokio::sync::broadcast::Receiver<SequencedEvent>;
}

mod command_support;
pub(crate) mod results;
pub(crate) mod runtime_service;
pub(crate) mod shared;

pub use command_support::CommandSupportPolicy;
// P7:WallClock 受 orphan 规则锚定在本 crate(shared.rs 接缝注记),
// 经服务根对 caly-composition 公开。
pub use results::{ActorResultError, ActorResultOutcome};
pub use runtime_service::{
    DispatchOutcome, DispatchRejectReason, ProjectionService, RuntimeDispatchError, RuntimeService,
};
pub use shared::WallClock;
