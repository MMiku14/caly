//! caly composition root (Round 11: 5-namespace dispatch; P8a: host shell).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))] // #53: tests assert with unwrap/expect/panic; production lint stays deny
use std::process::ExitCode;

pub mod bootstrap;
mod commands;
mod daemon;
mod daemon_config;
mod logging;

fn main() -> ExitCode {
    let _ = logging::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `caly daemon` (the bare host command) is dispatched back here by the
    // presentation layer (caly-cli); every other command runs inside it.
    caly_cli::run(args, commands::daemon::run)
}
