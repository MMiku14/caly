//! Bounded external command execution contract.

pub mod linux;

pub use linux::LinuxCommandRunner;

use std::{path::PathBuf, time::Duration};

use caly_domain::{BoundedText, BoundedVec};

use crate::PlatformFailure;

/// Maximum command arguments and captured bytes per stream.
pub const MAX_COMMAND_ARGUMENTS: usize = 128;
pub const MAX_COMMAND_OUTPUT_BYTES: usize = 1_048_576;

pub type CommandArgument = BoundedText<4_096>;
pub type CommandArguments = BoundedVec<CommandArgument, MAX_COMMAND_ARGUMENTS>;
pub type CommandOutput = BoundedVec<u8, MAX_COMMAND_OUTPUT_BYTES>;

/// Shell-free bounded command request.
pub struct CommandRequest {
    pub executable: PathBuf,
    pub arguments: CommandArguments,
    pub timeout: Duration,
}

/// Fully reaped command result with bounded output.
pub struct CommandResult {
    pub exit_code: Option<i32>,
    pub stdout: CommandOutput,
    pub stderr: CommandOutput,
}

/// Dedicated backend must enforce timeout, output caps, kill and wait.
pub trait CommandRunner {
    fn run_bounded(&mut self, request: CommandRequest) -> Result<CommandResult, PlatformFailure>;
}
