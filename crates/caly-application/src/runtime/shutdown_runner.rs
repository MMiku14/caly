//! Concrete ordered async shutdown action driver.

use async_trait::async_trait;
use caly_domain::{BoundedText, BoundedVec};

use super::{ShutdownDriver, ShutdownPhase};

pub const MAX_SHUTDOWN_FAILURES: usize = 12;
/// Hard upper bound for one shutdown phase. A timed-out phase is recorded and
/// later phases still run, preserving final resource cleanup.
pub const SHUTDOWN_PHASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub type ShutdownFailures = BoundedVec<ShutdownFailure, MAX_SHUTDOWN_FAILURES>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShutdownFailure {
    pub phase: ShutdownPhase,
    pub message: BoundedText<512>,
}

/// Resource owners implement each mandatory shutdown action. Phases a
/// composition does not bind may use the no-op defaults, which complete
/// immediately without touching resources.
#[async_trait]
pub trait ShutdownActions {
    async fn reject_transport_mutations(&mut self) -> Result<(), BoundedText<512>> { Ok(()) }
    async fn close_command_ingress(&mut self) -> Result<(), BoundedText<512>> { Ok(()) }
    async fn stop_telemetry(&mut self) -> Result<(), BoundedText<512>>;
    async fn stop_and_reap_core(&mut self) -> Result<(), BoundedText<512>>;
    async fn restore_platform(&mut self) -> Result<(), BoundedText<512>>;
    async fn stop_subscriptions(&mut self) -> Result<(), BoundedText<512>> { Ok(()) }
    async fn finish_config_transactions(&mut self) -> Result<(), BoundedText<512>> { Ok(()) }
    async fn flush_event_sequencer(&mut self) -> Result<(), BoundedText<512>> { Ok(()) }
    async fn stop_projector(&mut self) -> Result<(), BoundedText<512>> { Ok(()) }
    async fn release_transport_and_lock(&mut self) -> Result<(), BoundedText<512>> { Ok(()) }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownRunError {
    FailureCapacity,
    DriverCompletedEarly,
}

/// Attempts every phase in order, retaining all bounded failures.
pub async fn run_shutdown(
    actions: &mut (impl ShutdownActions + Send),
) -> Result<ShutdownFailures, ShutdownRunError> {
    run_shutdown_with_timeout(actions, SHUTDOWN_PHASE_TIMEOUT).await
}

async fn run_shutdown_with_timeout(
    actions: &mut (impl ShutdownActions + Send),
    timeout: std::time::Duration,
) -> Result<ShutdownFailures, ShutdownRunError> {
    let mut driver = ShutdownDriver::new();
    let mut failures = ShutdownFailures::new();
    while driver.current() != ShutdownPhase::Completed {
        let phase = driver.current();
        if let Err(message) = execute_phase(actions, phase, timeout).await {
            failures
                .try_push(ShutdownFailure { phase, message })
                .map_err(|_| ShutdownRunError::FailureCapacity)?;
        }
        driver
            .complete_current()
            .map_err(|_| ShutdownRunError::DriverCompletedEarly)?;
    }
    Ok(failures)
}

async fn execute_phase(
    actions: &mut (impl ShutdownActions + Send),
    phase: ShutdownPhase,
    timeout: std::time::Duration,
) -> Result<(), BoundedText<512>> {
    let action = async {
        match phase {
            ShutdownPhase::RejectTransportMutations => actions.reject_transport_mutations().await,
            ShutdownPhase::CloseCommandIngress => actions.close_command_ingress().await,
            ShutdownPhase::StopTelemetry => actions.stop_telemetry().await,
            ShutdownPhase::StopAndReapCore => actions.stop_and_reap_core().await,
            ShutdownPhase::RestorePlatform => actions.restore_platform().await,
            ShutdownPhase::StopSubscriptions => actions.stop_subscriptions().await,
            ShutdownPhase::FinishConfigTransactions => actions.finish_config_transactions().await,
            ShutdownPhase::FlushEventSequencer => actions.flush_event_sequencer().await,
            ShutdownPhase::StopProjector => actions.stop_projector().await,
            ShutdownPhase::ReleaseTransportAndLock => actions.release_transport_and_lock().await,
            ShutdownPhase::Completed => Ok(()),
        }
    };
    tokio::time::timeout(timeout, action)
        .await
        .unwrap_or_else(|_| Err(timeout_failure(phase, timeout)))
}

fn timeout_failure(phase: ShutdownPhase, timeout: std::time::Duration) -> BoundedText<512> {
    // The formatted status line is far under the 512-byte bound, so the
    // infallible constructor degrades to a stable "_" fallback instead of
    // the old process-kill `unwrap_or_else(|_| abort)` if a future refactor
    // widens the input.
    caly_domain::BoundedText::from_nonempty_clamped(
        format!("shutdown phase {phase:?} exceeded {timeout:?}"),
        "shutdown phase exceeded",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_failure_identifies_the_phase() {
        let message = timeout_failure(ShutdownPhase::RestorePlatform, SHUTDOWN_PHASE_TIMEOUT);
        assert!(message.as_str().contains("RestorePlatform"));
        assert!(message.as_str().contains("exceeded"));
    }
}
