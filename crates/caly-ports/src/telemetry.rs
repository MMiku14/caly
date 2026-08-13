//! TelemetryActor sampling port.

use super::error::ActorFailure;

/// Telemetry permits explicit coalescing but never invisible loss.
pub trait TelemetryCommandBackend {
    fn sample(&mut self) -> Result<(), ActorFailure>;
    fn record_dropped(&mut self, count: u64) -> Result<(), ActorFailure>;
    fn reset_generation(&mut self, generation: u64) -> Result<(), ActorFailure>;
}
