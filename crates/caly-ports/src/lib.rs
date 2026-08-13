//! Shared actor ports and the common bounded error type.
//!
//! This crate sits at the bottom of the product layer (above `caly-domain`) and
//! defines the trait boundaries that owner actors depend on and that concrete
//! adapters implement. Neither the application use-case crate nor the backend
//! adapters need to know each other's concrete types.
//!
//! # Why this crate is a separate workspace member
//!
//! Round 32 considered inlining the ports into `caly-application` (treating
//! the trait boundaries as application-internal) and removing the workspace
//! member. The inlining was attempted and reverted: back then
//! `caly-application` prod-deps `caly-backends` (its composition subtree
//! consumed the concrete backend structs) while `caly-backends` prod-deps
//! the port traits here, so inlining would have formed a workspace
//! dependency cycle that `cargo` rejects at the metadata level. The only
//! viable shape was a third crate both sides depend on. This crate is that
//! third crate.
//!
//! P7 (docs/crate-replan.md) completed the decoupling the revert was
//! waiting for: the composition subtree moved to `caly-composition`, the two
//! coordination cells moved up here (`cells`), and `caly-application`'s
//! internal deps converged to `{caly-domain, caly-ports}`. The port crate
//! stays a separate workspace member because `caly-backends` still must not
//! depend on the use-case crate.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
pub mod cells;
pub mod config;
pub mod core;
pub mod error;
pub mod lifecycle;
pub mod platform;
pub mod subscription;
pub mod telemetry;
pub mod tun;

pub use cells::{SharedActiveCore, SharedDesiredState};
pub use config::{CommittedConfig, ConfigActorPort, ConfigCandidate, PreparedConfig};
pub use core::CoreCommandBackend;
pub use error::{ActorFailure, ActorFailureKind, FailureMessage};
pub use lifecycle::CoreLifecycleCommandBackend;
pub use platform::PlatformCommandBackend;
pub use subscription::{RefreshMode, RefreshOutcome, SubscriptionCommandBackend};
pub use telemetry::TelemetryCommandBackend;
pub use tun::TunCommandBackend;
