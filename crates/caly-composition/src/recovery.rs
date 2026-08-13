//! Crash detection and automatic core restart (self-healing).
//!
//! Recovery is driven by a pure `CrashLoopPolicy` state machine so the backoff
//! and stabilization semantics can be unit tested without spawning tasks.
//! The async driver only decides when to poll, sleep or restart and performs
//! the projection reports.

use std::time::Duration;

use caly_application::{
    actor_result::{ActorResultClient, ResultDeltas},
    actors::CoreLifecycleCommandBackend,
    runtime::{CancellationToken, TokioTaskFailure, TokioTaskGroup},
};
use caly_backends::dual::DualCoreLifecycle;

use super::{CompositionError, task_failure, task_name};

/// Initial backoff before the first auto-restart after a crash.
const CRASH_RESTART_BACKOFF_START_MS: u64 = 1_000;
/// Maximum backoff between automatic restarts (crash-loop protection).
const CRASH_RESTART_BACKOFF_MAX_MS: u64 = 30_000;
/// A successful restart must stay up this long before crash-loop backoff resets.
const STABILIZE_WINDOW_MS: u64 = 5_000;
/// Poll interval when no crash or pending restart exists.
const POLL_INTERVAL_MS: u64 = 500;

/// Pure crash-loop backoff and stabilization policy.
///
/// The backoff only resets after the core stays running for a full
/// `STABILIZE_WINDOW_MS`; a successful restart alone never resets it, and a
/// failed restart keeps growing it so a crash-loop is not masked by retries.
#[derive(Debug)]
pub(super) struct CrashLoopPolicy {
    initial_backoff_ms: u64,
    max_backoff_ms: u64,
    backoff_ms: u64,
    consecutive_crashes: u64,
    stable_ms: u64,
}

impl CrashLoopPolicy {
    pub(super) const fn new() -> Self {
        Self::with_bounds(CRASH_RESTART_BACKOFF_START_MS, CRASH_RESTART_BACKOFF_MAX_MS)
    }

    /// Configurable bounds: the first backoff and the exponential ceiling.
    pub(super) const fn with_bounds(initial_backoff_ms: u64, max_backoff_ms: u64) -> Self {
        Self {
            initial_backoff_ms,
            max_backoff_ms,
            backoff_ms: initial_backoff_ms,
            consecutive_crashes: 0,
            stable_ms: 0,
        }
    }

    /// Backoff to wait before the next restart attempt.
    pub(super) const fn backoff_ms(&self) -> u64 {
        self.backoff_ms
    }

    /// Records a crash; returns the backoff before the next restart attempt.
    pub(super) fn on_crash(&mut self) -> u64 {
        self.consecutive_crashes = self.consecutive_crashes.saturating_add(1);
        self.stable_ms = 0;
        if self.consecutive_crashes > 1 {
            self.grow();
        }
        self.backoff_ms
    }

    /// Records a failed restart; grows the backoff for the retry.
    pub(super) fn on_restart_failed(&mut self) -> u64 {
        self.stable_ms = 0;
        self.grow()
    }

    /// Advances the stabilization window after a successful restart. Returns
    /// true when the window elapsed and the crash loop was reset.
    pub(super) fn on_stable_tick(&mut self, elapsed_ms: u64) -> bool {
        if self.stable_ms >= STABILIZE_WINDOW_MS {
            return false;
        }
        self.stable_ms = self.stable_ms.saturating_add(elapsed_ms);
        if self.stable_ms >= STABILIZE_WINDOW_MS {
            *self = Self::with_bounds(self.initial_backoff_ms, self.max_backoff_ms);
            return true;
        }
        false
    }

    fn grow(&mut self) -> u64 {
        self.backoff_ms = self.backoff_ms.saturating_mul(2).min(self.max_backoff_ms);
        self.backoff_ms
    }
}

impl Default for CrashLoopPolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawns the core-exit monitor that restarts a crashed core with backoff.
pub(super) fn spawn_exit_monitor(
    tasks: &mut TokioTaskGroup,
    lifecycle: caly_backends::dual::DualCoreLifecycle,
    results: ActorResultClient,
    cancellation: CancellationToken,
    initial_backoff_ms: u64,
    max_backoff_ms: u64,
) -> Result<(), CompositionError> {
    tasks
        .spawn_owned_with_fault(
            task_name("core-exit-monitor")?,
            caly_application::runtime::FatalFault::InfrastructureRecovery,
            async move {
                let mut policy = CrashLoopPolicy::with_bounds(initial_backoff_ms, max_backoff_ms);
                let mut observed = caly_domain::ObservedState::default();
                let mut restart_count: u64 = 0;
                let mut pending_restart = false;
                while !cancellation.is_cancelled() {
                    if pending_restart {
                        let retry = restart_step(
                            &lifecycle,
                            &results,
                            &mut policy,
                            &mut observed,
                            &mut restart_count,
                            &cancellation,
                        )
                        .await?;
                        pending_restart = retry;
                        continue;
                    }
                    if let Some(state) = poll_crash(&lifecycle)? {
                        report_crash(&results, state, observed)?;
                        policy.on_crash();
                        pending_restart = true;
                        continue;
                    }
                    if policy.on_stable_tick(POLL_INTERVAL_MS) {
                        observed = observe_restart(observed, restart_count, 0);
                        report_stable(&results, observed)?;
                    }
                    tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
                }
                Ok(())
            },
        )
        .map_err(|_| CompositionError::TaskCapacity)
}

