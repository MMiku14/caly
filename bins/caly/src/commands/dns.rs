//! Round 11: dns is now `caly tool dns`. Re-export the
//! underlying `crate::dns::run_dns` under a short name.
pub use crate::dns::run_dns as run;
