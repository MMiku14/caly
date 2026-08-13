//! Subscription-family error codes (`caly sub …`).

use crate::output::CliError;

pub const CHECK_FAILED: &str = "sub.check_failed";

pub fn check_failed(message: impl Into<String>, command: &str) -> CliError {
    CliError::new(CHECK_FAILED, message, command).with_hint(
        "verify the file is a valid subscription; try `caly sub list` to see configured providers",
    )
}
