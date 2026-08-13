//! `caly tool …` — static / one-shot tools.

use std::process::ExitCode;

use crate::cli::ToolCmd;
use crate::output::CliOutput;

pub fn run(cmd: ToolCmd, options: crate::cli::CliOptions) -> ExitCode {
    let _output = CliOutput::from_json_flag(options.json);
    match cmd {
        ToolCmd::Help(sub) => {
            // `--help` surfaces as a DisplayHelp clap error (exit 0); an
            // unknown subcommand surfaces as a usage error (exit 2). The
            // R1 back-link comes from parse_args' inspect_err.
            let parsed = if let Some(sub) = sub {
                crate::cli::parse_args([sub, "--help".to_owned()])
            } else {
                crate::cli::parse_args(["--help".to_owned()])
            };
            match parsed {
                Ok(_) => ExitCode::SUCCESS,
                Err(error) => {
                    // clap codes are 0 (help/version) or 2 (usage); fold
                    // out-of-range codes into 1 like main.rs does.
                    let code = error.exit_code();
                    let _ = error.print();
                    ExitCode::from(u8::try_from(code).unwrap_or(1))
                }
            }
        }
        ToolCmd::Version => {
            println!("caly {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        ToolCmd::Doctor { fix } => super::doctor::run(options, fix),
        ToolCmd::Dns(d) => super::dns::run(d, options.json),
    }
}
