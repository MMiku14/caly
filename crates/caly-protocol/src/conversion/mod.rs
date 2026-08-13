//! Strict conversion between transport DTOs and pure Domain values.

mod encode;
mod error;
mod event;
mod identity;
mod operation;
mod snapshot;

pub use encode::{
    applied_to_wire, capabilities_to_wire, desired_to_wire, nodes_to_wire, observed_to_wire,
    operation_status_to_wire, platform_to_wire, proxy_groups_to_wire, snapshot_to_wire,
    EncodeError,
};
pub use error::DecodeError;
pub use event::{event_from_wire, DecodedProjectionEvent};
pub use identity::{cursor_from_wire, cursor_to_wire, daemon_id_from_wire, operation_id_from_wire};
pub use operation::operation_status_from_wire;
pub use snapshot::{
    applied_from_wire, capabilities_from_wire, desired_from_wire, nodes_from_wire,
    platform_from_wire, snapshot_from_wire,
};
