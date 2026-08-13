//! Shared transport facade for the daemon composition root.

use std::{
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use caly_domain::{EventCursor, OperationId, OperationStatus, PresentationSnapshot, UnixMillis};

use crate::projection::ProjectionRuntime;
use crate::{
    command_bus::CommandEnvelope,
    events::SequencedEvent,
    operations::TimeSource,
    service::runtime_service::RuntimeService,
    service::{
        ApplicationServiceError, ApplicationServicePort, ApplicationWatch, CancellationResult,
    },
};

impl ApplicationServicePort for Arc<Mutex<RuntimeService<WallClock, ProjectionRuntime>>> {
    fn submit(
        &mut self,
        envelope: CommandEnvelope,
    ) -> Result<OperationStatus, ApplicationServiceError> {
        self.lock()
            .map_err(|_| ApplicationServiceError::InternalInvariant)?
            .submit(envelope)
    }
    fn cancel(&mut self, id: OperationId) -> Result<CancellationResult, ApplicationServiceError> {
        self.lock()
            .map_err(|_| ApplicationServiceError::InternalInvariant)?
            .cancel(id)
    }
    fn operation_status(
        &self,
        id: OperationId,
    ) -> Result<OperationStatus, ApplicationServiceError> {
        self.lock()
            .map_err(|_| ApplicationServiceError::InternalInvariant)?
            .operation_status(id)
    }
    fn snapshot(&self) -> Result<PresentationSnapshot, ApplicationServiceError> {
        self.lock()
            .map_err(|_| ApplicationServiceError::InternalInvariant)?
            .snapshot()
    }
    fn watch_after(
        &self,
        cursor: Option<EventCursor>,
    ) -> Result<ApplicationWatch, ApplicationServiceError> {
        self.lock()
            .map_err(|_| ApplicationServiceError::InternalInvariant)?
            .watch_after(cursor)
    }
    fn subscribe_live(&self) -> tokio::sync::broadcast::Receiver<SequencedEvent> {
        let Ok(guard) = self.lock() else {
            // Poisoned service cannot publish; return a channel that yields no
            // events by creating a detached receiver.
            let (tx, rx) = tokio::sync::broadcast::channel(1);
            drop(tx);
            return rx;
        };
        guard.subscribe_live()
    }
}

// P7 composition 抽离的接缝:上面的 `ApplicationServicePort` facade impl 只能
// 落在 trait 属主 crate(`Arc` 非 fundamental,orphan 规则),具体时钟随之
// 留下;caly-composition 组装根经 `service::WallClock` 重导出取用原类型。
/// Monotonic-enough wall clock used only by the composition root.
#[derive(Default)]
pub struct WallClock;

impl TimeSource for WallClock {
    fn now(&mut self) -> UnixMillis {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis().min(u128::from(u64::MAX))).unwrap_or(u64::MAX)
            });
        UnixMillis::new(millis)
    }
}
