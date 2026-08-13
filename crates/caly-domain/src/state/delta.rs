//! Credential-free presentation deltas shared by Application and clients.

use super::presentation::ProxyGroupView;
use super::{AppliedState, DesiredState, ObservedState, PlatformEffectView, SnapshotNodes};
use crate::CapabilitySet;

/// One complete replacement of a presentation-state slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PresentationDelta {
    DesiredReplaced(DesiredState),
    AppliedReplaced(AppliedState),
    ObservedReplaced(ObservedState),
    PlatformReplaced(PlatformEffectView),
    CapabilitiesReplaced(CapabilitySet),
    NodesReplaced(SnapshotNodes),
    /// W3b enrichment: the proxy-group slice (kind/members/selection),
    /// published at boot from the declared config and refreshed after
    /// every group-selection command.
    GroupsReplaced(Vec<ProxyGroupView>),
}
