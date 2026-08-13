//! Replay and full-snapshot recovery stream values.

use caly_domain::BoundedVec;

use super::{
    MAX_WIRE_PROXY_GROUPS, WireAppliedState, WireCapabilityStatus, WireDesiredState,
    WireDisplayNode, WireEventCursor, WireObservedState, WirePayload, WirePlatformEffect,
    WirePresentationSnapshot, WireProxyGroup,
};

/// Starts live delivery after an optional last-applied cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WatchEventsRequest {
    pub after: Option<WireEventCursor>,
}

/// Typed projection delta or preserved unknown future event.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WireProjectionEvent {
    DesiredReplaced(WireDesiredState),
    AppliedReplaced(WireAppliedState),
    ObservedReplaced(WireObservedState),
    PlatformReplaced(WirePlatformEffect),
    CapabilitiesReplaced(BoundedVec<WireCapabilityStatus, 64>),
    NodesReplaced(BoundedVec<WireDisplayNode, 10_000>),
    /// W3b: the kernel proxy-group slice after a selection refresh.
    GroupsReplaced(BoundedVec<WireProxyGroup, MAX_WIRE_PROXY_GROUPS>),
    Unknown {
        raw_kind: i32,
        payload: WirePayload,
    },
}

/// Ordered event payload.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WireEvent {
    pub cursor: WireEventCursor,
    pub event: WireProjectionEvent,
}

/// Watch stream recovery contract.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WatchResponse {
    /// One ordered event.
    Event(WireEvent),
    /// Authoritative replacement after epoch mismatch or replay gap
    /// (boxed: the full snapshot is the largest wire payload by far).
    FullSnapshot(Box<WirePresentationSnapshot>),
}
