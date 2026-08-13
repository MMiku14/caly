//! Identity and cursor conversion.

use caly_domain::{DaemonInstanceId, EventCursor, EventSequence, OperationId};

use crate::protocol::v2::{WireEventCursor, WireId};

/// Converts fixed wire bytes without parsing strings or accepting truncation.
pub const fn daemon_id_from_wire(value: WireId) -> DaemonInstanceId {
    DaemonInstanceId::from_bytes(value)
}

/// Converts fixed wire bytes to an operation identity.
pub const fn operation_id_from_wire(value: WireId) -> OperationId {
    OperationId::from_bytes(value)
}

/// Converts an epoch-aware cursor.
pub const fn cursor_from_wire(value: WireEventCursor) -> EventCursor {
    EventCursor::new(
        DaemonInstanceId::from_bytes(value.daemon_instance_id),
        EventSequence::new(value.sequence),
    )
}

/// Converts an epoch-aware cursor to wire representation.
pub const fn cursor_to_wire(value: EventCursor) -> WireEventCursor {
    WireEventCursor {
        daemon_instance_id: value.daemon_instance().into_bytes(),
        sequence: value.sequence().value(),
    }
}
