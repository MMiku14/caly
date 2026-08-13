//! Daemon control plane error codes (`caly daemon`).

use crate::output::CliError;

pub const ALREADY_RUNNING: &str = "daemon.already_running";

/// The daemon could not be reached (not running, socket missing, …).
pub const UNREACHABLE: &str = "daemon.unreachable";

pub fn already_running(detail: &str, command: &str) -> CliError {
    CliError::new(
        ALREADY_RUNNING,
        format!("another daemon is running: {detail}"),
        command,
    )
    .with_hint("stop the existing daemon or pass `--socket PATH` to a fresh one")
}
