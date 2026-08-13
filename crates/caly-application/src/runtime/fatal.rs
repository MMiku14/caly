//! First-cause fatal runtime guard and shutdown trigger.

use super::{CancellationToken, ShutdownDriver, ShutdownPhase};

/// Stable fatal category retained without leaking concrete error payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FatalFault {
    CommandDispatch,
    ActorResult,
    Projection,
    TaskJoin,
    InfrastructureRecovery,
}

/// Runtime health transition result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FatalRecordOutcome {
    FirstFault,
    AdditionalFault { total: u32 },
}

/// Owns root cancellation and ordered shutdown progress.
pub struct RuntimeGuard {
    cancellation: CancellationToken,
    shutdown: ShutdownDriver,
    first_fault: Option<FatalFault>,
    fault_count: u32,
    fatal_signal: tokio::sync::watch::Sender<Option<FatalFault>>,
}

impl RuntimeGuard {
    pub fn new() -> Self {
        let (fatal_signal, _) = tokio::sync::watch::channel(None);
        Self {
            cancellation: CancellationToken::new(),
            shutdown: ShutdownDriver::new(),
            first_fault: None,
            fault_count: 0,
            fatal_signal,
        }
    }

    /// Records first cause, counts later faults, and always requests cancellation.
    /// Every record is logged — a fatal that ends the daemon must never be
    /// silent (W3a 排查: a refresh-time fatal exited the daemon with zero
    /// log output before this call logged anything).
    /// Records an actor-result application failure as a runtime fatal.
    /// Restored after an over-eager regex deleted it along with
    /// `complete_shutdown_phase` (2026-08-12 dead-code audit).
    pub fn record_actor_result_error(&mut self, error: &impl std::fmt::Debug) {
        tracing::error!(
            ?error,
            "actor result application failed; recording runtime fatal"
        );
        self.record_fatal(FatalFault::ActorResult);
    }

    /// Records a command dispatch failure as a runtime fatal.
    pub fn record_dispatch_error(&mut self, error: &impl std::fmt::Debug) {
        tracing::error!(?error, "command dispatch failed; recording runtime fatal");
        self.record_fatal(FatalFault::CommandDispatch);
    }

    pub fn record_fatal(&mut self, fault: FatalFault) -> FatalRecordOutcome {
        self.cancellation.cancel();
        self.fault_count = self.fault_count.saturating_add(1);
        if self.first_fault.is_none() {
            tracing::error!(?fault, "runtime fatal; daemon will shut down");
            self.first_fault = Some(fault);
            // `send_replace` retains the first fault even when the transport
            // has not subscribed yet, so a late subscriber still exits.
            self.fatal_signal.send_replace(Some(fault));
            FatalRecordOutcome::FirstFault
        } else {
            FatalRecordOutcome::AdditionalFault {
                total: self.fault_count,
            }
        }
    }

    pub const fn first_fault(&self) -> Option<FatalFault> {
        self.first_fault
    }
    pub const fn fault_count(&self) -> u32 {
        self.fault_count
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
    /// Subscribes to the first fatal fault. The initial value is retained, so
    /// callers cannot miss a failure that happened before subscription.
    pub fn fatal_receiver(&self) -> tokio::sync::watch::Receiver<Option<FatalFault>> {
        self.fatal_signal.subscribe()
    }
    pub const fn shutdown_phase(&self) -> ShutdownPhase {
        self.shutdown.current()
    }
}

#[test]
fn first_fault_is_retained_and_cancellation_requested() {
    let mut guard = RuntimeGuard::new();
    assert_eq!(
        guard.record_fatal(FatalFault::Projection),
        FatalRecordOutcome::FirstFault
    );
    assert_eq!(
        guard.record_fatal(FatalFault::TaskJoin),
        FatalRecordOutcome::AdditionalFault { total: 2 },
    );
    assert_eq!(guard.first_fault(), Some(FatalFault::Projection));
    assert!(guard.cancellation().is_cancelled());
}

impl Default for RuntimeGuard {
    fn default() -> Self {
        Self::new()
    }
}
