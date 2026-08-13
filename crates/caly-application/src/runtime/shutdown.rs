//! Ordered daemon shutdown phases.

/// Mandatory shutdown order; phases cannot be skipped by the driver.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ShutdownPhase {
    RejectTransportMutations,
    CloseCommandIngress,
    StopTelemetry,
    StopAndReapCore,
    RestorePlatform,
    StopSubscriptions,
    FinishConfigTransactions,
    FlushEventSequencer,
    StopProjector,
    ReleaseTransportAndLock,
    Completed,
}

impl ShutdownPhase {
    /// Returns the only legal next phase.
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::RejectTransportMutations => Some(Self::CloseCommandIngress),
            Self::CloseCommandIngress => Some(Self::StopTelemetry),
            Self::StopTelemetry => Some(Self::StopAndReapCore),
            Self::StopAndReapCore => Some(Self::RestorePlatform),
            Self::RestorePlatform => Some(Self::StopSubscriptions),
            Self::StopSubscriptions => Some(Self::FinishConfigTransactions),
            Self::FinishConfigTransactions => Some(Self::FlushEventSequencer),
            Self::FlushEventSequencer => Some(Self::StopProjector),
            Self::StopProjector => Some(Self::ReleaseTransportAndLock),
            Self::ReleaseTransportAndLock => Some(Self::Completed),
            Self::Completed => None,
        }
    }
}

/// Driver state preventing out-of-order phase completion.
pub struct ShutdownDriver {
    current: ShutdownPhase,
}

impl ShutdownDriver {
    pub const fn new() -> Self {
        Self {
            current: ShutdownPhase::RejectTransportMutations,
        }
    }
    pub const fn current(&self) -> ShutdownPhase {
        self.current
    }

    pub fn complete_current(&mut self) -> Result<ShutdownPhase, ShutdownComplete> {
        let next = self.current.next().ok_or(ShutdownComplete)?;
        self.current = next;
        Ok(next)
    }
}

impl Default for ShutdownDriver {
    fn default() -> Self {
        Self::new()
    }
}

/// Indicates shutdown was already complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShutdownComplete;
