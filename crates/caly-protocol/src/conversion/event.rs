//! Strict typed projection-event conversion with unknown preservation.

use caly_domain::{EventCursor, ObservedState, PresentationDelta};

use crate::protocol::v2::{DecodeBudget, WireEvent, WirePayload, WireProjectionEvent};

use super::{
    DecodeError, applied_from_wire, capabilities_from_wire, cursor_from_wire, desired_from_wire,
    nodes_from_wire, platform_from_wire,
};

/// Known Domain delta or preserved future event.
pub enum DecodedProjectionEvent {
    Known {
        cursor: EventCursor,
        delta: PresentationDelta,
    },
    Unknown {
        cursor: EventCursor,
        raw_kind: i32,
        payload: WirePayload,
    },
}

/// Converts known events strictly and preserves unknown payloads.
pub fn event_from_wire(
    wire: WireEvent,
    budget: &mut DecodeBudget,
) -> Result<DecodedProjectionEvent, DecodeError> {
    let cursor = cursor_from_wire(wire.cursor);
    let delta = match wire.event {
        WireProjectionEvent::DesiredReplaced(value) => {
            PresentationDelta::DesiredReplaced(desired_from_wire(value)?)
        }
        WireProjectionEvent::AppliedReplaced(value) => {
            PresentationDelta::AppliedReplaced(applied_from_wire(value)?)
        }
        WireProjectionEvent::ObservedReplaced(value) => {
            PresentationDelta::ObservedReplaced(ObservedState::with_self_heal(
                value.upload_bytes_per_second,
                value.download_bytes_per_second,
                value.active_connections,
                value.telemetry_dropped,
                value.core_restart_count,
                value.core_restart_backoff_ms,
            ))
        }
        WireProjectionEvent::PlatformReplaced(value) => {
            PresentationDelta::PlatformReplaced(platform_from_wire(value, budget)?)
        }
        WireProjectionEvent::CapabilitiesReplaced(values) => {
            PresentationDelta::CapabilitiesReplaced(capabilities_from_wire(
                values.into_vec(),
                budget,
            )?)
        }
        WireProjectionEvent::NodesReplaced(values) => {
            PresentationDelta::NodesReplaced(nodes_from_wire(values.into_vec(), budget)?)
        }
        WireProjectionEvent::GroupsReplaced(values) => {
            let groups = values
                .into_vec()
                .into_iter()
                .map(|g| caly_domain::ProxyGroupView {
                    name: g.name,
                    kind: g.kind,
                    selected: g.selected,
                    members: g.members,
                })
                .collect();
            PresentationDelta::GroupsReplaced(groups)
        }
        WireProjectionEvent::Unknown { raw_kind, payload } => {
            budget.charge_bytes(payload.len())?;
            return Ok(DecodedProjectionEvent::Unknown {
                cursor,
                raw_kind,
                payload,
            });
        }
    };
    Ok(DecodedProjectionEvent::Known { cursor, delta })
}
