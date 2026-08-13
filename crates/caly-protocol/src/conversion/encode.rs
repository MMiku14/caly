//! Strict Domain-to-wire encoding without credentials or lossy defaults.

use caly_domain::{
    BoundedVec, Capability, ConfiguredSupport, CoreKind, CoreRunState, OperationFailureCode,
    OperationState, PresentationSnapshot, ProxyMode, RuntimeAvailability,
};

use crate::protocol::v2::{
    RawFailureCode, RawOperationState, WireAppliedState, WireCapabilityStatus, WireDesiredState,
    WireDisplayNode, WireObservedState, WireOperationFailure, WireOperationStatus,
    WirePlatformEffect, WirePresentationSnapshot, WireProxyGroup, MAX_WIRE_PROXY_GROUPS,
};

use super::cursor_to_wire;

/// Encoding failure means a Domain/Protocol bound changed without migration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EncodeError(pub caly_domain::CapacityError);

/// Encodes a credential-free presentation snapshot.
pub fn snapshot_to_wire(
    snapshot: &PresentationSnapshot,
) -> Result<WirePresentationSnapshot, EncodeError> {
    let capabilities = capabilities_to_wire(snapshot.capabilities())?;
    let nodes = nodes_to_wire(snapshot.nodes())?;
    let proxy_groups = proxy_groups_to_wire(snapshot.proxy_groups());
    Ok(WirePresentationSnapshot {
        daemon_instance_id: snapshot.daemon_instance().into_bytes(),
        revision: snapshot.revision().value(),
        cursor: cursor_to_wire(snapshot.cursor()),
        desired: desired_to_wire(snapshot.desired()),
        applied: applied_to_wire(snapshot.applied()),
        observed: observed_to_wire(snapshot.observed()),
        platform: platform_to_wire(snapshot.platform()),
        capabilities,
        nodes,
        proxy_groups,
    })
}

/// W3b: the kernel proxy-group slice, mapped 1:1 onto the wire shape.
/// Over-long slices clamp at the wire bound (same discipline as nodes).
pub fn proxy_groups_to_wire(
    groups: &[caly_domain::ProxyGroupView],
) -> BoundedVec<WireProxyGroup, MAX_WIRE_PROXY_GROUPS> {
    let mut wire = BoundedVec::new();
    for group in groups {
        let _ = wire.try_extend(vec![WireProxyGroup {
            name: group.name.clone(),
            kind: group.kind.clone(),
            selected: group.selected.clone(),
            members: group.members.clone(),
        }]);
    }
    wire
}

/// Encodes queryable operation status.
pub fn operation_status_to_wire(status: &caly_domain::OperationStatus) -> WireOperationStatus {
    WireOperationStatus {
        operation_id: status.id().into_bytes(),
        command_kind: status.kind().as_str().to_owned(),
        created_at_unix_ms: status.created_at().value(),
        updated_at_unix_ms: status.updated_at().value(),
        state: RawOperationState(operation_state(status.state())),
        failure: status.failure().map(|failure| WireOperationFailure {
            code: RawFailureCode(failure_code(failure.code())),
            message: failure.message().as_str().to_owned(),
            suggested_action: failure.suggested_action().as_str().to_owned(),
        }),
    }
}

/// Encodes persisted intent.
pub fn desired_to_wire(value: &caly_domain::DesiredState) -> WireDesiredState {
    WireDesiredState {
        mode: proxy_mode(value.mode()),
        selected_node_id: value.selected_node().map(caly_domain::NodeId::into_bytes),
        active_subscription_id: value
            .active_subscription()
            .map(caly_domain::SubscriptionId::into_bytes),
        tun_requested: value.tun_requested(),
        system_proxy_requested: value.system_proxy_requested(),
    }
}

/// Encodes runtime-applied state.
pub fn applied_to_wire(value: &caly_domain::AppliedState) -> WireAppliedState {
    WireAppliedState {
        core_kind: value.core().map(core_kind),
        run_state: run_state(value.run_state()),
        selected_node_id: value.selected_node().map(caly_domain::NodeId::into_bytes),
        config_generation: value.config_generation(),
    }
}

