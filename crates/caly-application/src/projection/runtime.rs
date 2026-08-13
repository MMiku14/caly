//! Single-owner reliable projection, replay and snapshot service.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use caly_domain::{EventCursor, PresentationDelta, PresentationSnapshot};

use crate::{
    events::{EventSequencer, SequencedEvent, SequencerError},
    service::runtime_service::ProjectionService,
    service::{ApplicationServiceError, ApplicationWatch, ReplayBatch},
};

use super::{ProjectionError, StateProjector};

/// Projection runtime failure; apply/channel failures poison further publication.
#[derive(Debug)]
pub enum ProjectionRuntimeError {
    InvalidCapacity,
    Poisoned,
    Sequencer(SequencerError),
    ReliableChannelEmpty,
    ReliableChannelClosed,
    CursorMismatch,
    Projector(ProjectionError),
}

impl core::fmt::Display for ProjectionRuntimeError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "projection runtime failed: {self:?}; replace from an authoritative snapshot"
        )
    }
}

impl std::error::Error for ProjectionRuntimeError {}

/// Capacity of the best-effort live broadcast ring buffer. Slow subscribers lag
/// and are told to re-fetch a full snapshot; the authoritative projector is
/// never blocked by a live fan-out.
pub const LIVE_BROADCAST_CAPACITY: usize = 256;

/// Sole owner of sequencer, reliable receiver and state projector.
pub struct ProjectionRuntime {
    sequencer: EventSequencer,
    receiver: Receiver<SequencedEvent>,
    projector: StateProjector,
    channel_capacity: usize,
    replay_capacity: usize,
    poisoned: bool,
    live: tokio::sync::broadcast::Sender<SequencedEvent>,
}

impl ProjectionRuntime {
    /// Starts from one coherent snapshot and bounded channel/replay capacities.
    pub fn new(
        snapshot: PresentationSnapshot,
        channel_capacity: usize,
        replay_capacity: usize,
    ) -> Result<Self, ProjectionRuntimeError> {
        if channel_capacity == 0 || replay_capacity == 0 || replay_capacity > 1_024 {
            return Err(ProjectionRuntimeError::InvalidCapacity);
        }
        let cursor = snapshot.cursor();
        if cursor.daemon_instance() != snapshot.daemon_instance() {
            return Err(ProjectionRuntimeError::CursorMismatch);
        }
        let (sender, receiver) = mpsc::sync_channel(channel_capacity);
        let sequencer = EventSequencer::new_at(cursor, replay_capacity, sender)
            .map_err(ProjectionRuntimeError::Sequencer)?;
        let (live, _) = tokio::sync::broadcast::channel(LIVE_BROADCAST_CAPACITY);
        Ok(Self {
            sequencer,
            receiver,
            projector: StateProjector::from_snapshot(snapshot),
            channel_capacity,
            replay_capacity,
            poisoned: false,
            live,
        })
    }

    /// Orders and reliably applies one complete projection-slice replacement.
    pub fn publish(
        &mut self,
        delta: PresentationDelta,
    ) -> Result<EventCursor, ProjectionRuntimeError> {
        if self.poisoned {
            return Err(ProjectionRuntimeError::Poisoned);
        }
        let cursor = self
            .sequencer
            .try_publish(delta)
            .map_err(ProjectionRuntimeError::Sequencer)?;
        let delivered = match self.receiver.try_recv() {
            Ok(event) => event,
            // A send→recv pair in the same thread cannot race, so an
            // Empty here is defensive only — a retryable anomaly, not a
            // poisoned state (W3a BUG-3).
            Err(TryRecvError::Empty) => {
                return Err(ProjectionRuntimeError::ReliableChannelEmpty);
            }
            Err(TryRecvError::Disconnected) => {
                return self.poison(ProjectionRuntimeError::ReliableChannelClosed);
            }
        };
        if delivered.cursor != cursor {
            return self.poison(ProjectionRuntimeError::CursorMismatch);
        }
        // Fan the event out to live watch subscribers (best-effort: a full ring
        // simply makes a slow subscriber lag and recover from a full snapshot).
        let _ = self.live.send(delivered.clone());
        if let Err(error) = self.projector.apply(delivered) {
            return self.poison(ProjectionRuntimeError::Projector(error));
        }
        Ok(cursor)
    }

