//! Shared actor ports and the common bounded error type.
//!
//! This crate sits at the bottom of the product layer (above `caly-domain`) and
//! defines the trait boundaries that owner actors depend on and that concrete
//! adapters implement. Neither the application use-case crate nor the backend
//! adapters need to know each other's concrete types.
//!
//! # Why this crate is a separate workspace member
//!
//! `caly-backends` prod-deps the port traits, while the use-case crate is
//! prod-deped by the backends' composition consumers; inlining the traits into
//! either side would form a workspace dependency cycle (attempted in round 32
//! and reverted; the P7 replan moved the composition subtree out of
//! `caly-application`, but `caly-backends` still must not depend on the
//! use-case crate). This crate is the shared third node both sides depend on.

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
