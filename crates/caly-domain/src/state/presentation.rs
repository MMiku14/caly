//! Immutable redacted state consumed by thin clients.

use crate::{BoundedVec, CapabilitySet, DaemonInstanceId, DisplayNode, EventCursor};

/// Maximum display nodes in one presentation snapshot.
pub const MAX_SNAPSHOT_NODES: usize = 10_000;
/// Capacity-enforced display node projection.
pub type SnapshotNodes = BoundedVec<DisplayNode, MAX_SNAPSHOT_NODES>;

use super::{AppliedState, DesiredState, ObservedState, PlatformEffectView};

/// Monotonic presentation publication revision within one daemon epoch.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SnapshotRevision(u64);

impl SnapshotRevision {
    /// Constructs a snapshot revision from a trusted projector boundary.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    /// Returns the primitive revision value.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// A proxy group as projected for the CLI/TUI (W3b enrichment): the group
/// kind, its kernel-side membership, and the currently selected member.
/// Seeded from the declared config at boot, refreshed from the kernel
/// (`GET /proxies`) after every group selection command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyGroupView {
    pub name: String,
    /// Kernel group kind: `Selector`, `URLTest`, `Fallback`, `LoadBalance`.
    pub kind: String,
    pub selected: Option<String>,
    pub members: Vec<String>,
}

/// Immutable, redacted projection consumed by TUI and CLI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentationSnapshot {
    daemon_instance: DaemonInstanceId,
    revision: SnapshotRevision,
    cursor: EventCursor,
    desired: DesiredState,
    applied: AppliedState,
    observed: ObservedState,
    platform: PlatformEffectView,
    capabilities: CapabilitySet,
    nodes: SnapshotNodes,
    proxy_groups: Vec<ProxyGroupView>,
}

impl PresentationSnapshot {
    /// Constructs a coherent snapshot from projector-owned slices.
    pub const fn new(
        daemon_instance: DaemonInstanceId,
        revision: SnapshotRevision,
        cursor: EventCursor,
        desired: DesiredState,
        applied: AppliedState,
        observed: ObservedState,
        platform: PlatformEffectView,
        capabilities: CapabilitySet,
        nodes: SnapshotNodes,
    ) -> Self {
        Self {
            daemon_instance,
            revision,
            cursor,
            desired,
            applied,
            observed,
            platform,
            capabilities,
            nodes,
            proxy_groups: Vec::new(),
        }
    }

    /// Returns the daemon epoch that produced this snapshot.
    pub const fn daemon_instance(&self) -> DaemonInstanceId {
        self.daemon_instance
    }
    /// Returns the projector publication revision.
    pub const fn revision(&self) -> SnapshotRevision {
        self.revision
    }
    /// Returns the last event incorporated into the snapshot.
    pub const fn cursor(&self) -> EventCursor {
        self.cursor
    }
    /// Returns persisted user intent.
    pub const fn desired(&self) -> &DesiredState {
        &self.desired
    }
    /// Returns core-acknowledged state.
    pub const fn applied(&self) -> &AppliedState {
        &self.applied
    }
    /// Returns current observations.
    pub const fn observed(&self) -> &ObservedState {
        &self.observed
    }
    /// Returns redacted platform side-effect state.
    pub const fn platform(&self) -> &PlatformEffectView {
        &self.platform
    }
    /// Returns configured and runtime capability assessments.
    pub const fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }
    /// Returns bounded credential-free nodes.
    pub const fn nodes(&self) -> &SnapshotNodes {
        &self.nodes
    }

    /// Returns the proxy-group slice (W3b enrichment).
    pub fn proxy_groups(&self) -> &[ProxyGroupView] {
        &self.proxy_groups
    }

    /// Sets the proxy-group slice (projector-owned; not part of `new` so
    /// boot-time constructors stay untouched).
    #[must_use]
    pub fn with_proxy_groups(mut self, proxy_groups: Vec<ProxyGroupView>) -> Self {
        self.proxy_groups = proxy_groups;
        self
    }
}
