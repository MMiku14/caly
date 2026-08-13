//! Core lifecycle / control error codes (`caly core …`).

use crate::output::CliError;

// Test-scoped: the production `core` leaves map their own
// codes (see `commands/core/*`); this pair survives only as
// the stable-code contract locked by
// `error_mod_exports_all_codes`.
#[cfg(test)]
pub const INVALID_TARGET: &str = "core.invalid_target";
pub const CONNECT_FAILED: &str = "core.connect_failed";
pub const HANDSHAKE_FAILED: &str = "core.handshake_failed";
pub const OPERATION_FAILED: &str = "core.operation_failed";

/// A core-side runtime failure surfaced from the daemon client.
pub const RUNTIME_FAILED: &str = "runtime.failed";

#[cfg(test)]
pub fn invalid_target(value: &str, command: &str) -> CliError {
    CliError::new(
        INVALID_TARGET,
        format!("core must be mihomo or sing-box (got `{value}`)"),
        command,
    )
    .with_hint("pass `--core mihomo` or `--core sing-box`")
}

pub fn connect_failed(detail: &str, command: &str) -> CliError {
    CliError::new(
        CONNECT_FAILED,
        format!("cannot reach the caly daemon: {detail}"),
        command,
    )
    .with_hint("start it with `caly daemon`, or pass `--socket PATH` if it is running on a non-default location")
}

pub fn handshake_failed(detail: &str, command: &str) -> CliError {
    CliError::new(
        HANDSHAKE_FAILED,
        format!("daemon handshake failed: {detail}"),
        command,
    )
    .with_hint("restart the daemon; the client and daemon must run the same caly version")
}

pub fn operation_failed(detail: &str, command: &str) -> CliError {
    CliError::new(OPERATION_FAILED, detail.to_owned(), command)
}
