//! Daemon control plane error codes (`caly daemon`).

// Test-scoped: the daemon-host / `status` surfaces map their
// own codes; this pair survives only as the stable-code
// contract locked by `error_mod_exports_all_codes`.
#[cfg(test)]
use crate::output::CliError;

#[cfg(test)]
pub const ALREADY_RUNNING: &str = "daemon.already_running";

/// The daemon could not be reached (not running, socket missing, …).
pub const UNREACHABLE: &str = "daemon.unreachable";

#[cfg(test)]
pub fn already_running(detail: &str, command: &str) -> CliError {
    CliError::new(
        ALREADY_RUNNING,
        format!("another daemon is running: {detail}"),
        command,
    )
    .with_hint("stop the existing daemon or pass `--socket PATH` to a fresh one")
}
