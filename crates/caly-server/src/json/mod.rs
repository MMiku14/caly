//! Daemon-side protocol services over the JSON-framed local transport.

mod adapter;
mod command;
mod error;
mod event;
mod service;
mod service_v2;
mod session;

pub use adapter::ServiceAdapter;
pub use command::{CommandDecodeError, command_from_wire};
pub use error::service_error;
pub use event::event_to_wire;
pub use service::{JsonService, WatchStream};
pub use service_v2::{ServiceError, ServiceV2};
pub use session::{
    MAX_PROTOCOL_SESSIONS, ProtocolSession, SESSION_TTL_MS, SessionError, SessionRegistry,
    parse_session_token, unix_millis,
};
