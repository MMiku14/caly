//! Linux bounded external-command runner.
//!
//! Commands are executed directly without a shell. stdout and stderr are
//! drained concurrently, capped independently, and the child is always
//! waited on before returning.

use std::{
    io::{self, Read},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use super::{CommandOutput, CommandRequest, CommandResult, CommandRunner};
use crate::PlatformFailure;
use crate::bounded_text as bounded;
use caly_domain::BoundedVec;

/// Linux command runner with bounded output and timeout enforcement.
#[derive(Default)]
pub struct LinuxCommandRunner;

type CapturedOutput = io::Result<(Vec<u8>, bool)>;
type OutputReader = thread::JoinHandle<CapturedOutput>;

impl CommandRunner for LinuxCommandRunner {
    fn run_bounded(&mut self, request: CommandRequest) -> Result<CommandResult, PlatformFailure> {
        let mut child = spawn_command(&request)?;
        let (stdout_reader, stderr_reader) = capture_readers(&mut child)?;
        let wait_result = wait_until(&mut child, request.timeout);
        // Always join the reader threads so a timeout cannot orphan them.
        let stdout = join_reader(stdout_reader, "stdout")?;
        let stderr = join_reader(stderr_reader, "stderr")?;
        let status = wait_result?;
        if stdout.1 || stderr.1 {
            return Err(failure(
                "command-output-limit",
                "command output exceeded the configured bounded capacity".to_owned(),
            ));
        }
        Ok(CommandResult {
            exit_code: status.code(),
            stdout: bounded_output(stdout.0)?,
            stderr: bounded_output(stderr.0)?,
        })
    }
}

fn spawn_command(request: &CommandRequest) -> Result<std::process::Child, PlatformFailure> {
    Command::new(&request.executable)
        .args(
            request
                .arguments
                .iter()
                .map(caly_domain::BoundedText::as_str),
        )
        // Diagnostics parsers (`is_busy_failure`, getcap checks, …) match the
        // English tool output; without a pinned locale, a `LANG=de_DE.UTF-8`
        // daemon would see localized `ip` errors and mis-classify the
        // outcome. `LC_ALL=C` pins child output to the C locale.
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| failure("spawn-command", format!("cannot spawn command: {error}")))
}

fn capture_readers(
    child: &mut std::process::Child,
) -> Result<(OutputReader, OutputReader), PlatformFailure> {
    let stdout = child.stdout.take().ok_or_else(|| {
        failure(
            "capture-command-output",
            "stdout pipe was not created".to_owned(),
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        failure(
            "capture-command-output",
            "stderr pipe was not created".to_owned(),
        )
    })?;
    Ok((
        thread::spawn(|| read_capped(stdout)),
        thread::spawn(|| read_capped(stderr)),
    ))
}

fn wait_until(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<std::process::ExitStatus, PlatformFailure> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| failure("wait-command", format!("cannot poll command: {error}")))?
        {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait().map_err(|error| {
                failure(
                    "reap-command",
                    format!("cannot reap timed-out command: {error}"),
                )
            })?;
            return Err(failure(
                "timeout-command",
                format!("command did not finish before timeout; final status {status}"),
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn join_reader(reader: OutputReader, stream: &str) -> Result<(Vec<u8>, bool), PlatformFailure> {
    reader
        .join()
        .map_err(|_| failure("read-command-output", format!("{stream} reader panicked")))?
        .map_err(|error| {
            failure(
                "read-command-output",
                format!("cannot read {stream}: {error}"),
            )
        })
}

fn read_capped(mut reader: impl Read) -> io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut overflow = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok((output, overflow));
        }
        if output.len().saturating_add(count) <= super::MAX_COMMAND_OUTPUT_BYTES {
            output.extend_from_slice(&buffer[..count]);
        } else {
            overflow = true;
            let remaining = super::MAX_COMMAND_OUTPUT_BYTES.saturating_sub(output.len());
            output.extend_from_slice(&buffer[..remaining]);
        }
    }
}

fn bounded_output(output: Vec<u8>) -> Result<CommandOutput, PlatformFailure> {
    BoundedVec::try_from_vec(output).map_err(|_| {
        failure(
            "bound-command-output",
            "captured output exceeded capacity".to_owned(),
        )
    })
}

fn failure(operation: &'static str, message: String) -> PlatformFailure {
    PlatformFailure {
        operation: bounded(operation, "command-operation"),
        resource: bounded("linux-command".to_owned(), "command"),
        message: bounded(message, "Linux command operation failed"),
        suggested_action: bounded(
            "inspect the executable, timeout, permissions, and output size".to_owned(),
            "inspect command execution",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandArguments, CommandRunner};
    use std::path::PathBuf;

    #[test]
    fn direct_command_is_reaped_with_bounded_empty_output() -> Result<(), PlatformFailure> {
        let mut runner = LinuxCommandRunner;
        let result = runner.run_bounded(CommandRequest {
            executable: PathBuf::from("/bin/true"),
            arguments: CommandArguments::new(),
            timeout: Duration::from_secs(1),
        })?;
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
        Ok(())
    }

    #[test]
    fn timeout_kills_and_returns_error_without_hanging() {
        let mut arguments = CommandArguments::new();
        if let Ok(argument) = crate::command::CommandArgument::new("5".to_owned()) {
            let _ = arguments.try_push(argument);
        }
        let start = Instant::now();
        let mut runner = LinuxCommandRunner;
        let result = runner.run_bounded(CommandRequest {
            executable: PathBuf::from("/bin/sleep"),
            arguments,
            timeout: Duration::from_millis(100),
        });
        assert!(result.is_err());
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "timeout path must not hang (reader threads joined)"
        );
    }
}
