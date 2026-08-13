//! Round 13: 5-namespace command dispatch + 4 helpers.
//!
//! Each module owns one concern:
//!
//! - `daemon` — long-running process (Round 10).
//! - `tool` — static utilities (Round 11).
//! - `show` — read-only queries (Round 11).
//! - `set` — mutations, one sub-module per resource
//!   (`set/core` / `set/proxy` / `set/tun` / `set/sub` /
//!   `set/profile` / `set/config` / `set/daemon` /
//!   `set/rule_provider`) (Round 13).
//! - `refresh` — `Refreshable` trait + 3 impls (Round 13
//!   locked, Round 14 to route through).
//! - `rule_provider` — `set rule-provider` family
//!   (`list` is real; the rest are planned_ok).
//! - `status` — `show status` handler.
//! - `dns` / `doctor` — re-exports + adapters for
//!   `tool doctor` / `tool dns`.
//!
//! Round 16: `bridge` retired (5 `old_*` shims + 3 `sub_*`
//! shims all moved into `client::*`). The
//! `commands::bridge` module no longer exists.

pub mod daemon;
pub mod dns;
pub mod doctor;
pub mod flow;
pub mod history;
pub mod node_dispatch;
pub mod node_tree;
pub mod proxy_group;
pub mod refresh;
pub mod rule_provider;
pub mod set;
pub mod show;
pub mod status;
pub mod sys_status;
pub mod tool;
