//! Subscription-family error codes (`caly sub …`).

// Test-scoped: the production `sub` leaves map their own
// codes; this pair survives only as the stable-code
// contract locked by `error_mod_exports_all_codes`.
#[cfg(test)]
use crate::output::CliError;

#[cfg(test)]
pub const CHECK_FAILED: &str = "sub.check_failed";

#[cfg(test)]
pub fn check_failed(message: impl Into<String>, command: &str) -> CliError {
    CliError::new(CHECK_FAILED, message, command).with_hint(
        "verify the file is a valid subscription; try `caly sub list` to see configured providers",
    )
}
