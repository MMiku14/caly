//! Daemon application layer for caly.
//!
//! Mutable aggregates are owned by bounded actor mailboxes. Cross-owner work
//! is coordinated by acyclic sagas. This crate contains no transport server or
//! platform backend and starts no task without an externally supplied owner.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
pub mod actor_result;
pub mod actors;
pub mod command_bus;
pub mod events;
pub mod operations;
pub mod projection;
pub mod reconciler;
pub mod routing;
mod routing_fanout;
pub mod runtime;
pub mod service;
pub mod supervision;

pub use routing_fanout::ActorCommandFanout;
