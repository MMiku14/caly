//! Daemon-side transport adapters for caly.
//!
//! This crate admits authenticated requests, maps protocol commands to
//! application use cases, and maps structured results back to wire responses.
//! It owns no business state and performs no kernel or configuration work.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
pub mod admission;
pub mod compat;
pub mod json;
pub mod tcp;
pub mod uds;