    /// Rebuilds projection ownership from an authoritative snapshot after poisoning.
    pub fn recover_from_snapshot(
        &mut self,
        snapshot: PresentationSnapshot,
    ) -> Result<(), ProjectionRuntimeError> {
        if snapshot.cursor().daemon_instance() != snapshot.daemon_instance() {
            return Err(ProjectionRuntimeError::CursorMismatch);
        }
        let (sender, receiver) = mpsc::sync_channel(self.channel_capacity);
        self.sequencer = EventSequencer::new_at(snapshot.cursor(), self.replay_capacity, sender)
            .map_err(ProjectionRuntimeError::Sequencer)?;
        self.receiver = receiver;
        self.projector = StateProjector::from_snapshot(snapshot);
        self.poisoned = false;
        Ok(())
    }

    /// Rebuilds the runtime from the projector's own last-consistent state
    /// after a poisoning fault (W3a BUG-3): the projector keeps the last
    /// applied snapshot, so a self-heal is possible without an external
    /// authority. Returns `Err` when the projector state itself is gone.
    pub fn try_recover(&mut self) -> Result<(), ProjectionRuntimeError> {
        if !self.poisoned {
            return Ok(());
        }
        let snapshot = self.projector.snapshot();
        self.recover_from_snapshot(snapshot)
    }

    /// Returns the current authoritative immutable snapshot.
    pub fn current_snapshot(&self) -> Result<PresentationSnapshot, ProjectionRuntimeError> {
        if self.poisoned {
            return Err(ProjectionRuntimeError::Poisoned);
        }
        Ok(self.projector.snapshot())
    }

    /// Subscribes to live projection events after the current position. A slow
    /// subscriber receives `Lagged` and must recover from a full snapshot.
    pub fn subscribe_live(&self) -> tokio::sync::broadcast::Receiver<SequencedEvent> {
        self.live.subscribe()
    }

    fn poison<T>(&mut self, error: ProjectionRuntimeError) -> Result<T, ProjectionRuntimeError> {
        self.poisoned = true;
        Err(error)
    }
}

impl ProjectionService for ProjectionRuntime {
    fn snapshot(&self) -> Result<PresentationSnapshot, ApplicationServiceError> {
        self.current_snapshot()
            .map_err(|_| ApplicationServiceError::InternalInvariant)
    }

    fn watch_after(
        &self,
        cursor: Option<EventCursor>,
    ) -> Result<ApplicationWatch, ApplicationServiceError> {
        if self.poisoned {
            return Err(ApplicationServiceError::InternalInvariant);
        }
        let Some(cursor) = cursor else {
            return Ok(ApplicationWatch::FullSnapshot(Box::new(
                self.projector.snapshot(),
            )));
        };
        match self.sequencer.replay_after(cursor) {
            Some(events) => ReplayBatch::try_from_vec(events)
                .map(ApplicationWatch::Replay)
                .map_err(|_| ApplicationServiceError::InternalInvariant),
            None => Ok(ApplicationWatch::FullSnapshot(Box::new(
                self.projector.snapshot(),
            ))),
        }
    }

