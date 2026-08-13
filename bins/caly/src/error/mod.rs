//! Catalog of stable error codes used across the CLI.
//!
//! Every error returned by caly's CLI surface has a
//! `code: &'static str` drawn from this module. Scripts can
//! branch on the code (e.g. `case "$code" in
//! profile.not_declared) ... esac`) without parsing the
//! human message.
//!
//! # Code naming convention
//!
//! `<family>.<subkind>` — the family is the top-level command
//! (`profile`, `sub`, `core`, …) and the subkind describes the
//! failure mode. `usage.*` codes always exit 2; everything
//! else exits 1.
//!
//! # Module layout (Round 13)
//!
//! Each family lives in its own sub-module. The public API
//! is the same as Round 12: callers do
//! `crate::error::profile::NOT_DECLARED` or
//! `crate::error::core::connect_failed(...)` exactly as
//! before. Only the file layout changed.

pub use crate::output::CliError;

pub mod config;
pub mod core;
pub mod daemon;
pub mod profile;
pub mod sub;
pub mod usage;

// Round 13: re-export the top-level usage functions so the
// Round 10/11/12 public API (`crate::error::json_not_valid(...)`)
// keeps working unchanged. The implementations live in
// `usage::*` now; the re-exports make the path stable.
pub use usage::{binary_override_outside_daemon, json_not_valid, json_with_completion};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_error_carry_stable_codes() {
        let error = crate::client::profile::ProfileCmdError::NotDeclared("team".to_owned());
        let cli = profile::from_cmd_error(&error, "profile show team");
        assert_eq!(cli.code, profile::NOT_DECLARED);
        assert!(cli.message.contains("team"));
        assert!(cli.hint.is_some());
        assert_eq!(cli.command, "profile show team");
    }

    #[test]
    fn fetch_chain_error_carries_id() {
        use caly_profile::profile_fetch::ProfileFetchError;
        let error = crate::client::profile::ProfileCmdError::FetchChain {
            id: "outer".to_owned(),
            source: ProfileFetchError::ResolutionFailed,
        };
        let cli = profile::from_cmd_error(&error, "profile refresh outer");
        assert_eq!(cli.code, profile::FETCH_CHAIN);
        assert!(cli.message.contains("outer"));
    }

    #[test]
    fn error_mod_exports_all_codes() {
        // Locks the public API: every family has at least one
        // `pub const` and at least one `pub fn` so the
        // `commands::*` call sites compile unchanged.
        let _ = profile::NOT_DECLARED;
        let _: for<'a> fn(
            &crate::client::profile::ProfileCmdError,
            &'a str,
        ) -> crate::output::CliError = profile::from_cmd_error;
        let _ = sub::CHECK_FAILED;
        let _: fn(&str, &str) -> crate::output::CliError = |m, c| sub::check_failed(m, c);
        let _ = core::INVALID_TARGET;
        let _: for<'a> fn(&'a str, &'a str) -> crate::output::CliError = core::invalid_target;
        let _ = config::CHECK_FAILED;
        let _: for<'a> fn(&'a str, &'a str) -> crate::output::CliError = config::check_failed;
        let _ = daemon::ALREADY_RUNNING;
        let _: for<'a> fn(&'a str, &'a str) -> crate::output::CliError = daemon::already_running;
        let _ = usage::json_not_valid;
    }
}