/// Waits the current backoff, attempts a restart, and reports the outcome.
/// Returns `true` when the restart failed and another retry is pending.
async fn restart_step(
    lifecycle: &DualCoreLifecycle,
    results: &ActorResultClient,
    policy: &mut CrashLoopPolicy,
    observed: &mut caly_domain::ObservedState,
    restart_count: &mut u64,
    cancellation: &CancellationToken,
) -> Result<bool, TokioTaskFailure> {
    if !cancellation.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(policy.backoff_ms())).await;
    }
    if cancellation.is_cancelled() {
        return Ok(false);
    }
    if let Some(running) = try_restart(lifecycle) {
        *restart_count = restart_count.saturating_add(1);
        *observed = observe_restart(*observed, *restart_count, policy.backoff_ms());
        report_recovered(results, running, *observed)?;
        Ok(false)
    } else {
        // Keep retrying with a growing backoff instead of waiting for a new
        // exit event (a failed restart consumes the process tree).
        policy.on_restart_failed();
        Ok(true)
    }
}

/// Polls the shared lifecycle for an abnormal exit.
fn poll_crash(
    lifecycle: &DualCoreLifecycle,
) -> Result<Option<caly_domain::AppliedState>, TokioTaskFailure> {
    lifecycle
        .poll_abnormal_exit()
        .map_err(|_| task_failure("core-exit-monitor", "core abnormal-exit polling failed"))
}

/// Attempts to restart the core; returns the running state on success.
fn try_restart(lifecycle: &DualCoreLifecycle) -> Option<caly_domain::AppliedState> {
    let mut lifecycle = lifecycle.clone();
    CoreLifecycleCommandBackend::restart(&mut lifecycle).ok()
}

/// Publishes a projection-only crash observation.
fn report_crash(
    results: &ActorResultClient,
    state: caly_domain::AppliedState,
    observed: caly_domain::ObservedState,
) -> Result<(), TokioTaskFailure> {
    report_observation(
        results,
        |deltas| caly_application::actor_result::ActorReport::Crashed { deltas },
        state,
        observed,
    )
}

/// Publishes a projection-only recovery (auto-restart) observation.
fn report_recovered(
    results: &ActorResultClient,
    state: caly_domain::AppliedState,
    observed: caly_domain::ObservedState,
) -> Result<(), TokioTaskFailure> {
    report_observation(
        results,
        |deltas| caly_application::actor_result::ActorReport::Recovered { deltas },
        state,
        observed,
    )
}

/// Publishes a stabilization observation clearing the active backoff.
fn report_stable(
    results: &ActorResultClient,
    observed: caly_domain::ObservedState,
) -> Result<(), TokioTaskFailure> {
    let deltas =
        ResultDeltas::try_from_vec(vec![caly_domain::PresentationDelta::ObservedReplaced(
            observed,
        )])
        .map_err(|_| task_failure("core-exit-monitor", "core stable delta overflow"))?;
    results
        .try_report(caly_application::actor_result::ActorReport::Recovered { deltas })
        .map_err(|_| task_failure("core-exit-monitor", "core stable result mailbox failed"))
}

/// Builds `AppliedReplaced` + `ObservedReplaced` deltas and submits them.
fn report_observation(
    results: &ActorResultClient,
    kind: impl FnOnce(ResultDeltas) -> caly_application::actor_result::ActorReport,
    state: caly_domain::AppliedState,
    observed: caly_domain::ObservedState,
) -> Result<(), TokioTaskFailure> {
    let deltas = ResultDeltas::try_from_vec(vec![
        caly_domain::PresentationDelta::ObservedReplaced(observed),
        caly_domain::PresentationDelta::AppliedReplaced(state),
    ])
    .map_err(|_| task_failure("core-exit-monitor", "core crash delta overflow"))?;
    results
        .try_report(kind(deltas))
        .map_err(|_| task_failure("core-exit-monitor", "core crash result mailbox failed"))
}

/// Returns `observed` with the self-heal counters updated after a restart.
fn observe_restart(
    observed: caly_domain::ObservedState,
    restart_count: u64,
    backoff_ms: u64,
) -> caly_domain::ObservedState {
    caly_domain::ObservedState::with_self_heal(
        observed.upload_bytes_per_second(),
        observed.download_bytes_per_second(),
        observed.active_connections(),
        observed.telemetry_dropped(),
        restart_count,
        backoff_ms,
    )
}

#[cfg(test)]
mod recovery_tests;
