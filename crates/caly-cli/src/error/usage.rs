//! Usage-family error codes.
//!
//! `usage.*` codes always exit 2. They represent "the user
//! typed something the grammar should have caught but
//! didn't" — a mis-typed flag, a flag used in the wrong
//! context, etc. The `commands::*` dispatch checks for
//! these at runtime; clap catches the rest at parse time.

use crate::output::CliError;

/// Invalid usage that the grammar should have caught (mis-typed flag,
/// flag used in the wrong context, …).
pub const INVALID: &str = "usage.invalid";

/// `--json` is not valid for a particular command.
pub fn json_not_valid(command: &str) -> CliError {
    CliError::new(
        "usage.json_not_valid",
        "--json is only valid for client commands",
        command,
    )
}

/// A binary override flag was used outside `caly daemon`.
pub fn binary_override_outside_daemon(command: &str) -> CliError {
    CliError::new(
        "usage.binary_outside_daemon",
        "--mihomo-bin and --sing-box-bin are valid only with `caly daemon`",
        command,
    )
}

/// `--json` was used with shell completion.
pub fn json_with_completion(command: &str) -> CliError {
    CliError::new(
        "usage.json_with_completion",
        "--json cannot be used with generated shell completions",
        command,
    )
}
