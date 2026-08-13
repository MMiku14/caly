//! Proxy-kernel infrastructure for Mihomo and sing-box.
//!
//! `SpawnSpecFactory` and `KernelControl` are deliberately separate. The
//! Application `CoreActor` owns the process tree returned by `caly-platform`.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
pub mod common;
pub mod contract;
pub mod mihomo;
pub mod sing_box;
pub mod validation;
