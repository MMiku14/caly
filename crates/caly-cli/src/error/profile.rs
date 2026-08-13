//! Profile-family error codes (`caly profile …`).
//!
//! Each code is the last dotted segment of a stable
//! `profile.<subkind>` identifier. Scripts branch on the
//! code, not the human message; the human message is for
//! operators.

use crate::output::CliError;

pub const NOT_DECLARED: &str = "profile.not_declared";
pub const ALREADY_DECLARED: &str = "profile.already_declared";
pub const INVALID_ID: &str = "profile.invalid_id";
pub const INVALID_SOURCE: &str = "profile.invalid_source";
pub const READ_CONFIG: &str = "profile.read_config";
pub const PARSE_CONFIG: &str = "profile.parse_config";
pub const BACKUP: &str = "profile.backup";
pub const FETCH: &str = "profile.fetch";
pub const FETCH_CHAIN: &str = "profile.fetch_chain";
pub const STORE: &str = "profile.store";

/// Wraps a `ProfileCmdError` from the profile command
/// module into a `CliError` carrying a stable code.
/// `command` is the user-typed command line (e.g.
/// `"profile add team remote:https://..."`).
pub fn from_cmd_error(error: &crate::client::profile::ProfileCmdError, command: &str) -> CliError {
    use crate::client::profile::ProfileCmdError as E;
    let (code, message) = match error {
        E::NotDeclared(id) => (NOT_DECLARED, format!("profile `{id}` is not declared")),
        E::AlreadyDeclared(id) => (
            ALREADY_DECLARED,
            format!("profile `{id}` is already declared"),
        ),
        E::InvalidId(id) => (
            INVALID_ID,
            format!("profile id `{id}` is not path-safe ASCII"),
        ),
        E::InvalidSource(spec) => (INVALID_SOURCE, format!("source spec `{spec}` is invalid")),
        E::ReadConfig(reason) => (READ_CONFIG, format!("read config: {reason}")),
        E::ParseConfig(reason) => (PARSE_CONFIG, format!("parse config: {reason}")),
        E::Store(reason) => (STORE, format!("profile store: {reason}")),
        E::Fetch(reason) => (FETCH, format!("profile fetch: {reason}")),
        E::FetchChain { id, source } => {
            (FETCH_CHAIN, format!("profile `{id}` chain fetch: {source}"))
        }
        E::Backup { from, to, reason } => (
            BACKUP,
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
