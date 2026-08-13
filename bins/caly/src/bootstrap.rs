//! Composition-root startup ordering.

use caly_composition::{ApplicationComposition, CompositionError, RuntimeCapacities};
use caly_domain::DaemonInstanceId;

/// Required daemon assembly order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapPhase {
    AcquireInstanceLock,
    RestorePendingEffects,
    BuildOwnedRuntime,
    BindTransport,
    Ready,
}

impl BootstrapPhase {
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::AcquireInstanceLock => Some(Self::RestorePendingEffects),
            Self::RestorePendingEffects => Some(Self::BuildOwnedRuntime),
            Self::BuildOwnedRuntime => Some(Self::BindTransport),
            Self::BindTransport => Some(Self::Ready),
            Self::Ready => None,
        }
    }
}

/// Runtime/application owners assembled before transport binding.
pub struct DaemonAssembly {
    pub bootstrap: BootstrapDriver,
    pub application: ApplicationComposition,
}

impl DaemonAssembly {
    pub fn new(
        daemon_instance: DaemonInstanceId,
        core_override: Option<caly_domain::CoreKind>,
        tun: Option<caly_domain::TunConfig>,
        controllers: caly_domain::Controllers,
        binaries: caly_composition::CoreBinaryPaths,
        subscription_urls: Vec<String>,
        tuning: caly_composition::RuntimeTuning,
    ) -> Result<Self, CompositionError> {
        let telemetry_interval_ms = crate::daemon_config::telemetry_interval_ms();
        Ok(Self {
            bootstrap: BootstrapDriver::new(),
            application: ApplicationComposition::new_with_binaries(
                daemon_instance,
                RuntimeCapacities::default(),
                core_override,
                tun,
                controllers,
                binaries,
                subscription_urls,
                telemetry_interval_ms,
                tuning,
            )?,
        })
    }
}

/// Driver preventing transport/core startup before restore-first succeeds.
pub struct BootstrapDriver {
    phase: BootstrapPhase,
}

impl BootstrapDriver {
    pub const fn new() -> Self {
        Self {
            phase: BootstrapPhase::AcquireInstanceLock,
        }
    }
    pub const fn phase(&self) -> BootstrapPhase {
        self.phase
    }

    pub fn complete(
        &mut self,
        completed: BootstrapPhase,
    ) -> Result<BootstrapPhase, BootstrapError> {
        if completed != self.phase {
            return Err(BootstrapError::OutOfOrder);
        }
        let next = self.phase.next().ok_or(BootstrapError::AlreadyReady)?;
        self.phase = next;
        Ok(next)
    }
}

impl Default for BootstrapDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapError {
    OutOfOrder,
    AlreadyReady,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_walks_the_required_assembly_order() {
        // The driver (not a flag struct — the pre-#76
        // `DaemonResources` bool set was removed because no
        // production code consumed it and it duplicated this
        // gate) is the single source of startup ordering.
        let mut driver = BootstrapDriver::new();
        for phase in [
            BootstrapPhase::AcquireInstanceLock,
            BootstrapPhase::RestorePendingEffects,
            BootstrapPhase::BuildOwnedRuntime,
            BootstrapPhase::BindTransport,
        ] {
            driver.complete(phase).unwrap();
        }
        assert_eq!(driver.phase(), BootstrapPhase::Ready);
        // Past Ready: replaying an earlier phase is out of
        // order; completing Ready again hits AlreadyReady.
        assert_eq!(
            driver.complete(BootstrapPhase::BindTransport),
            Err(BootstrapError::OutOfOrder)
        );
        assert_eq!(
            driver.complete(BootstrapPhase::Ready),
            Err(BootstrapError::AlreadyReady)
        );
    }

    #[test]
    fn runtime_cannot_precede_restore() {
        let mut driver = BootstrapDriver::new();
        assert_eq!(
            driver.complete(BootstrapPhase::BuildOwnedRuntime),
            Err(BootstrapError::OutOfOrder),
        );
        assert_eq!(driver.phase(), BootstrapPhase::AcquireInstanceLock);
    }
}
