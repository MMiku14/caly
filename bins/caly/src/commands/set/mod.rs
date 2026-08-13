//! `caly set …` — mutations.
//!
//! Round 13 split: the dispatch is one declarative match on
//! `SetCmd`, and each resource family lives in its own
//! sub-module (`core` / `proxy` / `tun` / `sub` / `profile` /
//! `config` / `daemon` / `rule_provider`). The sub-modules
//! each export a `dispatch` function with the same
//! `(cmd, options, output) -> ExitCode` signature so the
//! top-level match stays trivial.
//!
//! Round 29: the `proxy_group` and `rule_provider`
//! resources were previously routed through a
//! `set::proxy_group::dispatch` /
//! `set::rule_provider::dispatch` 1-line wrapper that
//! just delegated to `commands::proxy_group::dispatch` /
//! `commands::rule_provider::dispatch`. The wrappers are
//! gone; the match here calls the canonical
//! `commands::<resource>::dispatch` directly. The
//! `set::` sub-module for those two resources folded
//! into a 1-line entry in the match arms (a `super::
//! super::` super-path is the same length as the
//! pre-Round-29 `proxy_group::dispatch` call site).
//!
//! # Dry-run policy
//!
//! Write commands default to `apply: false, dry_run: false`
//! (safe — no write). The operator must pass `--apply` to
//! actually mutate. The legacy `--dry-run` flag is still
//! accepted and still honored; `--apply` / `--dry-run` are
//! mutually exclusive (`clap::Arg::conflicts_with`).
//!
//! # Refresh semantics
//!
//! `refresh` always writes the body. The `--apply` /
//! `--dry-run` flags were dropped from `set profile refresh`
//! and `set rule-provider refresh` in Round 12 (a
//! `NotModified` HTTP 304 is a real outcome, not a dry-run).

use std::process::ExitCode;

use crate::cli::SetCmd;
use crate::output::CliOutput;

pub mod common;
pub mod config;
pub mod core;
pub mod daemon;
pub mod profile;
pub mod proxy;
pub mod sub;

/// Top-level dispatch. Round 13: each resource's `dispatch`
/// function lives in its own sub-module; the match here is
/// pure pass-through. Round 20: added `proxy_group` to the
/// match (the declarative `proxy_groups:` CRUD family).
/// Round 29: the `proxy_group` and `rule_provider`
/// resources moved out of `set/` (the per-resource
/// `set::proxy_group::dispatch` /
/// `set::rule_provider::dispatch` 1-line wrappers were
/// the only residents of those sub-modules, and they
/// just delegated to the canonical
/// `commands::proxy_group::dispatch` /
/// `commands::rule_provider::dispatch`). The
/// `super::super::proxy_group::dispatch` /
/// `super::super::rule_provider::dispatch` call site
/// is the same length as the pre-Round-29
/// `proxy_group::dispatch` /
/// `rule_provider::dispatch` references (the wrappers
/// were pure indirection).
pub fn run(cmd: SetCmd, options: crate::cli::CliOptions, output: CliOutput) -> ExitCode {
    match cmd {
        SetCmd::Core(c) => core::dispatch(c, options, output),
        SetCmd::Proxy(c) => proxy::dispatch(c, options, output),
        SetCmd::Tun(on) => crate::client::run_client(
            crate::ClientCommand::Sys(crate::client::legacy::SysCmd::Tun(on)),
            options,
        ),
        SetCmd::Sub(c) => sub::dispatch(c, options, output),
        SetCmd::Profile(c) => profile::dispatch(c, options, output),
        SetCmd::Config(c) => config::dispatch(c, options, output),
        SetCmd::Daemon(c) => daemon::dispatch(c, output),
        SetCmd::RuleProvider(c) => super::rule_provider::dispatch(c, options, output),
        SetCmd::ProxyGroup(c) => super::proxy_group::dispatch(c, options, output),
        SetCmd::Entry(c) => super::node_dispatch::dispatch(c, options, output),
    }
}
