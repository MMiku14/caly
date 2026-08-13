//! Single-owner actor request boundaries.
//!
//! Port traits, the shared error type, and the config port types are defined in
//! `caly-ports` and re-exported here so application code (handlers, coordinators)
//! and the concrete `caly-backends` adapters share one contract without knowing
//! each other's concrete types.

mod config_command_handler;
mod core_handler;
mod lifecycle_handler;
mod platform_command;
mod platform_handler;
mod reporting;
mod subscription_command;
mod subscription_handler;
mod telemetry_command;
mod telemetry_handler;

pub use config_command_handler::{ConfigCommandHandler, ConfigCommandHandlerError};
pub use core_handler::{CoreCommandHandler, CoreHandlerError};
pub use lifecycle_handler::{CoreLifecycleCommandHandler, CoreLifecycleHandlerError};
pub use platform_command::PlatformActorCommand;
pub use platform_handler::{PlatformCommandHandler, PlatformHandlerError};
pub use subscription_command::SubscriptionActorCommand;
pub use subscription_handler::{SubscriptionCommandHandler, SubscriptionHandlerError};
pub use telemetry_command::TelemetryActorCommand;
pub use telemetry_handler::TelemetryCommandHandler;

// Port traits and shared error come from the `caly-ports`
// crate (the round-32 attempt to inline them into
// `crate::ports` was reverted because `caly-backends`
// prod-deps the port traits and the backends crate is
// itself prod-deped by this crate, which would form a
// workspace-level cycle).
pub use caly_ports::{
    ActorFailure, ActorFailureKind, CommittedConfig, ConfigActorPort, ConfigCandidate,
    CoreCommandBackend, CoreLifecycleCommandBackend, FailureMessage, PlatformCommandBackend,
    PreparedConfig, SubscriptionCommandBackend, TelemetryCommandBackend, TunCommandBackend,
};
