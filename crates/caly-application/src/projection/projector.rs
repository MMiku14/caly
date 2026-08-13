//! Immutable presentation snapshot projector.

use caly_domain::{
    AppliedState, CapabilitySet, DaemonInstanceId, DesiredState, EventCursor, ObservedState,
    PlatformEffectView, PresentationSnapshot, SnapshotNodes, SnapshotRevision,
};

use crate::events::{ApplicationEvent, SequencedEvent};

/// Projector rejection requiring ordered recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionError {
    EpochMismatch,
    NonContiguousEvent,
    RevisionExhausted,
}

/// Single-owner projection of reliable ordered events.
pub struct StateProjector {
    daemon_instance: DaemonInstanceId,
    cursor: EventCursor,
    revision: SnapshotRevision,
    desired: DesiredState,
    applied: AppliedState,
    observed: ObservedState,
    platform: PlatformEffectView,
    capabilities: CapabilitySet,
    nodes: SnapshotNodes,
    proxy_groups: Vec<caly_domain::ProxyGroupView>,
}

impl StateProjector {
    /// Starts from an authoritative coherent snapshot.
    pub fn from_snapshot(snapshot: PresentationSnapshot) -> Self {
        Self {
            daemon_instance: snapshot.daemon_instance(),
            cursor: snapshot.cursor(),
            revision: snapshot.revision(),
            desired: snapshot.desired().clone(),
            applied: snapshot.applied().clone(),
            observed: *snapshot.observed(),
            platform: snapshot.platform().clone(),
            capabilities: snapshot.capabilities().clone(),
            nodes: snapshot.nodes().clone(),
            proxy_groups: snapshot.proxy_groups().to_vec(),
        }
    }

    /// Applies exactly one contiguous event.
    pub fn apply(&mut self, sequenced: SequencedEvent) -> Result<(), ProjectionError> {
        if sequenced.cursor.daemon_instance() != self.daemon_instance {
            return Err(ProjectionError::EpochMismatch);
        }
        let expected = self
            .cursor
            .sequence()
            .checked_next()
            .map_err(|_| ProjectionError::RevisionExhausted)?;
        if sequenced.cursor.sequence() != expected {
            return Err(ProjectionError::NonContiguousEvent);
        }
        self.replace(sequenced.event);
        self.cursor = sequenced.cursor;
        let revision = self
            .revision
            .value()
            .checked_add(1)
            .ok_or(ProjectionError::RevisionExhausted)?;
        self.revision = SnapshotRevision::new(revision);
        Ok(())
    }

    /// Returns the current immutable redacted snapshot.
    pub fn snapshot(&self) -> PresentationSnapshot {
        PresentationSnapshot::new(
            self.daemon_instance,
            self.revision,
            self.cursor,
            self.desired.clone(),
            self.applied.clone(),
            self.observed,
            self.platform.clone(),
            self.capabilities.clone(),
            self.nodes.clone(),
        )
        .with_proxy_groups(self.proxy_groups.clone())
    }

    fn replace(&mut self, event: ApplicationEvent) {
        match event {
            ApplicationEvent::DesiredReplaced(value) => self.desired = value,
            ApplicationEvent::AppliedReplaced(value) => self.applied = value,
            ApplicationEvent::ObservedReplaced(value) => self.observed = value,
            ApplicationEvent::PlatformReplaced(value) => self.platform = value,
            ApplicationEvent::CapabilitiesReplaced(value) => self.capabilities = value,
            ApplicationEvent::NodesReplaced(value) => self.nodes = value,
            ApplicationEvent::GroupsReplaced(groups) => self.proxy_groups = groups,
        }
    }
}
