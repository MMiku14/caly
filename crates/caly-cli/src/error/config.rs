//! Config-family error codes (`caly config …`).

use crate::output::CliError;

pub const CHECK_FAILED: &str = "config.check_failed";

pub fn check_failed(detail: &str, command: &str) -> CliError {
    CliError::new(
        CHECK_FAILED,
        format!("configuration check failed: {detail}"),
        command,
    )
    .with_hint("fix the reported line; the schema rejects unknown fields and out-of-bounds values")
}
