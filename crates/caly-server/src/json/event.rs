//! Application event to typed wire-event encoding.

use caly_application::events::{ApplicationEvent, SequencedEvent};
use caly_protocol::{
    conversion::{
        EncodeError, applied_to_wire, capabilities_to_wire, cursor_to_wire, desired_to_wire,
        nodes_to_wire, observed_to_wire, platform_to_wire,
    },
    protocol::v2::{WireEvent, WireProjectionEvent, WireProxyGroup},
};

/// Encodes one already-sequenced projection event.
pub fn event_to_wire(value: &SequencedEvent) -> Result<WireEvent, EncodeError> {
    let event = match &value.event {
        ApplicationEvent::DesiredReplaced(state) => {
            WireProjectionEvent::DesiredReplaced(desired_to_wire(state))
        }
        ApplicationEvent::AppliedReplaced(state) => {
            WireProjectionEvent::AppliedReplaced(applied_to_wire(state))
        }
        ApplicationEvent::ObservedReplaced(state) => {
            WireProjectionEvent::ObservedReplaced(observed_to_wire(state))
        }
        ApplicationEvent::PlatformReplaced(state) => {
            WireProjectionEvent::PlatformReplaced(platform_to_wire(state))
        }
        ApplicationEvent::CapabilitiesReplaced(state) => {
            WireProjectionEvent::CapabilitiesReplaced(capabilities_to_wire(state)?)
        }
        ApplicationEvent::NodesReplaced(state) => {
            WireProjectionEvent::NodesReplaced(nodes_to_wire(state)?)
        }
        ApplicationEvent::GroupsReplaced(groups) => {
            let mut wire = caly_domain::BoundedVec::new();
            for group in groups {
                let _ = wire.try_extend(vec![WireProxyGroup {
                    name: group.name.clone(),
                    kind: group.kind.clone(),
                    selected: group.selected.clone(),
                    members: group.members.clone(),
                }]);
            }
            WireProjectionEvent::GroupsReplaced(wire)
        }
    };
    Ok(WireEvent {
        cursor: cursor_to_wire(value.cursor),
        event,
    })
}