/// Encodes observed runtime counters.
pub fn observed_to_wire(value: &caly_domain::ObservedState) -> WireObservedState {
    WireObservedState {
        upload_bytes_per_second: value.upload_bytes_per_second(),
        download_bytes_per_second: value.download_bytes_per_second(),
        active_connections: value.active_connections(),
        telemetry_dropped: value.telemetry_dropped(),
        core_restart_count: value.core_restart_count(),
        core_restart_backoff_ms: value.core_restart_backoff_ms(),
    }
}

/// Encodes redacted platform effects.
pub fn platform_to_wire(value: &caly_domain::PlatformEffectView) -> WirePlatformEffect {
    WirePlatformEffect {
        proxy_engaged: value.proxy_engaged(),
        tun_engaged: value.tun_engaged(),
        recovery_pending: value.recovery_pending(),
        degraded_reason: value
            .degraded_reason()
            .map(|reason| reason.as_str().to_owned()),
    }
}

fn capability_to_wire(value: &caly_domain::CapabilityStatus) -> WireCapabilityStatus {
    WireCapabilityStatus {
        capability: capability(value.capability()),
        configured: configured(value.configured()),
        runtime: runtime(value.runtime()),
        caveat: value.caveat().map(|text| text.as_str().to_owned()),
    }
}

/// Encodes a bounded capability set.
pub fn capabilities_to_wire(
    values: &caly_domain::CapabilitySet,
) -> Result<caly_domain::BoundedVec<WireCapabilityStatus, 64>, EncodeError> {
    let converted = values.iter().map(capability_to_wire).collect();
    caly_domain::BoundedVec::try_from_vec(converted).map_err(EncodeError)
}

/// Encodes bounded credential-free display nodes.
pub fn nodes_to_wire(
    values: &caly_domain::SnapshotNodes,
) -> Result<caly_domain::BoundedVec<WireDisplayNode, 10_000>, EncodeError> {
    let converted = values
        .iter()
        .map(|node| WireDisplayNode {
            node_id: node.id().into_bytes(),
            name: node.name().as_str().to_owned(),
            protocol: node.protocol().as_str().to_owned(),
            available: node.is_available(),
            latency_ms: node.latency_ms(),
        })
        .collect();
    caly_domain::BoundedVec::try_from_vec(converted).map_err(EncodeError)
}

fn proxy_mode(value: ProxyMode) -> i32 {
    crate::protocol::v2::WireMode::from(value).wire()
}
fn core_kind(value: CoreKind) -> i32 {
    crate::protocol::v2::WireCoreKind::from(value).wire()
}
fn run_state(value: CoreRunState) -> i32 {
    crate::protocol::v2::WireRunState::from(value).wire()
}
fn operation_state(value: OperationState) -> i32 {
    crate::protocol::v2::WireOperationState::from(value).wire()
}
const fn failure_code(value: OperationFailureCode) -> i32 {
    match value {
        OperationFailureCode::InvalidInput => 1,
        OperationFailureCode::Conflict => 2,
        OperationFailureCode::ResourceExhausted => 3,
        OperationFailureCode::Unsupported => 4,
        OperationFailureCode::DeadlineExceeded => 5,
        OperationFailureCode::TooLateToCancel => 6,
        OperationFailureCode::Infrastructure => 7,
        OperationFailureCode::RecoveryRequired => 8,
    }
}
const fn configured(value: ConfiguredSupport) -> i32 {
    match value {
        ConfiguredSupport::Unsupported => 1,
        ConfiguredSupport::Supported => 2,
        ConfiguredSupport::Partial => 3,
    }
}
const fn runtime(value: RuntimeAvailability) -> i32 {
    match value {
        RuntimeAvailability::NotRequired => 1,
        RuntimeAvailability::Available => 2,
        RuntimeAvailability::Unavailable => 3,
        RuntimeAvailability::Unknown => 4,
    }
}
const fn capability(value: Capability) -> i32 {
    match value {
        Capability::TunConfiguration => 1,
        Capability::DnsConfiguration => 2,
        Capability::RuntimeModeSwitch => 3,
        Capability::ProxyGroups => 4,
        Capability::ProxySelection => 5,
        Capability::UrlTest => 6,
        Capability::Connections => 7,
        Capability::ConnectionClose => 8,
        Capability::Traffic => 9,
        Capability::Logs => 10,
        Capability::Rules => 11,
    }
}
