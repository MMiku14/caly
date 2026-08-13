//! caly-cli — the presentation layer (CLI surface) of caly.
//!
//! P8 (docs/refactor-p8-design.md): the TUI's former layer-5 slot is
//! resurrected by the thin CLI client. The crate boundary enforces the
//! thin-client rule at compile time: the presentation layer talks to the
//! daemon only through the wire protocol (caly-protocol v2) and offline
//! projections over config/subscription data — it must never reach into
//! composition/application/backends/server (checked by
//! scripts/check-integration-invariants.py, P8c).
//!
//! The daemon host stays in `bins/caly`; `caly daemon` is dispatched back
//! to the host through the `host` closure of [`run`].

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny

use std::process::ExitCode;

use crate::cli::{Command, parse_args};
use crate::output::CliOutput;

pub mod cli;
pub(crate) mod client;
pub(crate) mod commands;
/// Layered `config.yaml` read/parse surface (P8a: moved up from the
/// daemon host so offline CLI commands share one loader with boot).
pub mod config;
/// DNS probing for `tool dns` (P8a: moved up from the daemon host —
/// pure caly-dns + std, no host internals).
pub(crate) mod dns;
pub(crate) mod doctor;
/// Entry-tree renderer (cli-v3-design.md G1 / W3a): the shared 三区制
/// layout behind `sub parse` and `node list --offline --format=tree`.
mod entry_tree;
pub(crate) mod error;
pub(crate) mod output;
mod subscription;

/// Cross-module test helpers. Round 26 collapsed 11
/// per-module `temp_root` + 7 per-module
/// `hermetic_paths` copies into a single source of
/// truth. The module is `pub(crate)` so internal
/// test modules can `use crate::test_helpers::...`
/// without exposing the helpers to downstream
/// crates.
pub(crate) mod test_helpers;

pub use client::ClientCommand;

/// CLI entry: parses `args`, records side-effecting operations in
/// history, and dispatches. `caly daemon` (the bare host command) is
/// handed to `host` — the composition-root shell in `bins/caly` — which
/// owns the daemon runtime.
pub fn run(args: Vec<String>, host: impl FnOnce(cli::CliOptions) -> ExitCode) -> ExitCode {
    install_broken_pipe_hook();
    match parse_args(args.iter().cloned()) {
        Ok(invocation) => {
            // Operation memory: record side-effecting operations (sub
            // enable, core switch, …) before they run. Read-only leaves
            // are filtered inside; a failed record never fails the command.
            crate::commands::history::maybe_record(&args, &invocation.command);
            dispatch(invocation.command, invocation.options, host)
        }
        Err(error) => crate::error::print_and_fold(&error),
    }
}

fn install_broken_pipe_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let is_broken_pipe = info
            .payload()
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
            || info.to_string().contains("Broken pipe");
        if is_broken_pipe {
            // Flush before the raw exit so pipelined
            // consumers get every byte written so far
            // (`process::exit` skips destructors).
            use std::io::Write;
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            std::process::exit(141);
        }
        default(info);
    }));
}

/// Dispatches a parsed invocation. `pub(crate)` so `caly history replay`
/// can re-dispatch recorded operations through the identical path.
pub(crate) fn dispatch(
    command: Command,
    options: cli::CliOptions,
    host: impl FnOnce(cli::CliOptions) -> ExitCode,
) -> ExitCode {
    let output = CliOutput::from_json_flag(options.json);
    if !matches!(&command, Command::Daemon)
        && (options.mihomo_bin.is_some() || options.sing_box_bin.is_some())
    {
        return output::report_error_returning(
            output,
            error::binary_override_outside_daemon("caly (not daemon)"),
        );
    }
    match command {
        Command::Daemon => {
            if options.json {
                return output::report_error_returning(output, error::json_not_valid("daemon"));
            }
            host(options)
        }
        Command::Tool(cmd) => commands::tool::run(cmd, options),
        Command::Show(cmd) => commands::show::run(cmd, options, output),
        Command::Set(cmd) => commands::set::run(cmd, options, output),
        Command::History(cmd) => commands::history::run(cmd, &options),
        Command::Completions(shell) => {
            if options.json {
                return output::report_error_returning(
                    output,
                    error::json_with_completion("completions"),
                );
            }
            cli::generate_completions(shell);
            ExitCode::SUCCESS
        }
    }
}
