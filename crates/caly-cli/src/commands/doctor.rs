//! Round 11: doctor is now `caly tool doctor`. Round 10's
//! `run_doctor(options, fix)` logic ported here so the
//! `commands::tool` namespace can route to it directly.
use std::process::ExitCode;

use crate::output::{self, CliOutput};

pub fn run(options: crate::cli::CliOptions, fix: bool) -> ExitCode {
    let output = CliOutput::from_json_flag(options.json);
    if fix {
        if let Err(error) = crate::doctor::grant_tun_capabilities() {
            return output::report_error_returning(
                output,
                crate::error::CliError::new(
                    "doctor.fix_failed",
                    format!("doctor fix failed: {error}"),
                    "tool doctor --fix",
                ),
            );
        }
        println!();
    }
    let results = crate::doctor::run_checks(&|key| std::env::var_os(key));
    if output.is_json() {
        match crate::doctor::render_json(&results) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                return output::report_error_returning(
                    output,
                    crate::error::CliError::new(
                        "doctor.render_failed",
                        format!("cannot render doctor JSON: {error}"),
                        "tool doctor",
                    ),
                );
            }
        }
    } else {
        print!("{}", crate::doctor::render_human(&results));
    }
    if crate::doctor::any_failed(&results) {
        // C-A: 3 = diagnostic failure (doctor/dns probe), matching dns.rs.
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}
