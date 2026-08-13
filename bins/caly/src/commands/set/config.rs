//! `set config …` dispatch.
//!
//! Round 15: `apply` / `generate` / `default` / `edit` route
//! through the typed `client::run_client` / `client::*`
//! helpers (the `bridge` shim was retired). Round 16:
//! `diff` is a real line-by-line diff (current vs
//! `--file <PATH>` or vs the documented default).

use std::process::ExitCode;

use crate::cli::SetConfigCmd;
use crate::client::legacy::ConfigCmd;
use crate::output::CliOutput;

pub fn dispatch(c: SetConfigCmd, options: crate::cli::CliOptions, _output: CliOutput) -> ExitCode {
    match c {
        SetConfigCmd::Apply => {
            crate::client::run_client(crate::ClientCommand::Config(ConfigCmd::Apply), options)
        }
        SetConfigCmd::Generate => crate::client::config_generate::generate_config(options.json),
        SetConfigCmd::Default => crate::client::config_generate::reset_default_config(options.json),
        SetConfigCmd::Edit(editor) => {
            crate::client::config_generate::edit_config(editor, options.json)
        }
        SetConfigCmd::Diff { file } => {
            crate::client::config_generate::diff_config(file.as_deref(), options.json)
        }
    }
}
