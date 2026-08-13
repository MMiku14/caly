//! Real-core candidate validation contract.

pub mod linux;

pub use linux::LinuxCommandValidator;

use std::time::Duration;

use caly_platform::{PlatformFailure, process::SpawnSpec};

/// Bounded validator execution result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationReport {
    pub accepted: bool,
    pub diagnostic: Option<caly_domain::BoundedText<1_024>>,
}

/// Backend executes a contained process with output caps, timeout, kill and reap.
pub trait CoreValidator {
    fn validate(
        &mut self,
        spec: SpawnSpec,
        timeout: Duration,
    ) -> Result<ValidationReport, PlatformFailure>;
}
