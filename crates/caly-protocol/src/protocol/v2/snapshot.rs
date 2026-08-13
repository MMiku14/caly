//! Typed snapshot wire projection with raw enum preservation.

use caly_domain::BoundedVec;

use super::{WireEventCursor, WireId};

/// Maximum nodes accepted in one wire snapshot.
pub const MAX_WIRE_NODES: usize = 10_000;
/// Maximum capability assessments accepted in one wire snapshot.
pub const MAX_WIRE_CAPABILITIES: usize = 64;
/// W3b: sane ceiling for kernel proxy groups (Selector/URLTest/…).
pub const MAX_WIRE_PROXY_GROUPS: usize = 256;

/// Requests the current immutable presentation snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetSnapshotRequest;

/// Complete typed presentation snapshot envelope.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WirePresentationSnapshot {
    #[serde(with = "crate::json_serde::hex_id")]
    pub daemon_instance_id: WireId,
    pub revision: u64,
    pub cursor: WireEventCursor,
    pub desired: WireDesiredState,
    pub applied: WireAppliedState,
    pub observed: WireObservedState,
    pub platform: WirePlatformEffect,
    pub capabilities: BoundedVec<WireCapabilityStatus, MAX_WIRE_CAPABILITIES>,
    pub nodes: BoundedVec<WireDisplayNode, MAX_WIRE_NODES>,
    pub proxy_groups: BoundedVec<WireProxyGroup, MAX_WIRE_PROXY_GROUPS>,
}

/// Persisted intent projection using raw enum values.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WireDesiredState {
    pub mode: i32,
    #[serde(with = "crate::json_serde::hex_id_opt")]
    pub selected_node_id: Option<WireId>,
    #[serde(with = "crate::json_serde::hex_id_opt")]
    pub active_subscription_id: Option<WireId>,
    pub tun_requested: bool,
    pub system_proxy_requested: bool,
}

/// Runtime-applied projection using raw enum values.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WireAppliedState {
    pub core_kind: Option<i32>,
    pub run_state: i32,
    #[serde(with = "crate::json_serde::hex_id_opt")]
    pub selected_node_id: Option<WireId>,
    pub config_generation: Option<u64>,
}

/// Runtime observations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WireObservedState {
    pub upload_bytes_per_second: u64,
    pub download_bytes_per_second: u64,
    pub active_connections: u32,
    pub telemetry_dropped: u64,
    pub core_restart_count: u64,
    pub core_restart_backoff_ms: u64,
}

/// Redacted platform side-effect projection.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WirePlatformEffect {
    pub proxy_engaged: bool,
    pub tun_engaged: bool,
    pub recovery_pending: bool,
    pub degraded_reason: Option<String>,
}

/// Configured/runtime capability assessment with raw enums.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WireCapabilityStatus {
    pub capability: i32,
    pub configured: i32,
    pub runtime: i32,
    pub caveat: Option<String>,
}

/// Credential-free node projection.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WireDisplayNode {
    #[serde(with = "crate::json_serde::hex_id")]
    pub node_id: WireId,
    pub name: String,
    pub protocol: String,
    pub available: bool,
    pub latency_ms: Option<u32>,
}

/// W3b enrichment: a proxy group as seen by the kernel (kind,
/// kernel-side membership, current selection).
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WireProxyGroup {
    pub name: String,
    /// Kernel group kind: `Selector`, `URLTest`, `Fallback`, `LoadBalance`.
    pub kind: String,
    pub selected: Option<String>,
    pub members: Vec<String>,
}
