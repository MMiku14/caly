//! Versioned protocol and transport contracts for caly.
//!
//! Unknown wire enums remain raw until strict conversion. Decode budgets are
//! enforced independently of transport message ceilings. Thin clients depend
//! on this crate without acquiring daemon application implementations.
//!
//! Transport is length-prefixed compact JSON over UDS/TCP (`framing`,
//! `wire_frames`); no gRPC/protobuf machinery is involved.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
pub mod client;
pub mod conversion;
pub mod framing;
pub mod json_serde;
pub mod local_ipc;
pub mod protocol;
pub mod wire_frames;