    fn subscribe_live(&self) -> tokio::sync::broadcast::Receiver<SequencedEvent> {
        ProjectionRuntime::subscribe_live(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_domain::{
        AppliedState, BoundedVec, CapabilitySet, DaemonInstanceId, DesiredState, EventSequence,
        ObservedState, PlatformEffectView, ProxyMode, SnapshotRevision,
    };

    fn snapshot() -> Result<PresentationSnapshot, Box<dyn std::error::Error>> {
        let daemon = DaemonInstanceId::from_bytes([1; 16]);
        let cursor = EventCursor::new(daemon, EventSequence::ZERO);
        let capabilities = CapabilitySet::new(BoundedVec::new())?;
        Ok(PresentationSnapshot::new(
            daemon,
            SnapshotRevision::new(0),
            cursor,
            DesiredState::new(ProxyMode::Rule, None, None, false, false),
            AppliedState::stopped(),
            ObservedState::default(),
            PlatformEffectView::none(),
            capabilities,
            BoundedVec::new(),
        ))
    }

    #[test]
    fn publish_updates_snapshot_and_replay_together() -> Result<(), Box<dyn std::error::Error>> {
        let initial = snapshot()?;
        let initial_cursor = initial.cursor();
        let mut runtime = ProjectionRuntime::new(initial, 4, 4)?;
        let observed = ObservedState::new(10, 20, 3, 0);
        let cursor = runtime.publish(PresentationDelta::ObservedReplaced(observed))?;
        assert_eq!(runtime.current_snapshot()?.observed(), &observed);
        assert_eq!(cursor.sequence(), EventSequence::new(1));
        let watch = runtime.watch_after(Some(initial_cursor))?;
        assert!(matches!(watch, ApplicationWatch::Replay(events) if events.len() == 1));
        Ok(())
    }

    #[test]
    fn authoritative_snapshot_can_rebuild_runtime() -> Result<(), Box<dyn std::error::Error>> {
        let initial = snapshot()?;
        let mut runtime = ProjectionRuntime::new(initial.clone(), 4, 4)?;
        runtime.recover_from_snapshot(initial.clone())?;
        assert_eq!(runtime.current_snapshot()?, initial);
        Ok(())
    }

    #[test]
    fn future_cursor_receives_full_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let initial = snapshot()?;
        let daemon = initial.daemon_instance();
        let runtime = ProjectionRuntime::new(initial, 4, 4)?;
        let future = EventCursor::new(daemon, EventSequence::new(9));
        assert!(matches!(
            runtime.watch_after(Some(future))?,
            ApplicationWatch::FullSnapshot(_)
        ));
        Ok(())
    }

    #[test]
    fn mismatched_epoch_requires_full_snapshot_recovery() -> Result<(), Box<dyn std::error::Error>>
    {
        // A cursor from a previous daemon epoch must not replay; the client
        // recovers by receiving an authoritative full snapshot instead.
        let initial = snapshot()?;
        let old_epoch = DaemonInstanceId::from_bytes([0xAA; 16]);
        let old_cursor = EventCursor::new(old_epoch, EventSequence::new(2));
        let mut runtime = ProjectionRuntime::new(initial, 4, 4)?;
        let observed = ObservedState::new(1, 2, 3, 0);
        runtime.publish(PresentationDelta::ObservedReplaced(observed))?;
        assert!(matches!(
            runtime.watch_after(Some(old_cursor))?,
            ApplicationWatch::FullSnapshot(_)
        ));
        Ok(())
    }

    #[test]
    fn subscribe_live_delivers_published_events() -> Result<(), Box<dyn std::error::Error>> {
        let initial = snapshot()?;
        let mut runtime = ProjectionRuntime::new(initial, 4, 4)?;
        let mut rx = runtime.subscribe_live();
        let observed = ObservedState::new(1, 2, 3, 0);
        let _cursor = runtime.publish(PresentationDelta::ObservedReplaced(observed))?;
        let event = rx.blocking_recv().map_err(|_| "no live event")?;
        assert_eq!(event.cursor.sequence(), EventSequence::new(1));
        Ok(())
    }

    #[test]
    fn publish_and_snapshot_stay_within_perf_bound() -> Result<(), Box<dyn std::error::Error>> {
        // Regression guard for the daemon's hot path: publishing projections and
        // reading the snapshot must stay fast. Uses a generous bound so slow CI
        // does not flake while still catching pathological regressions.
        let initial = snapshot()?;
        let mut runtime = ProjectionRuntime::new(initial, 1024, 1024)?;
        let iterations = 10_000;
        let started = std::time::Instant::now();
        for i in 0..iterations {
            let value = u64::try_from(i).unwrap_or(u64::MAX);
            runtime.publish(PresentationDelta::ObservedReplaced(ObservedState::new(
                value, value, 1, 0,
            )))?;
        }
        for _ in 0..iterations {
            runtime.current_snapshot()?;
        }
        let elapsed = started.elapsed();
        // ~20k bounded operations must complete well under 1s; 5s is a safe CI bound.
        assert!(
            elapsed.as_secs() < 5,
            "projection publish+snapshot too slow: {elapsed:?}"
        );
        Ok(())
    }
}
