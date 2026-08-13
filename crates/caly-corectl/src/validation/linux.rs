//! Linux validator adapter backed by the bounded command runner.

use std::time::Duration;

use caly_domain::BoundedText;
use caly_platform::{
    PlatformFailure,
    command::{CommandRequest, CommandRunner},
    process::SpawnSpec,
};

use super::{CoreValidator, ValidationReport};

/// Runs a core's validation executable without a shell.
pub struct LinuxCommandValidator<R> {
    runner: R,
}

impl<R> LinuxCommandValidator<R> {
    /// Creates a validator around a command runner owner.
    pub const fn new(runner: R) -> Self {
        Self { runner }
    }

    /// Returns the underlying runner after validation ownership ends.
    pub fn into_inner(self) -> R {
        self.runner
    }
}

impl<R: CommandRunner> CoreValidator for LinuxCommandValidator<R> {
    fn validate(
        &mut self,
        spec: SpawnSpec,
        timeout: Duration,
    ) -> Result<ValidationReport, PlatformFailure> {
        let result = self.runner.run_bounded(CommandRequest {
            executable: spec.executable,
            arguments: spec.arguments,
            timeout,
        })?;
        let diagnostic = if result.exit_code == Some(0) {
            None
        } else {
            let bytes = if result.stderr.is_empty() {
                result.stdout.as_slice()
            } else {
                result.stderr.as_slice()
            };
            Some(bounded_diagnostic(
                String::from_utf8_lossy(bytes).into_owned(),
            ))
        };
        Ok(ValidationReport {
            accepted: result.exit_code == Some(0),
            diagnostic,
        })
    }
}

fn bounded_diagnostic(value: String) -> BoundedText<1_024> {
    // The validator's stderr/stdout is dynamic (any bytes the core validator
    // emitted) and can easily exceed the 1 KiB bounded-text capacity on a
    // hostile or chatty binary. The previous double-fallback form was a
    // process-kill when both `BoundedText::new` calls failed; switching to
    // the infallible `from_nonempty_clamped` keeps the same behaviour for
    // the well-formed call sites and surfaces a stable
    // `"core validator rejected output"` fallback for any future error
    // variant that overshoots the bound. `caly-corectl` is a low-level crate
    // that does not depend on `tracing`; the truncation is silent at the
    // crate boundary and the `ValidationReport` caller surfaces the
    // bounded text directly.
    BoundedText::from_nonempty_clamped(value, "core validator rejected output")
}

#[cfg(test)]
mod tests {
    use super::*;
    use caly_platform::{command::LinuxCommandRunner, process::ProcessArguments};
    use std::path::PathBuf;

    #[test]
    fn true_validator_reports_accepted() -> Result<(), PlatformFailure> {
        let mut validator = LinuxCommandValidator::new(LinuxCommandRunner);
        let report = validator.validate(
            SpawnSpec {
                executable: PathBuf::from("/bin/true"),
                arguments: ProcessArguments::new(),
                working_directory: PathBuf::from("/tmp"),
                kill_on_owner_drop: true,
                label: "probe".to_owned(),
            },
            Duration::from_secs(1),
        )?;
        assert!(report.accepted);
        assert!(report.diagnostic.is_none());
        Ok(())
    }
}
