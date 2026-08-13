//! Strict typed snapshot conversion.

use caly_domain::{
    AppliedState, BoundedText, BoundedVec, Capability, CapabilitySet, CapabilityStatus,
    ConfiguredSupport, CoreKind, CoreRunState, DesiredState, DisplayNode, NodeId, ObservedState,
    PlatformEffectView, PresentationSnapshot, ProxyMode, RuntimeAvailability, SnapshotRevision,
    SubscriptionId,
};

use super::{cursor_from_wire, DecodeError};
use crate::protocol::v2::{
    CollectionKind, DecodeBudget, WireAppliedState, WireCapabilityStatus, WireDesiredState,
    WireDisplayNode, WirePlatformEffect, WirePresentationSnapshot,
};

/// Converts a complete snapshot or rejects the entire payload.
pub fn snapshot_from_wire(
    wire: WirePresentationSnapshot,
    budget: &mut DecodeBudget,
) -> Result<PresentationSnapshot, DecodeError> {
    budget.check_count(CollectionKind::Nodes, wire.nodes.len())?;
    let cursor = cursor_from_wire(wire.cursor);
    let daemon = caly_domain::DaemonInstanceId::from_bytes(wire.daemon_instance_id);
    if cursor.daemon_instance() != daemon {
        return Err(DecodeError::EpochMismatch);
    }
    let desired = desired_from_wire(wire.desired)?;
    let applied = applied_from_wire(wire.applied)?;
    let observed = ObservedState::with_self_heal(
        wire.observed.upload_bytes_per_second,
        wire.observed.download_bytes_per_second,
        wire.observed.active_connections,
        wire.observed.telemetry_dropped,
        wire.observed.core_restart_count,
        wire.observed.core_restart_backoff_ms,
    );
    let platform = platform_from_wire(wire.platform, budget)?;
    let capabilities = capabilities_from_wire(wire.capabilities.into_vec(), budget)?;
    let nodes = nodes_from_wire(wire.nodes.into_vec(), budget)?;
    Ok(PresentationSnapshot::new(
        daemon,
        SnapshotRevision::new(wire.revision),
        cursor,
        desired,
        applied,
        observed,
        platform,
        capabilities,
        nodes,
    ))
}

/// Strictly decodes persisted intent.
pub fn desired_from_wire(wire: WireDesiredState) -> Result<DesiredState, DecodeError> {
    Ok(DesiredState::new(
        proxy_mode(wire.mode)?,
        wire.selected_node_id.map(NodeId::from_bytes),
        wire.active_subscription_id.map(SubscriptionId::from_bytes),
        wire.tun_requested,
        wire.system_proxy_requested,
    ))
}

/// Strictly decodes runtime-applied state.
pub fn applied_from_wire(wire: WireAppliedState) -> Result<AppliedState, DecodeError> {
    let core = wire.core_kind.map(core_kind).transpose()?;
    AppliedState::new(
        core,
        run_state(wire.run_state)?,
        wire.selected_node_id.map(NodeId::from_bytes),
        wire.config_generation,
    )
    .map_err(|error| DecodeError::InvalidState {
        reason: error.to_string(),
    })
}

/// Strictly decodes redacted platform effects.
pub fn platform_from_wire(
    wire: WirePlatformEffect,
    budget: &mut DecodeBudget,
) -> Result<PlatformEffectView, DecodeError> {
    let reason = wire
        .degraded_reason
        .map(|value| bounded_text(value, "platform.degraded_reason", budget))
        .transpose()?;
    Ok(PlatformEffectView::new(
        wire.proxy_engaged,
        wire.tun_engaged,
        wire.recovery_pending,
        reason,
    ))
}

/// Strictly decodes bounded capability assessments.
pub fn capabilities_from_wire(
    values: Vec<WireCapabilityStatus>,
    budget: &mut DecodeBudget,
) -> Result<CapabilitySet, DecodeError> {
    let mut converted = Vec::with_capacity(values.len());
    for wire in values {
        let caveat = wire
            .caveat
            .map(|value| bounded_text(value, "capability.caveat", budget))
            .transpose()?;
        converted.push(CapabilityStatus::new(
            capability(wire.capability)?,
            configured(wire.configured)?,
            runtime(wire.runtime)?,
            caveat,
        ));
    }
    let bounded =
        BoundedVec::try_from_vec(converted).map_err(|error| DecodeError::InvalidState {
            reason: error.to_string(),
        })?;
    CapabilitySet::new(bounded).map_err(|error| DecodeError::InvalidState {
        reason: error.to_string(),
    })
}

