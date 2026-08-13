//! Version and feature negotiation values.

use caly_domain::BoundedVec;

use super::{DecodeLimits, WireId};

/// Maximum feature discriminants carried in one handshake.
pub const MAX_HANDSHAKE_FEATURES: usize = 64;
/// Capacity-enforced raw feature list.
pub type FeatureList = BoundedVec<RawFeature, MAX_HANDSHAKE_FEATURES>;

/// Protocol version negotiated before any other RPC.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    /// First caly v2 contract.
    pub const V2_0: Self = Self { major: 2, minor: 0 };

    /// Negotiates equal major and the lower minor version.
    pub const fn negotiate(self, peer: Self) -> Option<Self> {
        if self.major != peer.major {
            return None;
        }
        Some(Self {
            major: self.major,
            minor: if self.minor < peer.minor {
                self.minor
            } else {
                peer.minor
            },
        })
    }
}

/// Features known to this source version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Feature {
    Operations,
    OperationCancellation,
    EventReplay,
    FullSnapshotRecovery,
    UnknownEnumPreservation,
}

impl Feature {
    /// Strict numeric representation used on the wire.
    pub const fn raw(self) -> i32 {
        match self {
            Self::Operations => 1,
            Self::OperationCancellation => 2,
            Self::EventReplay => 3,
            Self::FullSnapshotRecovery => 4,
            Self::UnknownEnumPreservation => 5,
        }
    }
}

/// Raw feature value retained when the local version does not recognize it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RawFeature(pub i32);

impl RawFeature {
    /// Converts known values without fabricating a default.
    pub const fn known(self) -> Option<Feature> {
        match self.0 {
            1 => Some(Feature::Operations),
            2 => Some(Feature::OperationCancellation),
            3 => Some(Feature::EventReplay),
            4 => Some(Feature::FullSnapshotRecovery),
            5 => Some(Feature::UnknownEnumPreservation),
            _ => None,
        }
    }
}

/// Initial client negotiation request.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HandshakeRequest {
    pub client_version: ProtocolVersion,
    pub requested_features: FeatureList,
    /// Optional admission token presented to a daemon whose
    /// `daemon.auth_token` demands one (wire-compatible: older
    /// clients simply omit the field and fail admission against a
    /// token-enforcing daemon, which is the intended fail-closed
    /// behaviour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
}

/// The complete feature set implemented by this protocol source version.
///
/// Both the daemon (to advertise what it supports) and thin clients (to request
/// it) derive from this single canonical list, so a handshake never understates
/// real capabilities.
pub fn all_features() -> FeatureList {
    let mut list = FeatureList::new();
    for feature in [
        Feature::Operations,
        Feature::OperationCancellation,
        Feature::EventReplay,
        Feature::FullSnapshotRecovery,
        Feature::UnknownEnumPreservation,
    ] {
        let _ = list.try_push(RawFeature(feature.raw()));
    }
    list
}

/// Daemon negotiation result.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HandshakeResponse {
    pub negotiated_version: ProtocolVersion,
    #[serde(with = "crate::json_serde::hex_id")]
    pub daemon_instance_id: WireId,
    pub enabled_features: FeatureList,
    pub unknown_requested_features: FeatureList,
    pub limits: DecodeLimits,
    #[serde(with = "crate::json_serde::hex_id")]
    pub session_token: WireId,
}
