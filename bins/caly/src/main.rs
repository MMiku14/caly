//! caly composition root (Round 11: 5-namespace dispatch).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
use std::process::ExitCode;

use crate::cli::{Command, parse_args};
use crate::output::CliOutput;

pub mod bootstrap;
mod cli;
mod client;
mod daemon;
mod daemon_config;
mod dns;
mod doctor;
pub mod error;
mod logging;
pub mod output;

/// Entry-tree renderer (cli-v3-design.md G1 / W3a): the shared 三区制
/// layout behind `sub parse` and `node list --offline --format=tree`.
mod entry_tree;
mod subscription;

/// Cross-module test helpers. Round 26 collapsed 11
/// per-module `temp_root` + 7 per-module
/// `hermetic_paths` copies into a single source of
/// truth. The module is `pub(crate)` so internal
/// test modules can `use crate::test_helpers::...`
/// without exposing the helpers to downstream
/// crates. See the module-level docs for the
/// `cfg(test)` gating rationale.
pub(crate) mod test_helpers;

mod commands;

pub use client::ClientCommand;

fn main() -> ExitCode {
    install_broken_pipe_hook();
    let _ = logging::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(args.iter().cloned()) {
        Ok(invocation) => {
            // Operation memory: record side-effecting operations (sub
            // enable, core switch, …) before they run. Read-only leaves
            // are filtered inside; a failed record never fails the command.
            crate::commands::history::maybe_record(&args, &invocation.command);
            run(invocation.command, invocation.options)
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
pub(crate) fn run(command: Command, options: cli::CliOptions) -> ExitCode {
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
            commands::daemon::run(options)
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
