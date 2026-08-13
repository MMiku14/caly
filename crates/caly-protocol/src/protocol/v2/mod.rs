//! Protocol v2 transport-neutral DTOs.

use caly_domain::BoundedVec;

mod budget;
mod handshake;
mod negotiation;
mod operation;
mod snapshot;
mod watch;
mod wire;

pub use budget::{BudgetError, CollectionKind, DecodeBudget, DecodeLimits};
pub use handshake::{
    Feature, FeatureList, HandshakeRequest, HandshakeResponse, ProtocolVersion, RawFeature,
    all_features,
};
pub use negotiation::{HandshakeError, negotiate_handshake};
pub use operation::{
    CancelOperationRequest, CancelOperationResponse, CancelOutcome, CommandKind, ExecuteRequest,
    ExecuteResponse, GetOperationStatusRequest, RawCancelOutcome, RawCommandKind, RawFailureCode,
    RawOperationState, WireCommand, WireOperationFailure, WireOperationStatus,
};
pub use snapshot::{
    GetSnapshotRequest, MAX_WIRE_PROXY_GROUPS, WireAppliedState, WireCapabilityStatus,
    WireDesiredState, WireDisplayNode, WireObservedState, WirePlatformEffect,
    WirePresentationSnapshot, WireProxyGroup,
};
pub use watch::{WatchEventsRequest, WatchResponse, WireEvent, WireProjectionEvent};
pub use wire::{WireCoreAction, WireCoreKind, WireMode, WireOperationState, WireRunState};

/// Maximum opaque payload accepted by a typed v2 DTO.
pub const MAX_WIRE_PAYLOAD_BYTES: usize = 16 * 1_024 * 1_024;
/// Capacity-enforced opaque payload.
pub type WirePayload = BoundedVec<u8, MAX_WIRE_PAYLOAD_BYTES>;
/// Opaque wire representation of a 128-bit identity.
pub type WireId = [u8; 16];

/// Epoch-aware wire cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WireEventCursor {
    /// Daemon epoch bytes.
    #[serde(with = "crate::json_serde::hex_id")]
    pub daemon_instance_id: WireId,
    /// Sequence within the daemon epoch.
    pub sequence: u64,
}
