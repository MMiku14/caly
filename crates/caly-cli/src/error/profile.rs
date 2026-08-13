//! Profile-family error codes (`caly profile …`).
//!
//! Each code is the last dotted segment of a stable
//! `profile.<subkind>` identifier. Scripts branch on the
//! code, not the human message; the human message is for
//! operators.

// The production `profile` leaves map `ProfileCmdError` via
// their own `code_for`; `NOT_DECLARED` / `FETCH_CHAIN` /
// `from_cmd_error` survive here only as the stable-code
// contract locked by `error_mod_exports_all_codes` (and the
// codes they emit), so they are test-scoped.
#[cfg(test)]
use crate::output::CliError;

#[cfg(test)]
pub const NOT_DECLARED: &str = "profile.not_declared";
#[cfg(test)]
pub const FETCH_CHAIN: &str = "profile.fetch_chain";

/// Wraps a `ProfileCmdError` from the profile command
/// module into a `CliError` carrying a stable code.
/// `command` is the user-typed command line (e.g.
/// `"profile add team remote:https://..."`). Test-scoped:
/// the production `profile` leaves map their own codes.
#[cfg(test)]
pub fn from_cmd_error(error: &crate::client::profile::ProfileCmdError, command: &str) -> CliError {
    use crate::client::profile::ProfileCmdError as E;
    let (code, message) = match error {
        E::NotDeclared(id) => (NOT_DECLARED, format!("profile `{id}` is not declared")),
        E::AlreadyDeclared(id) => (
            "profile.already_declared",
            format!("profile `{id}` is already declared"),
        ),
        E::InvalidId(id) => (
            "profile.invalid_id",
            format!("profile id `{id}` is not path-safe ASCII"),
        ),
        E::InvalidSource(spec) => (
            "profile.invalid_source",
            format!("source spec `{spec}` is invalid"),
        ),
        E::ReadConfig(reason) => ("profile.read_config", format!("read config: {reason}")),
        E::ParseConfig(reason) => ("profile.parse_config", format!("parse config: {reason}")),
        E::Store(reason) => ("profile.store", format!("profile store: {reason}")),
        E::Fetch(reason) => ("profile.fetch", format!("profile fetch: {reason}")),
        E::FetchChain { id, source } => {
            (FETCH_CHAIN, format!("profile `{id}` chain fetch: {source}"))
        }
        E::Backup { from, to, reason } => (
            "profile.backup",
            format!(
                "config backup from {} to {} failed: {reason}",
                from.display(),
                to.display()
            ),
        ),
    };
    let mut cli = CliError::new(code, message, command);
    cli = match error {
        E::NotDeclared(_) => cli.with_hint("declare it first with `caly profile add <id> ...`"),
        E::AlreadyDeclared(_) => {
            cli.with_hint("run `caly profile list` to see the current declarations")
        }
        E::InvalidId(_) => {
            cli.with_hint("ids must be path-safe ASCII (alphanumeric, `-`, `_`); max 64 bytes")
        }
        E::InvalidSource(_) => cli.with_hint(
            "source spec is `remote:<url>`, `local:<path>` or `merge:<id1,id2,...>`",
        ),
        E::Backup { .. } => {
            cli.with_hint("check the source file is readable and the config dir is writable")
        }
        E::Fetch(_) | E::FetchChain { .. } => {
            cli.with_hint("check the URL, your network, and the SSRF guard; try `--dry-run` first")
        }
        E::ReadConfig(_) | E::ParseConfig(_) => cli.with_hint(
            "run `caly config generate` to create a fresh `config.yaml`, or `caly config validate` to inspect the merged tree",
        ),
        E::Store(_) => cli.with_hint("check the state directory is writable"),
    };
    cli
}