/// Strictly decodes bounded display nodes.
pub fn nodes_from_wire(
    values: Vec<WireDisplayNode>,
    budget: &mut DecodeBudget,
) -> Result<caly_domain::SnapshotNodes, DecodeError> {
    let mut converted = Vec::with_capacity(values.len());
    for wire in values {
        converted.push(DisplayNode::new(
            NodeId::from_bytes(wire.node_id),
            bounded_text(wire.name, "node.name", budget)?,
            bounded_text(wire.protocol, "node.protocol", budget)?,
            wire.available,
            wire.latency_ms,
        ));
    }
    BoundedVec::try_from_vec(converted).map_err(|error| DecodeError::InvalidState {
        reason: error.to_string(),
    })
}

fn bounded_text<const MAX: usize>(
    value: String,
    field: &'static str,
    budget: &mut DecodeBudget,
) -> Result<BoundedText<MAX>, DecodeError> {
    budget.check_string(value.len())?;
    budget.charge_bytes(value.len())?;
    BoundedText::new(value).map_err(|error| DecodeError::InvalidText {
        field,
        reason: error.to_string(),
    })
}

fn proxy_mode(raw: i32) -> Result<ProxyMode, DecodeError> {
    use crate::protocol::v2::WireMode;
    WireMode::from_wire(raw)
        .map(ProxyMode::from)
        .ok_or_else(|| unknown_value("desired.mode", raw))
}

fn core_kind(raw: i32) -> Result<CoreKind, DecodeError> {
    use crate::protocol::v2::WireCoreKind;
    WireCoreKind::from_wire(raw)
        .map(CoreKind::from)
        .ok_or_else(|| unknown_value("applied.core_kind", raw))
}

fn run_state(raw: i32) -> Result<CoreRunState, DecodeError> {
    use crate::protocol::v2::WireRunState;
    WireRunState::from_wire(raw)
        .map(CoreRunState::from)
        .ok_or_else(|| unknown_value("applied.run_state", raw))
}

fn capability(raw: i32) -> Result<Capability, DecodeError> {
    match raw {
        1 => Ok(Capability::TunConfiguration),
        2 => Ok(Capability::DnsConfiguration),
        3 => Ok(Capability::RuntimeModeSwitch),
        4 => Ok(Capability::ProxyGroups),
        5 => Ok(Capability::ProxySelection),
        6 => Ok(Capability::UrlTest),
        7 => Ok(Capability::Connections),
        8 => Ok(Capability::ConnectionClose),
        9 => Ok(Capability::Traffic),
        10 => Ok(Capability::Logs),
        11 => Ok(Capability::Rules),
        value => unknown("capability.kind", value),
    }
}

fn configured(raw: i32) -> Result<ConfiguredSupport, DecodeError> {
    match raw {
        1 => Ok(ConfiguredSupport::Unsupported),
        2 => Ok(ConfiguredSupport::Supported),
        3 => Ok(ConfiguredSupport::Partial),
        value => unknown("capability.configured", value),
    }
}

fn runtime(raw: i32) -> Result<RuntimeAvailability, DecodeError> {
    match raw {
        1 => Ok(RuntimeAvailability::NotRequired),
        2 => Ok(RuntimeAvailability::Available),
        3 => Ok(RuntimeAvailability::Unavailable),
        4 => Ok(RuntimeAvailability::Unknown),
        value => unknown("capability.runtime", value),
    }
}

fn unknown<T>(field: &'static str, raw: i32) -> Result<T, DecodeError> {
    Err(DecodeError::UnknownEnum { field, raw })
}

fn unknown_value(field: &'static str, raw: i32) -> DecodeError {
    DecodeError::UnknownEnum { field, raw }
}
