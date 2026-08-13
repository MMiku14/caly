//! Configuration infrastructure for caly.
//!
//! Schema parsing, validation and generation caches live here. Subscription
//! intake/fetch moved to `caly-subscription` (P6, with the `reqwest` WAN
//! boundary); process-based template rendering moved to `caly-template` (P2);
//! application coordinators own publication.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
pub mod loader;
pub mod profile_fetch;
pub mod profile_store;
pub mod rule_match;
pub mod schema;
